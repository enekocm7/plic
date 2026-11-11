//! Turning decoded frames into images GPUI can paint.

use std::sync::Arc;

use gpui_kit::RenderImage;
use image::{Frame, RgbaImage};
use smallvec::smallvec;

use crate::ffmpeg::RawFrame;

/// Wraps a decoded frame without copying its pixels.
///
/// GPUI's sprite atlas expects BGRA, which is exactly what the decoder
/// produces; the `RgbaImage` type is only a container here.
pub fn render_image(frame: RawFrame) -> Option<Arc<RenderImage>> {
    let buffer = RgbaImage::from_raw(frame.size.width, frame.size.height, frame.bgra)?;
    Some(Arc::new(RenderImage::new(smallvec![Frame::new(buffer)])))
}
