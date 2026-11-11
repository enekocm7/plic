//! Decoding video frames to raw BGRA pixels, the format GPUI uploads as-is.

use std::{
    io::Read,
    path::{Path, PathBuf},
    process::{Child, ChildStdout, Command, Stdio},
    sync::mpsc::{self, Receiver, SyncSender, TryRecvError},
    thread,
    time::Duration,
};

use anyhow::{Context, Result, bail};

use super::{CommandExt as _, seconds_arg};

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
pub struct RawFrame {
    pub size: FrameSize,
    /// Presentation time relative to the start of the clip.
    pub pts: Duration,
    pub bgra: Vec<u8>,
}

/// Decodes the exact frame shown at `at`, scaled to `size`.
pub fn extract_frame(path: &Path, at: Duration, size: FrameSize) -> Result<RawFrame> {
    let mut command = base_command();
    command.arg("-ss").arg(seconds_arg(at)).arg("-i").arg(path);
    run_single_frame(command, size, at, "bicubic")
}

/// Decodes a frame near `at`, normally the keyframe before it. Much faster
/// than [`extract_frame`] because frames between keyframes are skipped; good
/// enough for thumbnails.
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

fn base_command() -> Command {
    let mut command = Command::new("ffmpeg");
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
        .context("could not start ffmpeg; check that it is installed and on PATH")?;
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

fn scale_filter(size: FrameSize, scaler: &str) -> String {
    format!("scale={}:{}:flags={scaler}", size.width, size.height)
}

/// Frames decoded ahead of the playhead. Small, because every frame is a full
/// uncompressed image; the bound also pauses ffmpeg when playback pauses.
const QUEUE_DEPTH: usize = 6;

/// A running ffmpeg process decoding frames from `start` onwards.
///
/// Frames are delivered at a constant `frame_rate`, so frame `n` is shown at
/// `start + n / frame_rate`. Dropping the stream stops ffmpeg.
pub struct VideoStream {
    child: Option<Child>,
    frames: Receiver<RawFrame>,
    size: FrameSize,
    finished: bool,
}

impl VideoStream {
    pub fn spawn(path: &Path, start: Duration, size: FrameSize, frame_rate: f64) -> Result<Self> {
        let mut command = base_command();
        command
            .args(["-hwaccel", "auto"])
            .arg("-ss")
            .arg(seconds_arg(start))
            .arg("-i")
            .arg(path)
            .args(["-an", "-sn", "-vf"])
            .arg(format!(
                "fps={frame_rate:.6},{}",
                scale_filter(size, "fast_bilinear")
            ))
            .args(["-pix_fmt", "bgra", "-f", "rawvideo", "pipe:1"])
            .no_window()
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        let mut child = command
            .spawn()
            .context("could not start ffmpeg; check that it is installed and on PATH")?;
        let stdout = child.stdout.take().context("ffmpeg has no stdout")?;

        let (sender, frames) = mpsc::sync_channel(QUEUE_DEPTH);
        let path: PathBuf = path.to_owned();
        thread::Builder::new()
            .name(format!("decode {}", path.display()))
            .spawn(move || read_frames(stdout, sender, size, start, frame_rate))
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
