//! Exporting trimmed ranges of a clip with ffmpeg, either re-encoded for a
//! frame-accurate cut or stream-copied for a fast, lossless one. Several
//! ranges are joined, in order, into one file.
//!
//! Output is written to a hidden temporary file next to the destination and
//! only published once ffmpeg succeeds, so a crash or cancellation never
//! leaves a truncated video behind under the requested name.

use std::{
    fs::{self, OpenOptions},
    io::{self, BufRead, BufReader, Read},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    thread,
    time::Duration,
};

use anyhow::{Context, Result, bail};

use super::{
    Animation, CommandExt as _, Format, FrameSize, OutputLimits, Transform,
    encoder::{self, Codec, VideoEncoding},
    graph::{self, AudioAdjust, AudioPlan},
    seconds_arg, transform,
};
use crate::{scheduler::Workload, trim::TrimRange};

static NEXT_TEMP_ID: AtomicU64 = AtomicU64::new(0);
/// How the trimmed range is written.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ExportMode {
    /// Decode and encode again: cuts land on the exact frame.
    #[default]
    Reencode,
    /// Copy the compressed streams: near-instant and lossless, but the start
    /// moves back to the keyframe at or before the requested one.
    StreamCopy,
}

/// Everything needed to produce one trimmed output file.
#[derive(Debug, Clone)]
pub struct ExportRequest {
    pub source: PathBuf,
    pub output: PathBuf,
    /// The parts of the source to keep, joined in this order. Never empty.
    pub ranges: Vec<TrimRange>,
    pub mode: ExportMode,
    /// Codec and quality of a re-encoded video.
    pub video: VideoEncoding,
    /// Length of the source, used to report progress for open-ended ranges.
    pub source_duration: Option<Duration>,
    /// Audio streams (`0:a:N`) to keep, mixed into one when there are
    /// several. Empty drops the sound.
    pub audio_tracks: Vec<usize>,
    /// Gain and loudness normalization for the exported sound.
    pub audio_adjust: AudioAdjust,
    /// Display size of the source, needed to crop it.
    pub video_size: Option<FrameSize>,
    pub transform: Transform,
    /// Caps on the output's size and frame rate.
    pub limits: OutputLimits,
    /// Frame rate of the source, so a cap never adds frames.
    pub frame_rate: Option<f64>,
    /// MP4, or an animated GIF or WebP made with `animation`.
    pub format: Format,
    pub animation: Animation,
}

/// Lets the UI cancel a running export.
#[derive(Debug, Clone, Default)]
pub struct CancelToken(Arc<AtomicBool>);

impl ExportRequest {
    /// What running this export keeps busy.
    pub fn workload(&self) -> Workload {
        if self.format.is_animation() || self.video.target_size.is_some() {
            // Animations and size targets always encode in software.
            return Workload::Software;
        }
        match self.mode {
            ExportMode::StreamCopy => Workload::Copy,
            // Not probed yet: assume the scarcer hardware sessions.
            ExportMode::Reencode => match super::uses_hardware(self.video.codec) {
                Some(false) => Workload::Software,
                _ => Workload::Hardware,
            },
        }
    }
}

impl CancelToken {
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Relaxed);
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }
}

/// Why an export did not produce a file.
#[derive(Debug)]
pub enum ExportError {
    Cancelled,
    Failed(anyhow::Error),
}

impl std::fmt::Display for ExportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Cancelled => f.write_str("export was cancelled"),
            Self::Failed(error) => write!(f, "{error:#}"),
        }
    }
}

impl From<anyhow::Error> for ExportError {
    fn from(error: anyhow::Error) -> Self {
        Self::Failed(error)
    }
}

/// The source and the options an export is made with, for [`estimated_size`].
#[derive(Debug, Clone, Copy, Default)]
pub struct SizeInputs {
    /// Size of the source file, in bytes.
    pub source_bytes: u64,
    /// Length of the whole source.
    pub source_duration: Duration,
    /// Length of what is kept.
    pub length: Duration,
    pub mode: ExportMode,
    /// Codec and quality of a re-encoded export.
    pub video: VideoEncoding,
    pub transform: Transform,
    /// Caps on the output's size and frame rate.
    pub limits: OutputLimits,
    /// Display size of the source, with its rotation already applied.
    pub source_size: Option<FrameSize>,
    /// Frame rate of the source.
    pub source_frame_rate: Option<f64>,
    /// Codec of the source, assumed to be H.264 when unknown.
    pub source_codec: Option<Codec>,
    /// Every audio stream of the source.
    pub source_audio_tracks: usize,
    /// Audio streams kept in the export, mixed into one.
    pub audio_tracks: usize,
    /// Whether copied sound still has to be encoded, as mixing several
    /// tracks or adjusting the level does.
    pub audio_reencoded: bool,
}

/// Roughly how large an export will be.
///
/// Exports are encoded at constant quality, so the source's average bitrate
/// is the starting point. The picture's share of it is scaled by how many
/// pixels and frames the output keeps, and by how much better the chosen
/// codec and quality compress; the sound is counted apart, since it does not
/// shrink with the picture. A stream copy keeps the source's picture as it
/// is, so only its sound choices matter.
pub fn estimated_size(inputs: &SizeInputs) -> u64 {
    if inputs.source_duration.is_zero() {
        return 0;
    }
    let fraction =
        (inputs.length.as_secs_f64() / inputs.source_duration.as_secs_f64()).clamp(0.0, 1.0);
    let kept = inputs.source_bytes as f64 * fraction;
    let seconds = inputs.length.as_secs_f64();
    // A source track is assumed to weigh what one exported AAC track does.
    let track_bytes = DEFAULT_AUDIO_BITRATE as f64 / 8.0 * seconds;
    let source_audio = (inputs.source_audio_tracks as f64 * track_bytes).min(kept);
    let video = (kept - source_audio).max(0.0);

    let audio = if inputs.audio_tracks == 0 {
        0.0
    } else if inputs.mode == ExportMode::StreamCopy && !inputs.audio_reencoded {
        // Copied tracks keep the bitrate they had in the source.
        source_audio * inputs.audio_tracks as f64 / inputs.source_audio_tracks.max(1) as f64
    } else {
        // Encoded sound is mixed into one track at a known bitrate.
        track_bytes
    };

    let video = match inputs.mode {
        // A stream copy leaves the picture untouched.
        ExportMode::StreamCopy => video,
        ExportMode::Reencode => {
            let source_codec = inputs.source_codec.unwrap_or(Codec::H264);
            video
                * pixel_factor(inputs)
                * frame_rate_factor(inputs)
                * (inputs.video.codec.efficiency() / source_codec.efficiency())
                * inputs.video.quality.bitrate_factor(inputs.video.codec)
        }
    };
    (video + audio).round() as u64
}

/// The share of the source's pixels the output keeps, after cropping,
/// rotating and downscaling.
fn pixel_factor(inputs: &SizeInputs) -> f64 {
    let Some(source) = inputs.source_size else {
        return 1.0;
    };
    let source_pixels = f64::from(source.width) * f64::from(source.height);
    if source_pixels <= 0.0 {
        return 1.0;
    }
    let output = inputs.transform.output_size(source);
    let output = inputs.limits.scaled_size(output).unwrap_or(output);
    let output_pixels = f64::from(output.width) * f64::from(output.height);
    (output_pixels / source_pixels).clamp(0.0, 1.0)
}

/// The share of the source's frames the output keeps. A cap never adds
/// frames, and an unknown rate is taken to be the usual 30 fps.
fn frame_rate_factor(inputs: &SizeInputs) -> f64 {
    let source = inputs.source_frame_rate.unwrap_or(30.0);
    if source <= 0.0 {
        return 1.0;
    }
    let output = inputs
        .limits
        .max_fps
        .map_or(source, |cap| source.min(f64::from(cap)));
    (output / source).clamp(0.0, 1.0)
}

/// Runs ffmpeg for `request`, calling `on_progress` with a fraction in
/// `0.0..=1.0` as encoding advances. Blocks until the export finishes.
pub fn export(
    request: &ExportRequest,
    cancel: &CancelToken,
    mut on_progress: impl FnMut(f32),
) -> Result<PathBuf, ExportError> {
    let output = &request.output;
    if output.exists() {
        return Err(anyhow::anyhow!("{} already exists", output.display()).into());
    }
    if request.mode == ExportMode::StreamCopy
        && (!request.transform.is_identity() || !request.limits.is_none())
    {
        return Err(anyhow::anyhow!(
            "cropping, rotating, flipping and downscaling need re-encoding; turn off Lossless"
        )
        .into());
    }
    if request.transform.crop.is_some() && request.video_size.is_none() {
        return Err(anyhow::anyhow!("the video size is unknown, so it cannot be cropped").into());
    }
    let ranges = effective_ranges(request)?;
    let expected = ranges
        .iter()
        .map(|range| range.length(request.source_duration))
        .sum::<Option<Duration>>();
    let temporary = create_temp_path(output, request.format.extension())?;
    // Removes the temporary file on every early return below.
    let guard = TempFile(&temporary);

    let audio_filter = request.audio_adjust.filter();
    let audio = AudioPlan {
        tracks: &request.audio_tracks,
        chain: audio_filter.as_deref(),
    };
    let video_filter = transform::video_filter(
        &request.transform,
        &request.limits,
        request.video_size,
        request.frame_rate,
    );
    let parts = Parts {
        request,
        ranges: &ranges,
        audio,
        video_filter: video_filter.as_deref(),
    };

    if request.format.is_animation() {
        // Silent, shrunk and slowed down; the size limits do not apply.
        let mut chain: Vec<String> = request
            .transform
            .filter(request.video_size)
            .into_iter()
            .collect();
        chain.push(request.animation.chain(request.format));
        let chain = chain.join(",");
        let parts = Parts {
            audio: AudioPlan {
                tracks: &[],
                chain: None,
            },
            video_filter: Some(&chain),
            ..parts
        };
        let args: Vec<String> = request
            .format
            .encoder_args()
            .iter()
            .map(|arg| arg.to_string())
            .collect();
        let command = parts.reencode(&args, &[], None, 0);
        run(
            command,
            Output::File(&temporary),
            cancel,
            expected,
            &mut on_progress,
        )?;
        on_progress(1.0);
        publish(&temporary, output)?;
        drop(guard);
        return Ok(output.clone());
    }

    match request.mode {
        ExportMode::Reencode => {
            let rates = match request.video.target_size {
                Some(target) => {
                    let length = expected.context(
                        "the length of the export is unknown, so it cannot aim for a size",
                    )?;
                    Some(target_bitrates(target, length, !audio.is_silent())?)
                }
                None => None,
            };
            let plan = encoder::plan(request.video, rates.map(|rates| rates.video));
            let audio_bitrate = rates.map_or(DEFAULT_AUDIO_BITRATE, |rates| rates.audio);
            if plan.two_pass {
                // The first pass only gathers statistics for the second.
                let log = create_temp_path(output, "log")?;
                let _log = PassLog(log.clone());
                let first = parts.reencode(
                    &plan.args,
                    &["-pass", "1", "-passlogfile"],
                    Some(&log),
                    audio_bitrate,
                );
                run(first, NULL_OUTPUT, cancel, expected, |fraction| {
                    on_progress(fraction * 0.5)
                })?;
                let second = parts.reencode(
                    &plan.args,
                    &["-pass", "2", "-passlogfile"],
                    Some(&log),
                    audio_bitrate,
                );
                run(
                    second,
                    Output::File(&temporary),
                    cancel,
                    expected,
                    |fraction| on_progress(0.5 + fraction * 0.5),
                )?;
            } else {
                let command = parts.reencode(&plan.args, &[], None, audio_bitrate);
                run(
                    command,
                    Output::File(&temporary),
                    cancel,
                    expected,
                    &mut on_progress,
                )?;
            }
        }
        ExportMode::StreamCopy => {
            // Kept alive until ffmpeg is done with them.
            let mut _segments = None;
            let mut _concat_list = None;
            let list = if ranges.len() > 1 {
                let segments = cut_segments(request, &ranges, cancel, &mut on_progress)?;
                let list = create_temp_path(output, "txt")?;
                fs::write(&list, concat_list(&segments.0))
                    .with_context(|| format!("could not write {}", list.display()))?;
                _concat_list = Some(OwnedTempFile(list.clone()));
                _segments = Some(segments);
                Some(list)
            } else {
                None
            };
            // A join reads the intermediate files, which keep only the chosen
            // audio tracks, numbered from zero. A single range reads the
            // source itself, where the original track numbers apply.
            let copy_tracks: Vec<usize> = (0..request.audio_tracks.len()).collect();
            let copy_parts = Parts {
                request,
                ranges: &ranges,
                audio: AudioPlan {
                    tracks: &copy_tracks,
                    chain: audio_filter.as_deref(),
                },
                video_filter: video_filter.as_deref(),
            };
            let parts = if list.is_some() { &copy_parts } else { &parts };
            let start = if list.is_some() { CUT_SHARE } else { 0.0 };
            run(
                parts.stream_copy(list.as_deref()),
                Output::File(&temporary),
                cancel,
                expected,
                |fraction| on_progress(start + (1.0 - start) * fraction),
            )?;
        }
    }

    on_progress(1.0);
    publish(&temporary, output)?;
    // The published file is a hard link or a copy; the temporary goes either way.
    drop(guard);
    Ok(output.clone())
}

/// Audio bitrate of re-encoded sound when no target size squeezes it.
const DEFAULT_AUDIO_BITRATE: u64 = 192_000;

/// The share of a lossless join's progress spent cutting its segments;
/// joining them takes the rest.
const CUT_SHARE: f32 = 0.1;

/// The share of a target size left for the streams; the rest covers the
/// container and the encoder overshooting a little.
const TARGET_HEADROOM: f64 = 0.95;

/// The lowest video bitrate worth encoding at.
const MIN_VIDEO_BITRATE: u64 = 100_000;

/// Bitrates, in bits per second, that make an export land on a size.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TargetRates {
    pub video: u64,
    pub audio: u64,
}

/// Splits `target_bytes` over `length` between video and audio.
pub fn target_bitrates(
    target_bytes: u64,
    length: Duration,
    with_audio: bool,
) -> Result<TargetRates> {
    let headroom = TARGET_HEADROOM;
    let seconds = length.as_secs_f64().max(0.1);
    let total = (target_bytes as f64 * 8.0 * headroom / seconds) as u64;
    // Sound gets up to 128 kbps, but never more than a sixth of the budget.
    let audio = if with_audio {
        (total / 6).clamp(32_000, 128_000)
    } else {
        0
    };
    let video = total.saturating_sub(audio);
    if video < MIN_VIDEO_BITRATE {
        let needed = (MIN_VIDEO_BITRATE + audio) as f64 * seconds / 8.0 / headroom;
        bail!(
            "{:.1} MB is too small for {:.0} seconds; it needs at least {:.1} MB",
            target_bytes as f64 / MB,
            seconds,
            needed / MB
        );
    }
    Ok(TargetRates { video, audio })
}

const MB: f64 = 1024.0 * 1024.0;

/// Where ffmpeg writes.
#[derive(Clone, Copy)]
enum Output<'a> {
    File(&'a Path),
    /// Discarded, for a first pass.
    Null,
}

const NULL_OUTPUT: Output<'static> = Output::Null;

/// What every ffmpeg command of one export shares.
struct Parts<'a> {
    request: &'a ExportRequest,
    ranges: &'a [TrimRange],
    audio: AudioPlan<'a>,
    video_filter: Option<&'a str>,
}

impl Parts<'_> {
    /// A re-encode: one seeked input per range, joined by the concat
    /// filter, so every cut stays frame-accurate and the joins seamless.
    /// `pass_args` and `pass_log` add two-pass flags.
    fn reencode(
        &self,
        video_args: &[String],
        pass_args: &[&str],
        pass_log: Option<&Path>,
        audio_bitrate: u64,
    ) -> Command {
        let mut command = base_command();
        for range in self.ranges {
            add_input(&mut command, self.request, *range);
        }
        command
            .arg("-filter_complex")
            .arg(graph::reencode(
                self.ranges.len(),
                self.video_filter,
                self.audio,
            ))
            .args(["-map", "[v]"])
            .args(video_args)
            .args(pass_args);
        if let Some(log) = pass_log {
            command.arg(log);
        }
        if !self.audio.is_silent() {
            command
                .args(["-map", "[a]", "-c:a", "aac", "-b:a"])
                .arg(audio_bitrate.to_string());
        }
        command
    }

    /// A stream copy of one range, or of several pre-cut segments through
    /// the concat demuxer script at `list`.
    fn stream_copy(&self, list: Option<&Path>) -> Command {
        let mut command = base_command();
        match (list, self.ranges) {
            (Some(list), _) => {
                command.args(["-f", "concat", "-safe", "0", "-i"]).arg(list);
            }
            (None, [range, ..]) => add_input(&mut command, self.request, *range),
            (None, []) => {}
        }
        command.args(["-map", "0:v:0", "-c:v", "copy"]);
        if let Some(audio_graph) = graph::copy_audio(self.audio) {
            // Mixed or adjusted sound has to be encoded again.
            command
                .arg("-filter_complex")
                .arg(audio_graph)
                .args(["-map", "[a]", "-c:a", "aac", "-b:a"])
                .arg(DEFAULT_AUDIO_BITRATE.to_string());
        } else if let [track] = self.audio.tracks {
            command
                .arg("-map")
                .arg(format!("0:a:{track}"))
                .args(["-c:a", "copy"]);
        }
        command
    }
}

fn base_command() -> Command {
    let mut command = crate::ffmpeg::command(crate::ffmpeg::Tool::Ffmpeg);
    command.args(["-hide_banner", "-loglevel", "error", "-nostats", "-y"]);
    command
}

/// Runs one ffmpeg command to completion, reporting its progress through
/// `expected` as a fraction.
fn run(
    mut command: Command,
    output: Output,
    cancel: &CancelToken,
    expected: Option<Duration>,
    mut on_progress: impl FnMut(f32),
) -> Result<(), ExportError> {
    match output {
        Output::File(path) => {
            if path.extension().is_some_and(|extension| extension == "mp4") {
                command.args(["-movflags", "+faststart"]);
            }
            command.args(["-progress", "pipe:1"]).arg(path);
        }
        Output::Null => {
            command.args(["-progress", "pipe:1", "-f", "null", "-"]);
        }
    }
    command
        .no_window()
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let mut child = command.spawn().context(
        "could not start ffmpeg; wait for its download to finish or set its folder in the settings",
    )?;
    let stderr = drain_in_background(child.stderr.take());

    if let Some(stdout) = child.stdout.take() {
        for line in BufReader::new(stdout).lines() {
            if cancel.is_cancelled() {
                kill(&mut child);
                return Err(ExportError::Cancelled);
            }
            let Ok(line) = line else { break };
            if let (Some(expected), Some(done)) = (expected, parse_progress_line(&line)) {
                let fraction = done.as_secs_f64() / expected.as_secs_f64().max(f64::EPSILON);
                on_progress(fraction.clamp(0.0, 1.0) as f32);
            }
        }
    }

    let status = child.wait().context("ffmpeg did not finish")?;
    if cancel.is_cancelled() {
        return Err(ExportError::Cancelled);
    }
    if !status.success() {
        let stderr = stderr.join().unwrap_or_default();
        let stderr = stderr.trim();
        if stderr.is_empty() {
            return Err(anyhow::anyhow!("ffmpeg exited with status {status}").into());
        }
        return Err(anyhow::anyhow!("ffmpeg exited with status {status}: {stderr}").into());
    }
    Ok(())
}

/// The statistics files of a two-pass encode, named after a reserved
/// temporary path, removed when dropped.
struct PassLog(PathBuf);

impl Drop for PassLog {
    fn drop(&mut self) {
        let base = self.0.to_string_lossy().into_owned();
        for suffix in [
            "",
            "-0.log",
            "-0.log.mbtree",
            "-0.log.cutree",
            "-0.log.temp",
        ] {
            let path = PathBuf::from(format!("{base}{suffix}"));
            if path.exists() {
                remove_temporary(&path);
            }
        }
    }
}

/// The ranges that will actually be written. A stream copy can only start
/// on a keyframe, so each start is moved back to the one at or before the
/// requested start; starting there explicitly keeps every player in
/// agreement about where the clip begins.
fn effective_ranges(request: &ExportRequest) -> Result<Vec<TrimRange>> {
    if request.ranges.is_empty() {
        bail!("nothing to export");
    }
    match request.mode {
        ExportMode::Reencode => Ok(request.ranges.clone()),
        ExportMode::StreamCopy => request
            .ranges
            .iter()
            .map(|range| {
                let start = super::keyframe_at_or_before(&request.source, range.start)
                    .context("could not find a keyframe to start the lossless cut on")?;
                Ok(TrimRange::new(start, range.end))
            })
            .collect(),
    }
}

/// Adds `request.source` as an input limited to `range`.
fn add_input(command: &mut Command, request: &ExportRequest, range: TrimRange) {
    if !range.start.is_zero() {
        command.arg("-ss").arg(seconds_arg(range.start));
    }
    if let Some(length) = range.end.map(|end| end.saturating_sub(range.start)) {
        command.arg("-t").arg(seconds_arg(length));
    }
    if request.mode == ExportMode::Reencode {
        command.args(super::hwaccel_args());
    }
    command.arg("-i").arg(&request.source);
}

/// Stream-copies one range into a file of its own. Cutting it separately
/// lets ffmpeg normalize the segment's timestamps: its first picture and
/// sound both start at zero, including the reordering delay of B-frames.
/// The concat demuxer can then join these files from their durations,
/// without any packets straddling a join.
///
/// Asking it to cut the same source several times with `inpoint`/`outpoint`
/// instead leaves pre-roll packets at every join. Their timestamps go
/// backwards (a B-frame's DTS leads its PTS), and the muxer repairs the
/// non-monotonic DTS by nudging the picture and sound apart by a frame or
/// more, which is the drift this avoids.
fn segment_command(request: &ExportRequest, range: TrimRange) -> Command {
    let mut command = base_command();
    add_input(&mut command, request, range);
    command.args(["-map", "0:v:0"]);
    // Only the tracks the export keeps, in the same order: the concat
    // demuxer numbers them from zero on its input.
    for &track in &request.audio_tracks {
        command.arg("-map").arg(format!("0:a:{track}"));
    }
    command.args(["-c", "copy", "-avoid_negative_ts", "make_zero"]);
    command
}

/// Cuts the pieces of a lossless join, kept alive until the join is done.
fn cut_segments(
    request: &ExportRequest,
    ranges: &[TrimRange],
    cancel: &CancelToken,
    on_progress: &mut impl FnMut(f32),
) -> Result<Segments, ExportError> {
    // Reserved up front so an early failure still removes them all.
    let mut segments = Segments(Vec::with_capacity(ranges.len()));
    for _ in ranges {
        segments.0.push(create_temp_path(
            &request.output,
            request.format.extension(),
        )?);
    }
    for (index, (&range, path)) in ranges.iter().zip(&segments.0).enumerate() {
        if cancel.is_cancelled() {
            return Err(ExportError::Cancelled);
        }
        run(
            segment_command(request, range),
            Output::File(path),
            cancel,
            range.length(request.source_duration),
            |fraction| {
                on_progress(CUT_SHARE * (index as f32 + fraction) / ranges.len() as f32);
            },
        )?;
    }
    Ok(segments)
}

/// A concat demuxer script that plays `segments` back to back. Their own
/// durations place them on the timeline, so a `duration` line would only
/// guess at what the file already knows and put the joins out of sync.
fn concat_list(segments: &[PathBuf]) -> String {
    // Paths are single-quoted; a quote inside one is closed, escaped and
    // reopened.
    let mut list = String::from("ffconcat version 1.0\n");
    for segment in segments {
        let path = segment.to_string_lossy().replace('\'', r"'\''");
        list.push_str(&format!("file '{path}'\n"));
    }
    list
}

/// Reads `out_time_us=…` lines from ffmpeg's `-progress` output.
fn parse_progress_line(line: &str) -> Option<Duration> {
    let micros = line
        .strip_prefix("out_time_us=")?
        .trim()
        .parse::<u64>()
        .ok()?;
    Some(Duration::from_micros(micros))
}

fn kill(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
}

fn drain_in_background(pipe: Option<impl Read + Send + 'static>) -> thread::JoinHandle<String> {
    thread::spawn(move || {
        let mut text = String::new();
        if let Some(mut pipe) = pipe {
            let _ = pipe.read_to_string(&mut text);
        }
        text
    })
}

struct TempFile<'a>(&'a Path);

impl Drop for TempFile<'_> {
    fn drop(&mut self) {
        remove_temporary(self.0);
    }
}

/// A temporary file that is removed when dropped.
struct OwnedTempFile(PathBuf);

impl Drop for OwnedTempFile {
    fn drop(&mut self) {
        remove_temporary(&self.0);
    }
}

/// The intermediate files of a lossless join, removed when dropped.
struct Segments(Vec<PathBuf>);

impl Drop for Segments {
    fn drop(&mut self) {
        for path in &self.0 {
            remove_temporary(path);
        }
    }
}

fn remove_temporary(path: &Path) {
    // A killed ffmpeg can hold the file for a moment longer, especially
    // behind a launcher shim, and Windows will not delete an open file.
    for _ in 0..20 {
        match fs::remove_file(path) {
            Err(error) if error.kind() != io::ErrorKind::NotFound => {
                thread::sleep(Duration::from_millis(100));
            }
            _ => return,
        }
    }
}

fn create_temp_path(output: &Path, extension: &str) -> Result<PathBuf> {
    let parent = output.parent().unwrap_or_else(|| Path::new("."));
    let stem = output
        .file_stem()
        .map(|stem| stem.to_string_lossy())
        .unwrap_or_default();
    for _ in 0..100 {
        let id = NEXT_TEMP_ID.fetch_add(1, Ordering::Relaxed);
        let candidate = parent.join(format!(
            ".{stem}.plic-{}-{id}.{extension}",
            std::process::id()
        ));
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&candidate)
        {
            Ok(_) => return Ok(candidate),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => {
                return Err(error).with_context(|| {
                    format!("could not create temporary output in {}", parent.display())
                });
            }
        }
    }
    bail!(
        "could not reserve a temporary output name in {}",
        parent.display()
    );
}

/// Moves the finished temporary file to `output` without ever overwriting an
/// existing file.
fn publish(temporary: &Path, output: &Path) -> Result<()> {
    match fs::hard_link(temporary, output) {
        Ok(()) => return Ok(()),
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            bail!("{} already exists; choose another name", output.display());
        }
        // Hard links are unsupported on some file systems; fall back to a copy.
        Err(_) => {}
    }

    let mut destination = match OpenOptions::new().write(true).create_new(true).open(output) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            bail!("{} already exists; choose another name", output.display());
        }
        Err(error) => {
            return Err(error).with_context(|| format!("could not create {}", output.display()));
        }
    };

    let copied = (|| -> Result<()> {
        let mut source = fs::File::open(temporary)
            .with_context(|| format!("could not open {}", temporary.display()))?;
        io::copy(&mut source, &mut destination)
            .with_context(|| format!("could not copy video to {}", output.display()))?;
        destination
            .sync_all()
            .with_context(|| format!("could not finish writing {}", output.display()))
    })();
    drop(destination);

    if copied.is_err() {
        let _ = fs::remove_file(output);
    }
    copied
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ffmpeg::CropAspect;

    /// A minute of 1080p60 H.264 with no sound, exported unchanged.
    fn inputs() -> SizeInputs {
        SizeInputs {
            source_bytes: 60 * MB_U64,
            source_duration: Duration::from_secs(60),
            length: Duration::from_secs(60),
            mode: ExportMode::Reencode,
            video: VideoEncoding::default(),
            transform: Transform::identity(),
            limits: OutputLimits::default(),
            source_size: Some(FrameSize::new(1920, 1080)),
            source_frame_rate: Some(60.0),
            source_codec: Some(Codec::H264),
            source_audio_tracks: 0,
            audio_tracks: 0,
            audio_reencoded: false,
        }
    }

    const MB_U64: u64 = 1024 * 1024;

    #[test]
    fn estimates_size_from_the_source_bitrate() {
        let minute = Duration::from_secs(60);
        let full = estimated_size(&inputs());
        assert_eq!(full, 60 * MB_U64);
        assert_eq!(
            estimated_size(&SizeInputs {
                length: Duration::from_secs(15),
                ..inputs()
            }),
            15 * MB_U64
        );
        assert_eq!(
            estimated_size(&SizeInputs {
                source_duration: Duration::ZERO,
                ..inputs()
            }),
            0
        );
        // Keeping more than the source has still only keeps the source.
        assert_eq!(
            estimated_size(&SizeInputs {
                length: minute * 2,
                ..inputs()
            }),
            full
        );
    }

    #[test]
    fn downscaling_shrinks_the_estimate_by_the_pixels_kept() {
        let full = estimated_size(&inputs());
        let downscaled = SizeInputs {
            limits: OutputLimits {
                max_lines: Some(720),
                max_fps: None,
            },
            ..inputs()
        };
        // 720p keeps 1280x720 of the source's 1920x1080 pixels.
        assert_eq!(
            estimated_size(&downscaled),
            (full as f64 * (1280.0 * 720.0) / (1920.0 * 1080.0)).round() as u64
        );
        assert!(estimated_size(&downscaled) < full);
        // Cropping to a vertical slice of the same height keeps fewer pixels too.
        let cropped = SizeInputs {
            transform: Transform {
                crop: Some(CropAspect::Tall),
                ..Transform::identity()
            },
            ..inputs()
        };
        assert!(estimated_size(&cropped) < full);
    }

    #[test]
    fn capping_the_frame_rate_shrinks_the_estimate() {
        let full = estimated_size(&inputs());
        let capped = SizeInputs {
            limits: OutputLimits {
                max_lines: None,
                max_fps: Some(30),
            },
            ..inputs()
        };
        assert_eq!(estimated_size(&capped), full / 2);
        // A cap above the source's own rate changes nothing.
        let no_op = SizeInputs {
            limits: OutputLimits {
                max_lines: None,
                max_fps: Some(60),
            },
            ..inputs()
        };
        assert_eq!(estimated_size(&no_op), full);
    }

    #[test]
    fn more_efficient_codecs_shrink_the_estimate() {
        let full = estimated_size(&inputs());
        let hevc = SizeInputs {
            video: VideoEncoding {
                codec: Codec::Hevc,
                ..inputs().video
            },
            ..inputs()
        };
        let av1 = SizeInputs {
            video: VideoEncoding {
                codec: Codec::Av1,
                ..inputs().video
            },
            ..inputs()
        };
        assert_eq!(estimated_size(&hevc), (full as f64 * 0.65).round() as u64);
        assert!(estimated_size(&av1) < estimated_size(&hevc));
        // An H.264 source in the same codec is not expected to shrink.
        let already_hevc = SizeInputs {
            source_codec: Some(Codec::Hevc),
            ..hevc
        };
        assert_eq!(estimated_size(&already_hevc), full);
    }

    #[test]
    fn the_quality_preset_changes_the_estimate() {
        let full = estimated_size(&inputs());
        let small = SizeInputs {
            video: VideoEncoding {
                quality: encoder::Quality::Small,
                ..inputs().video
            },
            ..inputs()
        };
        let high = SizeInputs {
            video: VideoEncoding {
                quality: encoder::Quality::High,
                ..inputs().video
            },
            ..inputs()
        };
        assert!(estimated_size(&small) < full);
        assert!(estimated_size(&high) > full);
    }

    #[test]
    fn sound_counts_apart_from_the_picture() {
        // One track is re-encoded at the same bitrate it is assumed to have.
        let with_sound = SizeInputs {
            source_audio_tracks: 1,
            audio_tracks: 1,
            ..inputs()
        };
        assert_eq!(estimated_size(&with_sound), 60 * MB_U64);
        // Dropping it removes its share.
        let silent = SizeInputs {
            audio_tracks: 0,
            ..with_sound
        };
        assert!(estimated_size(&silent) < estimated_size(&with_sound));
        // Mixing two tracks into one costs a single track worth of bits.
        let two_tracks = SizeInputs {
            source_audio_tracks: 2,
            audio_tracks: 2,
            audio_reencoded: true,
            ..inputs()
        };
        assert!(estimated_size(&two_tracks) < 60 * MB_U64);
    }

    #[test]
    fn a_stream_copy_keeps_the_sources_picture() {
        let copy = SizeInputs {
            mode: ExportMode::StreamCopy,
            source_audio_tracks: 2,
            audio_tracks: 2,
            ..inputs()
        };
        assert_eq!(estimated_size(&copy), 60 * MB_U64);
        // Size and codec options do not apply to a copy.
        let ignored = SizeInputs {
            limits: OutputLimits {
                max_lines: Some(480),
                max_fps: Some(30),
            },
            video: VideoEncoding {
                codec: Codec::Av1,
                ..inputs().video
            },
            ..copy
        };
        assert_eq!(estimated_size(&ignored), estimated_size(&copy));
        // Half the sound tracks taken out is half the sound.
        let one_track = SizeInputs {
            audio_tracks: 1,
            ..copy
        };
        assert!(estimated_size(&one_track) < 60 * MB_U64);
    }

    #[test]
    fn writes_a_concat_list_of_segments_with_quoted_paths() {
        let list = concat_list(&[
            PathBuf::from("C:/clips/it's.mp4"),
            PathBuf::from("C:/clips/part 2.mp4"),
        ]);
        assert_eq!(
            list,
            "ffconcat version 1.0\n\
             file 'C:/clips/it'\\''s.mp4'\n\
             file 'C:/clips/part 2.mp4'\n"
        );
        // The files' own durations place them on the timeline.
        assert!(!list.contains("duration"));
        assert!(!list.contains("inpoint"));
        assert!(!list.contains("outpoint"));
    }

    #[test]
    fn a_segment_is_cut_with_its_own_timestamps() {
        let request = ExportRequest {
            source: PathBuf::from("clip.mkv"),
            output: PathBuf::from("out.mp4"),
            ranges: vec![TrimRange::new(
                Duration::from_secs(2),
                Some(Duration::from_secs(7)),
            )],
            mode: ExportMode::StreamCopy,
            video: VideoEncoding::default(),
            source_duration: None,
            audio_tracks: vec![0, 2],
            audio_adjust: AudioAdjust::default(),
            video_size: None,
            transform: Transform::default(),
            limits: OutputLimits::default(),
            frame_rate: None,
            format: Format::Mp4,
            animation: Animation::default(),
        };
        let args: Vec<String> = segment_command(&request, request.ranges[0])
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            args,
            [
                "-hide_banner",
                "-loglevel",
                "error",
                "-nostats",
                "-y",
                "-ss",
                "2.000",
                "-t",
                "5.000",
                "-i",
                "clip.mkv",
                "-map",
                "0:v:0",
                "-map",
                "0:a:0",
                "-map",
                "0:a:2",
                "-c",
                "copy",
                "-avoid_negative_ts",
                "make_zero",
            ]
        );
    }

    #[test]
    fn splits_a_target_size_between_video_and_audio() {
        let mb = 1024 * 1024;
        let minute = Duration::from_secs(60);
        let rates = target_bitrates(25 * mb, minute, true).unwrap();
        // 25 MB over a minute is about 3.3 Mbps of streams after headroom.
        assert_eq!(rates.audio, 128_000);
        assert_eq!(rates.video + rates.audio, 3_320_490);
        let silent = target_bitrates(25 * mb, minute, false).unwrap();
        assert_eq!(silent.audio, 0);
        assert_eq!(silent.video, 3_320_490);
        // A tiny budget for a long clip is refused with the size it needs.
        let error = target_bitrates(mb, Duration::from_secs(600), true)
            .unwrap_err()
            .to_string();
        assert!(error.contains("needs at least"), "{error}");
    }

    /// Needs ffmpeg on PATH: `cargo test -- --ignored`.
    #[test]
    #[ignore]
    fn lands_under_a_target_size() {
        let dir = std::env::temp_dir().join(format!("plic-target-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let source = dir.join("source.mp4");
        let made = crate::ffmpeg::command(crate::ffmpeg::Tool::Ffmpeg)
            .args([
                "-hide_banner",
                "-loglevel",
                "error",
                "-y",
                "-f",
                "lavfi",
                "-i",
            ])
            .arg("testsrc2=s=1280x720:r=30:d=12")
            .args([
                "-f",
                "lavfi",
                "-i",
                "sine=d=12",
                "-c:v",
                "libx264",
                "-crf",
                "10",
            ])
            .args(["-c:a", "aac", "-shortest"])
            .arg(&source)
            .status()
            .unwrap();
        assert!(made.success());

        let target = 1024 * 1024;
        let request = ExportRequest {
            source: source.clone(),
            output: dir.join("out.mp4"),
            ranges: vec![TrimRange::new(
                Duration::from_secs(1),
                Some(Duration::from_secs(11)),
            )],
            mode: ExportMode::Reencode,
            video: VideoEncoding {
                target_size: Some(target),
                ..VideoEncoding::default()
            },
            source_duration: Some(Duration::from_secs(12)),
            audio_tracks: vec![0],
            audio_adjust: AudioAdjust::default(),
            video_size: Some(FrameSize::new(1280, 720)),
            transform: Transform::default(),
            limits: OutputLimits::default(),
            frame_rate: Some(30.0),
            format: Format::Mp4,
            animation: Animation::default(),
        };
        let output = export(&request, &CancelToken::default(), |_| {}).unwrap();
        let size = fs::metadata(&output).unwrap().len();
        let leftovers = fs::read_dir(&dir).unwrap().count();
        fs::remove_dir_all(&dir).ok();
        assert!(size <= target, "{size} bytes is over the target");
        assert!(size > target / 2, "{size} bytes is far under the target");
        // Only the source and the export remain: no pass logs or temporaries.
        assert_eq!(leftovers, 2);
    }

    /// Needs ffmpeg on PATH: `cargo test -- --ignored`.
    #[test]
    #[ignore]
    fn exports_animated_gif_and_webp() {
        let dir = std::env::temp_dir().join(format!("plic-anim-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let source = dir.join("source.mp4");
        let made = crate::ffmpeg::command(crate::ffmpeg::Tool::Ffmpeg)
            .args([
                "-hide_banner",
                "-loglevel",
                "error",
                "-y",
                "-f",
                "lavfi",
                "-i",
            ])
            .arg("testsrc2=s=1280x720:r=60:d=4")
            .args([
                "-f", "lavfi", "-i", "sine=d=4", "-c:v", "libx264", "-c:a", "aac",
            ])
            .args(["-shortest"])
            .arg(&source)
            .status()
            .unwrap();
        assert!(made.success());

        for (format, magic) in [(Format::Gif, &b"GIF89a"[..]), (Format::Webp, &b"RIFF"[..])] {
            let request = ExportRequest {
                source: source.clone(),
                output: dir.join(format!("out.{}", format.extension())),
                ranges: vec![TrimRange::new(
                    Duration::from_secs(1),
                    Some(Duration::from_secs(3)),
                )],
                mode: ExportMode::Reencode,
                video: VideoEncoding::default(),
                source_duration: Some(Duration::from_secs(4)),
                audio_tracks: vec![0],
                audio_adjust: AudioAdjust::default(),
                video_size: Some(FrameSize::new(1280, 720)),
                transform: Transform::default(),
                limits: OutputLimits::default(),
                frame_rate: Some(60.0),
                format,
                animation: Animation::default(),
            };
            let output = export(&request, &CancelToken::default(), |_| {}).unwrap();
            let bytes = fs::read(&output).unwrap();
            assert!(bytes.starts_with(magic), "{format:?} has the wrong header");
        }
        let leftovers = fs::read_dir(&dir).unwrap().count();
        fs::remove_dir_all(&dir).ok();
        assert_eq!(leftovers, 3);
    }

    #[test]
    fn reads_progress_lines() {
        assert_eq!(
            parse_progress_line("out_time_us=1500000"),
            Some(Duration::from_millis(1500))
        );
        assert_eq!(parse_progress_line("out_time_us=N/A"), None);
        assert_eq!(parse_progress_line("frame=12"), None);
    }
}
