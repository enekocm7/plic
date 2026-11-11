//! Exporting short loops as animated GIF or WebP.
//!
//! Both are silent and re-encoded at a reduced width and frame rate, since
//! their size grows quickly with either. GIF gets a palette generated from
//! the clip itself, which looks far better than the default one.

use std::time::Duration;

use super::FrameSize;

/// The container and codec of an export.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Format {
    #[default]
    Mp4,
    Gif,
    Webp,
}

impl Format {
    pub const ALL: [Self; 3] = [Self::Mp4, Self::Gif, Self::Webp];

    pub fn label(self) -> &'static str {
        match self {
            Self::Mp4 => "MP4",
            Self::Gif => "GIF",
            Self::Webp => "WebP",
        }
    }

    pub fn extension(self) -> &'static str {
        match self {
            Self::Mp4 => "mp4",
            Self::Gif => "gif",
            Self::Webp => "webp",
        }
    }

    pub fn is_animation(self) -> bool {
        self != Self::Mp4
    }

    /// Rough bytes per pixel per frame, measured on busy game footage at the
    /// settings below; good enough to warn before a huge file.
    fn bytes_per_pixel(self) -> f64 {
        match self {
            Self::Mp4 => 0.0,
            Self::Gif => 0.10,
            Self::Webp => 0.045,
        }
    }

    /// Encoder arguments for an animation of this format.
    pub fn encoder_args(self) -> &'static [&'static str] {
        match self {
            Self::Mp4 => &[],
            Self::Gif => &["-c:v", "gif", "-loop", "0"],
            Self::Webp => &[
                "-c:v",
                "libwebp_anim",
                "-lossless",
                "0",
                "-q:v",
                "70",
                "-preset",
                "default",
                "-loop",
                "0",
            ],
        }
    }
}

/// Width and frame rate of an animated export.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Animation {
    /// The widest the animation may be; smaller sources keep their width.
    pub width: u32,
    pub fps: u32,
}

impl Default for Animation {
    fn default() -> Self {
        Self {
            width: 480,
            fps: 15,
        }
    }
}

impl Animation {
    pub const WIDTHS: [u32; 4] = [320, 480, 640, 800];
    pub const FRAME_RATES: [u32; 3] = [10, 15, 24];

    /// The size a `source`-sized picture is shrunk to, keeping its aspect.
    pub fn output_size(&self, source: FrameSize) -> FrameSize {
        let width = self.width.min(source.width);
        let height = (source.height as f64 * width as f64 / source.width.max(1) as f64 / 2.0)
            .round() as u32
            * 2;
        FrameSize::new(width, height.max(2))
    }

    /// The filter chain that turns the video into `format` frames. For a GIF
    /// it ends in a palette pass, so it must be the last chain of a graph.
    pub fn chain(&self, format: Format) -> String {
        let shrink = format!(
            "fps={},scale='min({},iw)':-2:flags=lanczos",
            self.fps, self.width
        );
        match format {
            Format::Gif => format!(
                "{shrink},split[gif_a][gif_b];[gif_a]palettegen=stats_mode=diff[gif_palette];\
                 [gif_b][gif_palette]paletteuse=dither=bayer:bayer_scale=5"
            ),
            _ => shrink,
        }
    }

    /// Roughly how large an animation of `length` from a `source`-sized
    /// picture will be.
    pub fn estimated_size(&self, format: Format, source: FrameSize, length: Duration) -> u64 {
        let size = self.output_size(source);
        let frames = length.as_secs_f64() * self.fps as f64;
        (size.width as f64 * size.height as f64 * frames * format.bytes_per_pixel()) as u64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shrinks_without_upscaling() {
        let animation = Animation::default();
        assert_eq!(
            animation.output_size(FrameSize::new(1920, 1080)),
            FrameSize::new(480, 270)
        );
        assert_eq!(
            animation.output_size(FrameSize::new(320, 240)),
            FrameSize::new(320, 240)
        );
    }

    #[test]
    fn gif_chain_builds_its_own_palette() {
        let chain = Animation::default().chain(Format::Gif);
        assert!(chain.starts_with("fps=15,scale='min(480,iw)':-2:flags=lanczos,split"));
        assert!(chain.ends_with("paletteuse=dither=bayer:bayer_scale=5"));
        assert_eq!(
            Animation::default().chain(Format::Webp),
            "fps=15,scale='min(480,iw)':-2:flags=lanczos"
        );
    }

    #[test]
    fn estimates_grow_with_length() {
        let animation = Animation::default();
        let hd = FrameSize::new(1920, 1080);
        let four = animation.estimated_size(Format::Gif, hd, Duration::from_secs(4));
        // 480x270, 60 frames, 0.1 bytes a pixel.
        assert_eq!(four, 777_600);
        assert!(animation.estimated_size(Format::Webp, hd, Duration::from_secs(4)) < four);
    }
}
