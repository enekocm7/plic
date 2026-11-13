//! Everything that shells out to the `ffmpeg` / `ffprobe` command-line tools.
//!
//! Using the CLI rather than linking libav keeps the build simple on every
//! platform and lets ffmpeg pick hardware decoders and encoders on its own.

mod animation;
mod encoder;
mod export;
mod frames;
mod graph;
mod probe;
pub mod tools;
mod transform;
mod waveform;

use std::{
    process::Command,
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};

pub use animation::{Animation, Format};
pub use encoder::{Codec, Quality, VideoEncoding, uses_hardware, warm_up as warm_up_encoder};
pub use export::{
    CancelToken, ExportError, ExportMode, ExportRequest, SizeInputs, estimated_size, export,
    target_bitrates,
};
pub use frames::{
    FrameSize, ImageFormat, RawFrame, VideoStream, extract_frame, extract_keyframe,
    frame_file_stem, keyframes, sampled_frames, save_frame,
};
pub use graph::{AudioAdjust, TARGET_LOUDNESS};
pub use probe::{MediaInfo, Timing, keyframe_at_or_before, probe, video_end};
pub use tools::Tool;
pub use transform::{CropAspect, CropRect, OutputLimits, Transform};
pub use waveform::{peaks as audio_peaks, resample as resample_peaks};

/// A command that runs `tool`: plic's own copy, the one in the folder set
/// in the settings, or the one on `PATH`.
pub fn command(tool: Tool) -> Command {
    Command::new(tools::program(tool))
}

pub trait CommandExt {
    /// Keeps the child from opening a console window. Release builds are GUI
    /// programs on Windows, so every ffmpeg spawn would otherwise flash one.
    fn no_window(&mut self) -> &mut Self;
}

impl CommandExt for Command {
    #[cfg(windows)]
    fn no_window(&mut self) -> &mut Self {
        use std::os::windows::process::CommandExt as _;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        self.creation_flags(CREATE_NO_WINDOW)
    }

    #[cfg(not(windows))]
    fn no_window(&mut self) -> &mut Self {
        self
    }
}

/// Set once the GPU has failed to decode AV1, so later decodes go straight to
/// ffmpeg's default decoder.
static AV1_HARDWARE_FAILED: AtomicBool = AtomicBool::new(false);

/// How ffmpeg decodes a clip's video. The default is ffmpeg's own choice,
/// on the CPU.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Decoder {
    /// Decode on the GPU.
    pub hardware: bool,
    /// The clip is AV1. ffmpeg's own AV1 decoder is used, on the GPU: the
    /// build plic downloads has no dav1d, and its fallback, libaom, decodes
    /// 1440p at about 30 frames a second, cannot use the GPU and ignores
    /// `-skip_frame`, so even a keyframe seek decodes every frame.
    pub av1: bool,
}

impl Decoder {
    fn native_av1(self) -> bool {
        self.av1 && !AV1_HARDWARE_FAILED.load(Ordering::Relaxed)
    }

    /// The same decoder on the CPU, for a frame the GPU is not worth
    /// starting up for; AV1 stays on the GPU, which is still faster.
    pub fn without_hardware(self) -> Self {
        Self {
            hardware: false,
            ..self
        }
    }

    /// The arguments that go before `-i`.
    pub(crate) fn args(self) -> Vec<&'static str> {
        let mut args = Vec::new();
        if self.hardware || self.native_av1() {
            args.extend(hwaccel_args());
        }
        if self.native_av1() {
            args.extend(["-c:v", "av1"]);
        }
        args
    }

    /// Runs `decode` with this decoder and, if the GPU turns out unable to
    /// decode AV1, again with ffmpeg's default decoder, which every later
    /// decode then uses.
    pub(crate) fn with_fallback<T>(
        self,
        mut decode: impl FnMut(Decoder) -> anyhow::Result<T>,
    ) -> anyhow::Result<T> {
        let first = decode(self);
        if first.is_ok() || !self.native_av1() {
            return first;
        }
        let fallback = Self { av1: false, ..self };
        let second = decode(fallback);
        if second.is_ok() {
            AV1_HARDWARE_FAILED.store(true, Ordering::Relaxed);
        }
        second.or(first)
    }
}

/// The arguments that let ffmpeg decode the next input on the GPU.
///
/// On Windows Direct3D 11 is named outright: `auto` tries other methods
/// first and, on some systems, ends up decoding AV1 on the CPU at a third
/// of the speed. ffmpeg still falls back to the CPU on its own when the
/// GPU cannot decode a clip.
pub(crate) fn hwaccel_args() -> [&'static str; 2] {
    if cfg!(windows) {
        ["-hwaccel", "d3d11va"]
    } else {
        ["-hwaccel", "auto"]
    }
}

/// Seconds with millisecond precision, the form ffmpeg's `-ss`/`-t` expect.
pub(crate) fn seconds_arg(duration: Duration) -> String {
    format!("{:.3}", duration.as_secs_f64())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_av1_with_ffmpegs_own_decoder_on_the_gpu() {
        assert!(Decoder::default().args().is_empty());
        let hardware = Decoder {
            hardware: true,
            av1: false,
        };
        assert_eq!(hardware.args(), hwaccel_args().to_vec());
        let av1 = Decoder {
            hardware: false,
            av1: true,
        };
        let mut expected = hwaccel_args().to_vec();
        expected.extend(["-c:v", "av1"]);
        assert_eq!(av1.args(), expected);
    }

    #[test]
    fn seconds_argument_has_millisecond_precision() {
        assert_eq!(seconds_arg(Duration::from_millis(62_345)), "62.345");
        assert_eq!(seconds_arg(Duration::ZERO), "0.000");
    }
}
