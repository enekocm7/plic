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
        Arc, OnceLock,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    thread,
    time::Duration,
};

use anyhow::{Context, Result, bail};

use super::{
    CommandExt as _, FrameSize, Transform,
    graph::{self, AudioAdjust, AudioPlan},
    seconds_arg,
};
use crate::trim::TrimRange;

static NEXT_TEMP_ID: AtomicU64 = AtomicU64::new(0);
static ENCODER: OnceLock<&'static Encoder> = OnceLock::new();

struct Encoder {
    args: &'static [&'static str],
}

// Hardware encoders first; software x264 is the fallback and always works.
const ENCODERS: [Encoder; 4] = [
    Encoder {
        args: &[
            "-c:v", "h264_amf", "-quality", "balanced", "-rc", "cqp", "-qp_i", "20", "-qp_p", "20",
        ],
    },
    Encoder {
        args: &[
            "-c:v",
            "h264_nvenc",
            "-preset",
            "p4",
            "-rc",
            "vbr",
            "-cq",
            "20",
            "-b:v",
            "0",
        ],
    },
    Encoder {
        args: &[
            "-c:v",
            "h264_qsv",
            "-preset",
            "fast",
            "-global_quality",
            "20",
        ],
    },
    Encoder {
        args: &["-c:v", "libx264", "-preset", "fast", "-crf", "20"],
    },
];

/// Picks the first encoder that can actually encode a test clip on this machine.
fn encoder() -> &'static Encoder {
    ENCODER.get_or_init(|| {
        ENCODERS
            .iter()
            .find(|candidate| works(candidate))
            .unwrap_or(&ENCODERS[ENCODERS.len() - 1])
    })
}

fn works(encoder: &Encoder) -> bool {
    Command::new("ffmpeg")
        .args(["-hide_banner", "-loglevel", "error", "-f", "lavfi", "-i"])
        .arg("color=c=black:s=640x360:r=30:d=0.5,format=yuv420p")
        .args(encoder.args)
        .args(["-f", "null", "-"])
        .no_window()
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

/// Probes the encoders on a background thread so the first export does not
/// pay for it.
pub fn warm_up_encoder() {
    thread::spawn(encoder);
}

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
    if request.mode == ExportMode::StreamCopy && !request.transform.is_identity() {
        return Err(anyhow::anyhow!(
            "cropping, rotating and flipping need re-encoding; turn off Lossless"
        )
        .into());
    }
    if request.transform.crop.is_some() && request.video_size.is_none() {
        return Err(anyhow::anyhow!("the video size is unknown, so it cannot be cropped").into());
    }
    let video_filter = request.transform.filter(request.video_size);
    let ranges = effective_ranges(request)?;
    let temporary = create_temp_path(output, "mp4")?;
    // Removes the temporary file on every early return below.
    let guard = TempFile(&temporary);

    let mut command = Command::new("ffmpeg");
    command.args(["-hide_banner", "-loglevel", "error", "-nostats", "-y"]);
    let audio_filter = request.audio_adjust.filter();
    let audio = AudioPlan {
        tracks: &request.audio_tracks,
        chain: audio_filter.as_deref(),
    };
    // Kept alive until ffmpeg is done with it.
    let mut _concat_list = None;
    match request.mode {
        ExportMode::Reencode => {
            // One seeked input per range, joined by the concat filter: every
            // cut stays frame-accurate and the joins are seamless.
            for range in &ranges {
                add_input(&mut command, request, *range);
            }
            command
                .arg("-filter_complex")
                .arg(graph::reencode(
                    ranges.len(),
                    video_filter.as_deref(),
                    audio,
                ))
                .args(["-map", "[v]"])
                .args(encoder().args);
            if !audio.is_silent() {
                command.args(["-map", "[a]", "-c:a", "aac", "-b:a", "192k"]);
            }
        }
        ExportMode::StreamCopy => {
            if let [range] = ranges.as_slice() {
                add_input(&mut command, request, *range);
            } else {
                let list = create_temp_path(output, "txt")?;
                fs::write(&list, concat_list(&request.source, &ranges))
                    .with_context(|| format!("could not write {}", list.display()))?;
                command
                    .args(["-f", "concat", "-safe", "0", "-i"])
                    .arg(&list);
                _concat_list = Some(OwnedTempFile(list));
            }
            command.args(["-map", "0:v:0", "-c:v", "copy"]);
            if let Some(audio_graph) = graph::copy_audio(audio) {
                // Mixed or adjusted sound has to be encoded again.
                command
                    .arg("-filter_complex")
                    .arg(audio_graph)
                    .args(["-map", "[a]", "-c:a", "aac", "-b:a", "192k"]);
            } else if let [track] = audio.tracks {
                command
                    .arg("-map")
                    .arg(format!("0:a:{track}"))
                    .args(["-c:a", "copy"]);
            }
        }
    }
    command
        .args(["-movflags", "+faststart"])
        .args(["-progress", "pipe:1"])
        .arg(&temporary)
        .no_window()
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let mut child = command
        .spawn()
        .context("could not start ffmpeg; check that it is installed and on PATH")?;
    let stderr = drain_in_background(child.stderr.take());

    let expected = ranges
        .iter()
        .map(|range| range.length(request.source_duration))
        .sum::<Option<Duration>>();
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

    on_progress(1.0);
    publish(&temporary, output)?;
    // The published file is a hard link or a copy; the temporary goes either way.
    drop(guard);
    Ok(output.clone())
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
    fn reads_progress_lines() {
        assert_eq!(
            parse_progress_line("out_time_us=1500000"),
            Some(Duration::from_millis(1500))
        );
        assert_eq!(parse_progress_line("out_time_us=N/A"), None);
        assert_eq!(parse_progress_line("frame=12"), None);
    }
}
