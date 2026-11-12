//! Cropping, rotating, flipping and downscaling the exported picture.
//!
//! All of it is a video filter, so an export with a transform or limits is
//! always re-encoded.

use super::FrameSize;

/// A quarter-turn rotation of the output, clockwise.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Rotation {
    #[default]
    None,
    Quarter,
    Half,
    ThreeQuarters,
}

impl Rotation {
    pub fn clockwise(self) -> Self {
        match self {
            Self::None => Self::Quarter,
            Self::Quarter => Self::Half,
            Self::Half => Self::ThreeQuarters,
            Self::ThreeQuarters => Self::None,
        }
    }

    pub fn counter_clockwise(self) -> Self {
        match self {
            Self::None => Self::ThreeQuarters,
            Self::Quarter => Self::None,
            Self::Half => Self::Quarter,
            Self::ThreeQuarters => Self::Half,
        }
    }

    pub fn degrees(self) -> u16 {
        match self {
            Self::None => 0,
            Self::Quarter => 90,
            Self::Half => 180,
            Self::ThreeQuarters => 270,
        }
    }

    fn is_sideways(self) -> bool {
        matches!(self, Self::Quarter | Self::ThreeQuarters)
    }
}

/// The aspect ratio the output is cropped to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CropAspect {
    Wide,
    Tall,
    Square,
    Portrait,
}

impl CropAspect {
    pub const ALL: [Self; 4] = [Self::Wide, Self::Tall, Self::Square, Self::Portrait];

    pub fn label(self) -> &'static str {
        match self {
            Self::Wide => "16:9",
            Self::Tall => "9:16",
            Self::Square => "1:1",
            Self::Portrait => "4:5",
        }
    }

    /// Width over height.
    fn ratio(self) -> f64 {
        match self {
            Self::Wide => 16.0 / 9.0,
            Self::Tall => 9.0 / 16.0,
            Self::Square => 1.0,
            Self::Portrait => 4.0 / 5.0,
        }
    }
}

/// A rectangle as fractions of the frame, for drawing the crop overlay.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CropRect {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

/// How the exported picture differs from the source.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Transform {
    /// The aspect ratio of the output, after rotation.
    pub crop: Option<CropAspect>,
    /// Where the crop sits along the axis it cuts: 0 is the left or top
    /// edge, 1 the right or bottom one.
    pub crop_position: f32,
    pub rotation: Rotation,
    pub flip_horizontal: bool,
    pub flip_vertical: bool,
}

impl Default for Transform {
    fn default() -> Self {
        Self::identity()
    }
}

impl Transform {
    pub const fn identity() -> Self {
        Self {
            crop: None,
            crop_position: 0.5,
            rotation: Rotation::None,
            flip_horizontal: false,
            flip_vertical: false,
        }
    }

    pub fn is_identity(&self) -> bool {
        self.crop.is_none()
            && self.rotation == Rotation::None
            && !self.flip_horizontal
            && !self.flip_vertical
    }

    /// The part of a `source`-sized frame that is kept, in pixels, as
    /// `(x, y, width, height)` with even sizes.
    fn crop_pixels(&self, source: FrameSize) -> Option<(u32, u32, u32, u32)> {
        let aspect = self.crop?;
        // The crop is taken before rotating, so a sideways output needs the
        // inverse shape from the source.
        let ratio = if self.rotation.is_sideways() {
            1.0 / aspect.ratio()
        } else {
            aspect.ratio()
        };
        let (width, height) = (source.width as f64, source.height as f64);
        let even = |value: f64| ((value / 2.0).floor() as u32 * 2).max(2);
        let position = self.crop_position.clamp(0.0, 1.0) as f64;
        Some(if width / height > ratio {
            let kept = even(height * ratio).min(source.width);
            let x = ((source.width - kept) as f64 * position).round() as u32;
            (x, 0, kept, even(height).min(source.height))
        } else {
            let kept = even(width / ratio).min(source.height);
            let y = ((source.height - kept) as f64 * position).round() as u32;
            (0, y, even(width).min(source.width), kept)
        })
    }

    /// The crop as fractions of the source frame, for the viewer overlay.
    pub fn crop_rect(&self, source: FrameSize) -> Option<CropRect> {
        let (x, y, width, height) = self.crop_pixels(source)?;
        let (w, h) = (source.width as f32, source.height as f32);
        Some(CropRect {
            x: x as f32 / w,
            y: y as f32 / h,
            width: width as f32 / w,
            height: height as f32 / h,
        })
    }

    /// The size of the picture after cropping and rotating a `source`-sized
    /// frame.
    pub fn output_size(&self, source: FrameSize) -> FrameSize {
        let (width, height) = self
            .crop_pixels(source)
            .map_or((source.width, source.height), |(_, _, width, height)| {
                (width, height)
            });
        if self.rotation.is_sideways() {
            FrameSize::new(height, width)
        } else {
            FrameSize::new(width, height)
        }
    }

    /// The video filter chain that applies this transform to a frame of
    /// `source` size (after the source's own rotation metadata), or `None`
    /// when the picture is left alone.
    pub fn filter(&self, source: Option<FrameSize>) -> Option<String> {
        let mut filters = Vec::new();
        if let Some((x, y, width, height)) = source.and_then(|size| self.crop_pixels(size)) {
            filters.push(format!("crop={width}:{height}:{x}:{y}"));
        }
        match self.rotation {
            Rotation::None => {}
            Rotation::Quarter => filters.push("transpose=clock".into()),
            Rotation::Half => filters.push("hflip,vflip".into()),
            Rotation::ThreeQuarters => filters.push("transpose=cclock".into()),
        }
        if self.flip_horizontal {
            filters.push("hflip".into());
        }
        if self.flip_vertical {
            filters.push("vflip".into());
        }
        (!filters.is_empty()).then(|| filters.join(","))
    }
}

/// Caps on the exported picture's size and frame rate. Never upscales.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct OutputLimits {
    /// The most pixels the shorter side may have: 1080 for "1080p", which
    /// is the width of a vertical video.
    pub max_lines: Option<u32>,
    pub max_fps: Option<u32>,
}

impl OutputLimits {
    pub fn is_none(&self) -> bool {
        self.max_lines.is_none() && self.max_fps.is_none()
    }

    /// The size a `size` picture is scaled down to, keeping its aspect ratio
    /// with even sides, or `None` when it already fits.
    pub fn scaled_size(&self, size: FrameSize) -> Option<FrameSize> {
        let limit = self.max_lines?;
        let short = size.width.min(size.height);
        if short <= limit {
            return None;
        }
        let scale = limit as f64 / short as f64;
        let even = |value: f64| ((value / 2.0).round() as u32 * 2).max(2);
        Some(FrameSize::new(
            even(size.width as f64 * scale),
            even(size.height as f64 * scale),
        ))
    }

    /// Filters that apply the limits to a `size` picture at `frame_rate`.
    pub fn filters(&self, size: Option<FrameSize>, frame_rate: Option<f64>) -> Vec<String> {
        let mut filters = Vec::new();
        if let Some(scaled) = size.and_then(|size| self.scaled_size(size)) {
            filters.push(format!(
                "scale={}:{}:flags=lanczos",
                scaled.width, scaled.height
            ));
        }
        if let Some(cap) = self.max_fps {
            // An unknown rate is capped too; fps never adds frames.
            if frame_rate.is_none_or(|rate| rate > cap as f64 + 0.5) {
                filters.push(format!("fps={cap}"));
            }
        }
        filters
    }
}

/// The whole video filter chain: the transform, then the limits applied to
/// the transformed picture.
pub fn video_filter(
    transform: &Transform,
    limits: &OutputLimits,
    source: Option<FrameSize>,
    frame_rate: Option<f64>,
) -> Option<String> {
    let mut filters: Vec<String> = transform.filter(source).into_iter().collect();
    filters.extend(limits.filters(source.map(|size| transform.output_size(size)), frame_rate));
    (!filters.is_empty()).then(|| filters.join(","))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn limits_scale_down_by_the_short_side_and_cap_the_rate() {
        let hd = OutputLimits {
            max_lines: Some(720),
            max_fps: Some(30),
        };
        assert_eq!(
            hd.filters(Some(FrameSize::new(2560, 1440)), Some(60.0)),
            ["scale=1280:720:flags=lanczos", "fps=30"]
        );
        // Vertical video: 720p means 720 wide.
        assert_eq!(
            hd.scaled_size(FrameSize::new(1080, 1920)),
            Some(FrameSize::new(720, 1280))
        );
        // Never upscaled, and a slower source keeps its rate.
        assert!(
            hd.filters(Some(FrameSize::new(640, 360)), Some(30.0))
                .is_empty()
        );
    }

    #[test]
    fn limits_apply_after_the_crop() {
        let transform = Transform {
            crop: Some(CropAspect::Tall),
            ..Transform::identity()
        };
        let limits = OutputLimits {
            max_lines: Some(480),
            max_fps: None,
        };
        // The 606x1080 vertical crop is 606 wide, scaled to 480 wide.
        assert_eq!(
            video_filter(&transform, &limits, Some(HD), None).as_deref(),
            Some("crop=606:1080:657:0,scale=480:856:flags=lanczos")
        );
    }

    const HD: FrameSize = FrameSize::new(1920, 1080);

    #[test]
    fn identity_has_no_filter() {
        assert!(Transform::identity().is_identity());
        assert_eq!(Transform::identity().filter(Some(HD)), None);
    }

    #[test]
    fn crops_a_vertical_slice_out_of_a_landscape_video() {
        let transform = Transform {
            crop: Some(CropAspect::Tall),
            ..Transform::identity()
        };
        // 1080 * 9 / 16 = 607.5, rounded down to an even width.
        assert_eq!(
            transform.filter(Some(HD)).as_deref(),
            Some("crop=606:1080:657:0")
        );
        let left = Transform {
            crop_position: 0.0,
            ..transform
        };
        assert_eq!(left.filter(Some(HD)).as_deref(), Some("crop=606:1080:0:0"));
        let rect = transform.crop_rect(HD).unwrap();
        assert!((rect.width - 606.0 / 1920.0).abs() < 1e-6);
        assert_eq!(rect.height, 1.0);
    }

    #[test]
    fn crops_before_rotating_so_the_output_has_the_asked_shape() {
        let transform = Transform {
            crop: Some(CropAspect::Tall),
            rotation: Rotation::Quarter,
            flip_horizontal: true,
            ..Transform::identity()
        };
        // Sideways, a 9:16 output is a 16:9 crop of the source: all of it.
        assert_eq!(
            transform.filter(Some(HD)).as_deref(),
            Some("crop=1920:1080:0:0,transpose=clock,hflip")
        );
    }

    #[test]
    fn rotation_cycles() {
        let mut rotation = Rotation::None;
        for degrees in [90, 180, 270, 0] {
            rotation = rotation.clockwise();
            assert_eq!(rotation.degrees(), degrees);
        }
        assert_eq!(Rotation::None.counter_clockwise(), Rotation::ThreeQuarters);
    }
}
