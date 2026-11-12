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
    CommandExt as _, FrameSize, OutputLimits, Transform,
    encoder::{self, VideoEncoding},
    graph::{self, AudioAdjust, AudioPlan},
    seconds_arg, transform,
};
use crate::trim::TrimRange;

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
}

/// Lets the UI cancel a running export.
#[derive(Debug, Clone, Default)]
pub struct CancelToken(Arc<AtomicBool>);

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

/// Roughly how large an export of `length` from a source of `source_bytes`
/// and `source_duration` will be. Exports are encoded at constant quality,
/// so the source's average bitrate is the best cheap guess.
pub fn estimated_size(source_bytes: u64, source_duration: Duration, length: Duration) -> u64 {
    if source_duration.is_zero() {
        return 0;
    }
    let fraction = (length.as_secs_f64() / source_duration.as_secs_f64()).clamp(0.0, 1.0);
    (source_bytes as f64 * fraction).round() as u64
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
    let temporary = create_temp_path(output, "mp4")?;
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
            // Kept alive until ffmpeg is done with it.
            let mut _concat_list = None;
            let list = if ranges.len() > 1 {
                let list = create_temp_path(output, "txt")?;
                fs::write(&list, concat_list(&request.source, &ranges))
                    .with_context(|| format!("could not write {}", list.display()))?;
                _concat_list = Some(OwnedTempFile(list.clone()));
                Some(list)
            } else {
                None
            };
            let command = parts.stream_copy(list.as_deref());
            run(
                command,
                Output::File(&temporary),
                cancel,
                expected,
                &mut on_progress,
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

    /// A stream copy of one range, or of several through the concat
    /// demuxer script at `list`.
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
    let mut command = Command::new("ffmpeg");
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
            command.args(["-movflags", "+faststart"]);
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

    let mut child = command
        .spawn()
        .context("could not start ffmpeg; check that it is installed and on PATH")?;
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
        command.args(["-hwaccel", "auto"]);
    }
    command.arg("-i").arg(&request.source);
}

/// A concat demuxer script that plays `ranges` of `source` back to back.
fn concat_list(source: &Path, ranges: &[TrimRange]) -> String {
    // Paths are single-quoted; a quote inside one is closed, escaped and
    // reopened.
    let path = source.to_string_lossy().replace('\'', r"'\''");
    let mut list = String::from("ffconcat version 1.0\n");
    for range in ranges {
        list.push_str(&format!("file '{path}'\n"));
        if !range.start.is_zero() {
            list.push_str(&format!("inpoint {}\n", seconds_arg(range.start)));
        }
        if let Some(end) = range.end {
            list.push_str(&format!("outpoint {}\n", seconds_arg(end)));
        }
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

    #[test]
    fn estimates_size_from_the_source_bitrate() {
        let mb = 1024 * 1024;
        let minute = Duration::from_secs(60);
        assert_eq!(
            estimated_size(60 * mb, minute, Duration::from_secs(15)),
            15 * mb
        );
        assert_eq!(estimated_size(60 * mb, minute, minute * 2), 60 * mb);
        assert_eq!(estimated_size(60 * mb, Duration::ZERO, minute), 0);
    }

    #[test]
    fn writes_a_concat_list_with_quoted_paths() {
        let s = Duration::from_secs;
        let list = concat_list(
            Path::new("C:/clips/it's.mp4"),
            &[
                TrimRange::new(s(0), Some(s(5))),
                TrimRange::new(s(10), None),
            ],
        );
        assert_eq!(
            list,
            "ffconcat version 1.0\n\
             file 'C:/clips/it'\\''s.mp4'\noutpoint 5.000\n\
             file 'C:/clips/it'\\''s.mp4'\ninpoint 10.000\n"
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
        let made = Command::new("ffmpeg")
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
