//! Decoding video frames to raw BGRA pixels, the format GPUI uploads as-is.

use std::{
    io::{BufRead as _, BufReader, Read},
    path::{Path, PathBuf},
    process::{Child, ChildStdout, Command, Stdio},
    sync::mpsc::{self, Receiver, SyncSender, TryRecvError},
    thread,
    time::Duration,
};

use anyhow::{Context, Result, bail};

use super::{CommandExt as _, Timing, seconds_arg};

/// Pixel dimensions of a frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FrameSize {
    pub width: u32,
    pub height: u32,
}

impl FrameSize {
    pub const fn new(width: u32, height: u32) -> Self {
        Self { width, height }
    }

    pub fn aspect_ratio(self) -> f32 {
        self.width as f32 / self.height.max(1) as f32
    }

    /// The largest size with this aspect ratio that fits in `bounds` without
    /// upscaling. Dimensions are even, as most pixel formats require.
    pub fn fit_within(self, bounds: FrameSize) -> FrameSize {
        let scale = (bounds.width as f64 / self.width as f64)
            .min(bounds.height as f64 / self.height as f64)
            .min(1.0);
        let even = |value: f64| ((value / 2.0).floor() as u32 * 2).max(2);
        FrameSize::new(
            even(self.width as f64 * scale),
            even(self.height as f64 * scale),
        )
    }

    fn byte_len(self) -> usize {
        self.width as usize * self.height as usize * 4
    }
}

/// One decoded frame in BGRA order.
#[derive(Clone)]
pub struct RawFrame {
    pub size: FrameSize,
    /// Presentation time relative to the start of the clip.
    pub pts: Duration,
    pub bgra: Vec<u8>,
}

/// Decodes the exact frame shown at `at`, scaled to `size`.
///
/// `hardware` decodes on the GPU when `at` is past the start, where every
/// frame since the previous keyframe has to be decoded first; see
/// [`MediaInfo::prefers_hardware_decode`](super::MediaInfo).
pub fn extract_frame(
    path: &Path,
    at: Duration,
    size: FrameSize,
    hardware: bool,
) -> Result<RawFrame> {
    let mut command = base_command();
    if hardware && !at.is_zero() {
        command.args(super::hwaccel_args());
    }
    if at.is_zero() {
        // Only the first picture is needed, so spreading it over every core
        // costs more in start-up than it saves; this is the frame shown
        // whenever a clip is opened.
        command.args(["-threads", "1"]);
    } else {
        command.arg("-ss").arg(seconds_arg(at));
    }
    command.arg("-i").arg(path);
    run_single_frame(command, size, at, "bicubic")
}

/// Decodes a frame near `at`, normally the keyframe before it. Faster than
/// [`extract_frame`] because frames between keyframes are skipped (a quarter
/// faster on 1440p video, a third on 4K); good enough for thumbnails and for
/// previewing a drag along the timeline.
pub fn extract_keyframe(path: &Path, at: Duration, size: FrameSize) -> Result<RawFrame> {
    let fast_seek = |skip_to_keyframes: bool| {
        let mut command = base_command();
        if skip_to_keyframes {
            command.args(["-skip_frame", "nokey"]);
        }
        command
            .arg("-noaccurate_seek")
            .arg("-ss")
            .arg(seconds_arg(at))
            .arg("-i")
            .arg(path);
        run_single_frame(command, size, at, "bilinear")
    };
    // Past the last keyframe, skipping non-key frames leaves nothing to
    // output; decoding normally from that keyframe still works.
    fast_seek(true).or_else(|_| fast_seek(false))
}

/// An image file format for saved frames.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageFormat {
    Png,
    Jpeg,
}

impl ImageFormat {
    pub fn extension(self) -> &'static str {
        match self {
            Self::Png => "png",
            Self::Jpeg => "jpg",
        }
    }
}

/// Writes the frame shown at `at` to `output` at full resolution. Fails
/// rather than overwrite an existing file.
pub fn save_frame(path: &Path, at: Duration, output: &Path, format: ImageFormat) -> Result<()> {
    let mut command = base_command();
    command
        .arg("-ss")
        .arg(seconds_arg(at))
        .arg("-i")
        .arg(path)
        .args(["-an", "-sn", "-frames:v", "1", "-update", "1", "-n"]);
    if format == ImageFormat::Jpeg {
        // Near the top of ffmpeg's 2 (best) to 31 (worst) JPEG scale.
        command.args(["-q:v", "2"]);
    }
    let result = command
        .arg(output)
        .no_window()
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .context("could not start ffmpeg; wait for its download to finish or set its folder in the settings")?;
    if !result.status.success() || !output.exists() {
        let stderr = String::from_utf8_lossy(&result.stderr);
        bail!("could not save the frame: {}", stderr.trim());
    }
    Ok(())
}

/// `clip-01m12s345`: a file name for the frame of `stem` at `at`.
pub fn frame_file_stem(stem: &str, at: Duration) -> String {
    let millis = at.as_millis();
    let (minutes, seconds, millis) = (millis / 60_000, millis / 1000 % 60, millis % 1000);
    format!("{stem}-{minutes:02}m{seconds:02}s{millis:03}")
}

fn base_command() -> Command {
    let mut command = crate::ffmpeg::command(crate::ffmpeg::Tool::Ffmpeg);
    command.args(["-hide_banner", "-loglevel", "error", "-nostdin"]);
    command
}

fn run_single_frame(
    mut command: Command,
    size: FrameSize,
    at: Duration,
    scaler: &str,
) -> Result<RawFrame> {
    let output = command
        .args(["-an", "-sn", "-frames:v", "1"])
        .arg("-vf")
        .arg(scale_filter(size, scaler))
        .args(["-pix_fmt", "bgra", "-f", "rawvideo", "pipe:1"])
        .no_window()
        .stdin(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .context("could not start ffmpeg; wait for its download to finish or set its folder in the settings")?;
    if output.stdout.len() < size.byte_len() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("no frame at {}: {}", seconds_arg(at), stderr.trim());
    }
    let mut bgra = output.stdout;
    bgra.truncate(size.byte_len());
    Ok(RawFrame {
        size,
        pts: at,
        bgra,
    })
}

/// Decodes every keyframe of `path` in one pass, scaled to `size`, and hands
/// each to `on_frame` as soon as it is ready, in order. Stops early, killing
/// ffmpeg, when `on_frame` returns false.
///
/// Frames between keyframes are skipped without being decoded, so a whole
/// clip takes about as long as two or three single-frame decodes: half a
/// second for three minutes of 1440p AV1 with a keyframe every four seconds.
pub fn keyframes(
    path: &Path,
    size: FrameSize,
    on_frame: impl FnMut(RawFrame) -> bool,
) -> Result<()> {
    let mut command = crate::ffmpeg::command(crate::ffmpeg::Tool::Ffmpeg);
    command.args(["-skip_frame", "nokey"]).arg("-i").arg(path);
    preview_frames(command, scale_filter(size, "bilinear"), size, on_frame)
}

/// Decodes the whole of `path` and hands over one frame every `interval`,
/// scaled to `size`, like [`keyframes`]. Slower, as every frame is decoded,
/// but not limited to where the keyframes happen to be: a short clip may
/// have only one every few seconds.
pub fn sampled_frames(
    path: &Path,
    size: FrameSize,
    interval: Duration,
    hardware: bool,
    on_frame: impl FnMut(RawFrame) -> bool,
) -> Result<()> {
    let mut command = crate::ffmpeg::command(crate::ffmpeg::Tool::Ffmpeg);
    if hardware {
        command.args(super::hwaccel_args());
    }
    // Deblocking makes no visible difference at thumbnail size, and
    // skipping it decodes a fifth faster.
    command
        .args(["-skip_loop_filter", "all"])
        .arg("-i")
        .arg(path);
    let rate = 1.0 / interval.as_secs_f64().max(0.001);
    let filter = format!("fps={rate:.6},{}", scale_filter(size, "bilinear"));
    preview_frames(command, filter, size, on_frame)
}

/// Runs `command`, an ffmpeg with its input set, through `filter` and
/// hands over each frame with its time.
fn preview_frames(
    mut command: Command,
    filter: String,
    size: FrameSize,
    mut on_frame: impl FnMut(RawFrame) -> bool,
) -> Result<()> {
    // rawvideo carries no timestamps; `showinfo` prints each frame's on
    // stderr, in the same order the pictures arrive on stdout.
    let mut child = command
        .args(["-hide_banner", "-loglevel", "info", "-nostats", "-nostdin"])
        .args(["-map", "0:v:0", "-an", "-sn", "-dn", "-vf"])
        .arg(format!("{filter},showinfo"))
        .args(["-fps_mode", "passthrough"])
        .args(["-pix_fmt", "bgra", "-f", "rawvideo", "pipe:1"])
        .no_window()
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("could not start ffmpeg; wait for its download to finish or set its folder in the settings")?;
    let mut stdout = child.stdout.take().context("ffmpeg has no stdout")?;
    let stderr = child.stderr.take().context("ffmpeg has no stderr")?;

    let (times, pts) = mpsc::channel();
    thread::Builder::new()
        .name("keyframe times".into())
        .spawn(move || {
            for line in BufReader::new(stderr).lines() {
                let Ok(line) = line else { return };
                if let Some(time) = showinfo_time(&line)
                    && times.send(time).is_err()
                {
                    return;
                }
            }
        })
        .context("could not start the keyframe reader")?;

    let mut result = Ok(());
    loop {
        let mut bgra = vec![0; size.byte_len()];
        if stdout.read_exact(&mut bgra).is_err() {
            break;
        }
        let Ok(pts) = pts.recv() else {
            result = Err(anyhow::anyhow!("ffmpeg gave a frame without its time"));
            break;
        };
        if !on_frame(RawFrame { size, pts, bgra }) {
            break;
        }
    }
    let _ = child.kill();
    let _ = child.wait();
    result
}

/// The time of a frame from one of `showinfo`'s lines, which look like
/// `[Parsed_showinfo_1 @ 0x…] n:   3 pts: 375000 pts_time:12.5 duration: …`.
fn showinfo_time(line: &str) -> Option<Duration> {
    if !line.contains("showinfo") {
        return None;
    }
    let seconds: f64 = line
        .split_once("pts_time:")?
        .1
        .split_whitespace()
        .next()?
        .parse()
        .ok()?;
    (seconds.is_finite() && seconds >= 0.0).then(|| Duration::from_secs_f64(seconds))
}

fn scale_filter(size: FrameSize, scaler: &str) -> String {
    format!("scale={}:{}:flags={scaler}", size.width, size.height)
}

/// Frames decoded ahead of the playhead. Small, because every frame is a full
/// uncompressed image; the bound also pauses ffmpeg when playback pauses.
const QUEUE_DEPTH: usize = 6;

/// A running ffmpeg process decoding frames from `start` onwards.
///
/// Frames are delivered at a constant `frame_rate`, so frame `n` is shown at
/// `first + n / frame_rate`, where `first` is the time of the first frame the
/// source has at or after `start`. The frames carry the clip's own
/// timestamps: ffmpeg keeps the container's clock (`-copyts`) and is told
/// where the output grid starts, so a clip whose video starts late, or
/// whose frames do not fall on whole milliseconds, still lines up with the
/// audio decoded for the same time. Dropping the stream stops ffmpeg.
pub struct VideoStream {
    child: Option<Child>,
    frames: Receiver<RawFrame>,
    size: FrameSize,
    finished: bool,
}

impl VideoStream {
    /// Starts decoding. `hardware` lets ffmpeg decode on the GPU; see
    /// [`MediaInfo::prefers_hardware_decode`](super::MediaInfo).
    pub fn spawn(
        path: &Path,
        start: Duration,
        size: FrameSize,
        frame_rate: f64,
        timing: Timing,
        hardware: bool,
    ) -> Result<Self> {
        let first = timing.first_frame(start);
        let mut command = base_command();
        if hardware {
            command.args(super::hwaccel_args());
        }
        command
            .arg("-ss")
            .arg(seconds_arg(start))
            .arg("-copyts")
            .arg("-i")
            .arg(path)
            .args(["-an", "-sn", "-vf"])
            .arg(format!(
                "fps={frame_rate:.6}:start_time={:.6},{}",
                timing.origin + first.as_secs_f64(),
                scale_filter(size, "fast_bilinear")
            ))
            .args(["-pix_fmt", "bgra", "-f", "rawvideo", "pipe:1"])
            .no_window()
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        let mut child = command
            .spawn()
            .context("could not start ffmpeg; wait for its download to finish or set its folder in the settings")?;
        let stdout = child.stdout.take().context("ffmpeg has no stdout")?;

        let (sender, frames) = mpsc::sync_channel(QUEUE_DEPTH);
        let path: PathBuf = path.to_owned();
        thread::Builder::new()
            .name(format!("decode {}", path.display()))
            .spawn(move || read_frames(stdout, sender, size, first, frame_rate))
            .context("could not start the decoder thread")?;

        Ok(Self {
            child: Some(child),
            frames,
            size,
            finished: false,
        })
    }

    pub fn size(&self) -> FrameSize {
        self.size
    }

    /// The next decoded frame, if one is ready.
    pub fn try_next(&mut self) -> Option<RawFrame> {
        match self.frames.try_recv() {
            Ok(frame) => Some(frame),
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => {
                self.finished = true;
                None
            }
        }
    }

    /// True once ffmpeg has delivered its last frame.
    pub fn is_finished(&self) -> bool {
        self.finished
    }
}

impl Drop for VideoStream {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            // Reap the process off the UI thread; `wait` can take a moment.
            thread::spawn(move || {
                let _ = child.wait();
            });
        }
    }
}

fn read_frames(
    mut stdout: ChildStdout,
    sender: SyncSender<RawFrame>,
    size: FrameSize,
    start: Duration,
    frame_rate: f64,
) {
    let interval = 1.0 / frame_rate;
    for index in 0u64.. {
        let mut bgra = vec![0; size.byte_len()];
        // EOF means ffmpeg reached the end of the clip; any other error means
        // it was killed. Either way the stream is over.
        if stdout.read_exact(&mut bgra).is_err() {
            return;
        }
        let pts = start + Duration::from_secs_f64(index as f64 * interval);
        if sender.send(RawFrame { size, pts, bgra }).is_err() {
            return; // The player dropped the stream.
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_frames_by_their_time() {
        assert_eq!(
            frame_file_stem("match", Duration::from_millis(72_345)),
            "match-01m12s345"
        );
        assert_eq!(
            frame_file_stem("a", Duration::from_secs(3_600)),
            "a-60m00s000"
        );
    }

    #[test]
    fn reads_frame_times_from_showinfo() {
        assert_eq!(
            showinfo_time(
                "[Parsed_showinfo_1 @ 000001] n:   3 pts: 375000 pts_time:12.5     duration:1500"
            ),
            Some(Duration::from_millis(12_500))
        );
        assert_eq!(
            showinfo_time("[Parsed_showinfo_1 @ 000001] config in time_base: 1/90000"),
            None
        );
        assert_eq!(showinfo_time("Stream #0:0: Video: av1 pts_time:3"), None);
    }

    /// Needs ffmpeg on PATH: `cargo test -- --ignored`.
    #[test]
    #[ignore]
    fn decodes_every_keyframe_with_its_time() {
        let dir = std::env::temp_dir().join(format!("plic-keys-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
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
            .arg("testsrc2=s=320x180:r=30:d=10")
            .args(["-c:v", "libx264", "-g", "60"])
            .arg(&source)
            .status()
            .unwrap();
        assert!(made.success());

        let mut times = Vec::new();
        keyframes(&source, FrameSize::new(64, 36), |frame| {
            assert_eq!(frame.bgra.len(), 64 * 36 * 4);
            times.push(frame.pts.as_secs_f64());
            true
        })
        .unwrap();
        std::fs::remove_dir_all(&dir).ok();
        assert_eq!(times.len(), 5, "{times:?}");
        for (index, time) in times.iter().enumerate() {
            assert!((time - index as f64 * 2.0).abs() < 0.01, "{times:?}");
        }
    }

    #[test]
    fn fits_without_upscaling_and_keeps_even_dimensions() {
        let source = FrameSize::new(1920, 1080);
        assert_eq!(
            source.fit_within(FrameSize::new(960, 960)),
            FrameSize::new(960, 540)
        );
        assert_eq!(source.fit_within(FrameSize::new(4000, 4000)), source);
        assert_eq!(
            FrameSize::new(1080, 1920).fit_within(FrameSize::new(801, 451)),
            FrameSize::new(252, 450)
        );
        assert_eq!(
            source.fit_within(FrameSize::new(1, 1)),
            FrameSize::new(2, 2)
        );
    }
}
