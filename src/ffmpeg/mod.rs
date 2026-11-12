//! Everything that shells out to the `ffmpeg` / `ffprobe` command-line tools.
//!
//! Using the CLI rather than linking libav keeps the build simple on every
//! platform and lets ffmpeg pick hardware decoders and encoders on its own.

mod export;
mod frames;
mod graph;
mod probe;
mod transform;

use std::{process::Command, time::Duration};

pub use export::{
    CancelToken, ExportError, ExportMode, ExportRequest, estimated_size, export, warm_up_encoder,
};
pub use frames::{FrameSize, RawFrame, VideoStream, extract_frame, extract_keyframe};
pub use graph::{AudioAdjust, TARGET_LOUDNESS};
pub use probe::{MediaInfo, keyframe_at_or_before, probe};
pub use transform::{CropAspect, CropRect, Transform};

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

/// Seconds with millisecond precision, the form ffmpeg's `-ss`/`-t` expect.
pub(crate) fn seconds_arg(duration: Duration) -> String {
    format!("{:.3}", duration.as_secs_f64())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seconds_argument_has_millisecond_precision() {
        assert_eq!(seconds_arg(Duration::from_millis(62_345)), "62.345");
        assert_eq!(seconds_arg(Duration::ZERO), "0.000");
    }
}
