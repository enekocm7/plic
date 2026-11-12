//! The timeline under the video: a filmstrip with the audio waveform along
//! its bottom, and the trim range and the playhead drawn over it.
//!
//! Press anywhere to move the playhead and drag to scrub; drag the handles
//! at either end of the highlighted range to trim. A clip can have several
//! segments: the active one has handles, and pressing inside another one
//! makes it active.
//!
//! The mouse wheel zooms in around the pointer, and Shift with the wheel (or
//! a sideways scroll) pans. Zoomed in far enough, a tick marks every frame.

use std::{sync::Arc, time::Duration};

use gpui_kit::component::ActiveTheme as _;
use gpui_kit::*;

use super::player::Player;
use crate::{
    ffmpeg,
    trim::{Segments, TrimRange},
};

const STRIP_HEIGHT: f32 = 44.;
/// Room above the strip for the playhead knob.
const STRIP_TOP: f32 = 18.;
pub(super) const HEIGHT: f32 = STRIP_TOP + STRIP_HEIGHT + 10.;
const KNOB_SIZE: f32 = 12.;
const INSET: f32 = 16.;
const RADIUS: f32 = 6.;
const HANDLE_WIDTH: f32 = 10.;
const FRAME_BORDER: f32 = 2.;
/// Height of the waveform band along the bottom of the strip.
const WAVEFORM_HEIGHT: f32 = 20.;
/// One waveform bar per this many pixels.
const WAVEFORM_STEP: f32 = 2.;
/// Extra room around a handle tab that still grabs it.
const GRAB_SLACK: f32 = 3.;
/// Frames get a tick once they are at least this far apart.
const FRAME_TICK_SPACING: f32 = 5.;
/// Height of the bar under the strip showing which part is in view.
const OVERVIEW_HEIGHT: f32 = 3.;
/// How much one notch of the mouse wheel zooms.
pub const WHEEL_ZOOM: f64 = 1.25;

/// What a timeline drag moves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Drag {
    Playhead,
    TrimStart,
    TrimEnd,
}

/// Everything the timeline draws, captured when the player renders.
pub struct TimelineState {
    pub duration: Option<Duration>,
    pub position: Duration,
    /// `None` hides the segments, for clips that are only viewed.
    pub segments: Option<Segments>,
    pub filmstrip: Vec<Option<Arc<RenderImage>>>,
    /// Audio peaks from 0 to 1, evenly spread over the clip.
    pub waveform: Option<Arc<[f32]>>,
    /// Where the pointer rests, marked with a faint line.
    pub hover: Option<Duration>,
    pub drag: Option<Drag>,
    pub markers: Vec<Duration>,
    pub aspect_ratio: f32,
    /// The part of the clip shown, when zoomed in.
    pub view: View,
    pub frame_interval: Duration,
}

/// The stretch of the clip the timeline shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct View {
    pub start: Duration,
    /// `None` shows the whole clip.
    pub span: Option<Duration>,
}

impl View {
    pub const WHOLE: Self = Self {
        start: Duration::ZERO,
        span: None,
    };

    /// The span shown of a clip `duration` long.
    pub fn span(&self, duration: Duration) -> Duration {
        self.span.map_or(duration, |span| span.min(duration))
    }

    /// Zooms by `factor` (above 1 zooms in) keeping `anchor` at the same
    /// fraction of the width. The span stays between `min_span` and the
    /// whole clip.
    pub fn zoom(
        self,
        factor: f64,
        anchor: Duration,
        duration: Duration,
        min_span: Duration,
    ) -> Self {
        let span = self.span(duration);
        let fraction = (anchor.saturating_sub(self.start)).as_secs_f64()
            / span.as_secs_f64().max(f64::EPSILON);
        let fraction = fraction.clamp(0.0, 1.0);
        let new_span = span
            .div_f64(factor.max(f64::EPSILON))
            .max(min_span.min(duration));
        if new_span >= duration {
            return Self::WHOLE;
        }
        let start = anchor.saturating_sub(new_span.mul_f64(fraction));
        Self {
            start,
            span: Some(new_span),
        }
        .clamped(duration)
    }

    /// Moves the view by `by` seconds, negative going back.
    pub fn pan(self, by: f64, duration: Duration) -> Self {
        let Some(span) = self.span else {
            return self;
        };
        let start = (self.start.as_secs_f64() + by).max(0.0);
        Self {
            start: Duration::from_secs_f64(start),
            span: Some(span),
        }
        .clamped(duration)
    }

    /// Scrolls so `position` is in view, paging in the direction it went.
    pub fn follow(self, position: Duration, duration: Duration) -> Self {
        let Some(span) = self.span else {
            return self;
        };
        let end = self.start + span;
        let start = if position >= end {
            position.saturating_sub(span.mul_f64(0.1))
        } else if position < self.start {
            position.saturating_sub(span.mul_f64(0.9))
        } else {
            return self;
        };
        Self {
            start,
            span: Some(span),
        }
        .clamped(duration)
    }

    /// Keeps the view inside the clip.
    fn clamped(self, duration: Duration) -> Self {
        match self.span {
            Some(span) if span < duration => Self {
                start: self.start.min(duration - span),
                span: Some(span),
            },
            _ => Self::WHOLE,
        }
    }

    pub fn is_zoomed(&self) -> bool {
        self.span.is_some()
    }
}

/// Horizontal mapping between media time and pixels, for the part of the
/// clip in view.
#[derive(Clone, Copy)]
struct Scale {
    left: Pixels,
    width: Pixels,
    duration: Duration,
    start: Duration,
    span: Duration,
}

impl Scale {
    fn new(bounds: Bounds<Pixels>, duration: Duration, view: View) -> Self {
        Self {
            left: bounds.origin.x + px(INSET),
            width: (bounds.size.width - px(INSET * 2.)).max(px(1.)),
            duration,
            start: view.start,
            span: view.span(duration),
        }
    }

    /// Where `time` falls, which may be outside the strip when zoomed in.
    fn unclamped_x(&self, time: Duration) -> Pixels {
        let offset = time.as_secs_f64() - self.start.as_secs_f64();
        let fraction = offset / self.span.as_secs_f64().max(f64::EPSILON);
        self.left + self.width * fraction as f32
    }

    /// Where `time` falls, pinned to the ends of the strip.
    fn x(&self, time: Duration) -> Pixels {
        self.unclamped_x(time)
            .max(self.left)
            .min(self.left + self.width)
    }

    fn in_view(&self, time: Duration) -> bool {
        time >= self.start && time <= self.start + self.span
    }

    fn time(&self, x: Pixels) -> Duration {
        let fraction = f32::from(x - self.left) / f32::from(self.width);
        (self.start + self.span.mul_f64(fraction.clamp(0.0, 1.0) as f64)).min(self.duration)
    }

    /// Seconds covered by one pixel.
    fn seconds_per_pixel(&self) -> f64 {
        self.span.as_secs_f64() / f64::from(f32::from(self.width))
    }
}

/// Horizontal extent of a range on the strip.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Span {
    start: Pixels,
    end: Pixels,
}

impl Span {
    fn new(scale: &Scale, range: TrimRange) -> Self {
        Self {
            start: scale.x(range.start),
            end: scale.x(range
                .resolved_end(Some(scale.duration))
                .unwrap_or(scale.duration)),
        }
    }

    fn contains(&self, x: Pixels) -> bool {
        x >= self.start && x <= self.end
    }
}

/// Every segment's position on the strip, used for drawing and hit testing.
#[derive(Clone)]
struct Layout {
    spans: Vec<Span>,
    active: usize,
}

impl Layout {
    fn new(scale: &Scale, segments: &Segments) -> Self {
        Self {
            spans: segments
                .ranges()
                .iter()
                .map(|range| Span::new(scale, *range))
                .collect(),
            active: segments.active_index(),
        }
    }

    fn handles(&self) -> Handles {
        let span = self.spans[self.active];
        Handles {
            start: span.start,
            end: span.end,
        }
    }

    /// The inactive segment under `x`, if any. Inside the active segment
    /// the press is for the playhead.
    fn inactive_at(&self, x: Pixels) -> Option<usize> {
        if self.spans[self.active].contains(x) {
            return None;
        }
        self.spans.iter().position(|span| span.contains(x))
    }

    /// The parts of the strip outside every segment.
    fn gaps(&self, left: Pixels, right: Pixels) -> Vec<Span> {
        let mut spans = self.spans.clone();
        spans.sort_by(|a, b| {
            a.start
                .partial_cmp(&b.start)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        let mut gaps = Vec::new();
        let mut covered_to = left;
        for span in spans {
            if span.start > covered_to {
                gaps.push(Span {
                    start: covered_to,
                    end: span.start,
                });
            }
            covered_to = covered_to.max(span.end);
        }
        if covered_to < right {
            gaps.push(Span {
                start: covered_to,
                end: right,
            });
        }
        gaps
    }
}

/// Positions of the active segment's trim handles.
#[derive(Clone, Copy)]
struct Handles {
    start: Pixels,
    end: Pixels,
}

impl Handles {
    /// What a press at `position` drags.
    ///
    /// The band above the strip, where the knob sits, always moves the
    /// playhead. Inside the strip only the handle tabs, which stick out of
    /// the kept range, pick up a trim point; everywhere else moves the
    /// playhead, so it can be grabbed even when it sits on a trim point.
    fn pick(&self, position: Point<Pixels>, strip_top: Pixels) -> Drag {
        if position.y < strip_top {
            return Drag::Playhead;
        }
        let x = position.x;
        let reach = px(HANDLE_WIDTH + GRAB_SLACK);
        let on_start = x <= self.start && x >= self.start - reach;
        let on_end = x >= self.end && x <= self.end + reach;
        match (on_start, on_end) {
            // In a range narrower than the tabs, take the nearer one.
            (true, true) if x - self.start > self.end - x => Drag::TrimEnd,
            (true, _) => Drag::TrimStart,
            (false, true) => Drag::TrimEnd,
            (false, false) => Drag::Playhead,
        }
    }
}

pub fn render(state: TimelineState, cx: &mut Context<Player>) -> impl IntoElement {
    let theme = cx.theme();
    let colors = Colors {
        strip: theme.muted,
        dim: black().opacity(0.6),
        selection: theme.yellow,
        waveform_band: black().opacity(0.45),
        waveform: white().opacity(0.8),
        handle_grip: black().opacity(0.55),
        playhead: white(),
        hover: white().opacity(0.5),
        marker: theme.cyan,
        knob_border: black().opacity(0.35),
        tick: white().opacity(0.55),
        overview: theme.muted_foreground.opacity(0.35),
        overview_view: theme.muted_foreground,
    };
    let this = cx.entity().downgrade();

    canvas(
        move |bounds, window, _| window.insert_hitbox(bounds, HitboxBehavior::Normal),
        move |bounds, hitbox, window, _| {
            let Some(duration) = state.duration.filter(|duration| !duration.is_zero()) else {
                return;
            };
            let scale = Scale::new(bounds, duration, state.view);
            let strip = Bounds {
                origin: point(scale.left, bounds.origin.y + px(STRIP_TOP)),
                size: size(scale.width, px(STRIP_HEIGHT)),
            };
            let in_view = |time: Duration| scale.in_view(time).then(|| scale.x(time));
            let layout = state
                .segments
                .as_ref()
                .map(|segments| Layout::new(&scale, segments));
            let handles = layout.as_ref().map(Layout::handles);

            paint_filmstrip(window, strip, &scale, &state, &colors);
            if let Some(peaks) = &state.waveform {
                paint_waveform(window, strip, &scale, peaks, &colors);
            }
            paint_ticks(window, strip, &scale, state.frame_interval, &colors);
            if let Some(layout) = &layout {
                paint_selection(window, strip, layout, &colors);
            }
            if let Some(x) = state.hover.and_then(in_view) {
                paint_hover(window, strip, x, &colors);
            }
            for x in state.markers.iter().filter_map(|marker| in_view(*marker)) {
                paint_marker(window, strip, x, &colors);
            }
            if state.view.is_zoomed() {
                paint_overview(window, bounds, strip, &scale, &colors);
            }
            if let Some(x) = in_view(state.position) {
                paint_playhead(window, strip, x, &colors);
            }

            let hovered = hitbox
                .is_hovered(window)
                .then(|| pick(handles, window.mouse_position(), strip.origin.y));
            let cursor = match state.drag.or(hovered) {
                Some(Drag::TrimStart | Drag::TrimEnd) => CursorStyle::ResizeLeftRight,
                _ => CursorStyle::PointingHand,
            };
            window.set_cursor_style(cursor, &hitbox);
            register_dragging(this.clone(), hitbox, bounds, scale, layout, strip, window);
        },
    )
    .w_full()
    .h(px(HEIGHT))
}

struct Colors {
    strip: Hsla,
    dim: Hsla,
    selection: Hsla,
    waveform_band: Hsla,
    waveform: Hsla,
    handle_grip: Hsla,
    playhead: Hsla,
    hover: Hsla,
    marker: Hsla,
    knob_border: Hsla,
    tick: Hsla,
    overview: Hsla,
    overview_view: Hsla,
}

fn paint_filmstrip(
    window: &mut Window,
    strip: Bounds<Pixels>,
    scale: &Scale,
    state: &TimelineState,
    colors: &Colors,
) {
    window.paint_quad(fill(strip, colors.strip).corner_radii(px(RADIUS)));
    let count = state.filmstrip.len();
    if count == 0 {
        return;
    }
    // Tiles are spread over the whole clip; zoomed in, only some show and
    // they are stretched.
    let tile_width =
        (scale.unclamped_x(scale.duration) - scale.unclamped_x(Duration::ZERO)) / count as f32;
    let first_x = scale.unclamped_x(Duration::ZERO);
    let zoomed = state.view.is_zoomed();
    // Tiles keep the video's aspect ratio and are cropped to their slot.
    let image_width = (strip.size.height * state.aspect_ratio).max(tile_width);
    let image_height = image_width / state.aspect_ratio;
    let mask = ContentMask { bounds: strip };
    window.with_content_mask(Some(mask), |window| {
        for (index, tile) in state.filmstrip.iter().enumerate() {
            let Some(tile) = tile else { continue };
            let slot = Bounds {
                origin: point(first_x + tile_width * index as f32, strip.origin.y),
                size: size(tile_width, strip.size.height),
            };
            if slot.origin.x > strip.origin.x + strip.size.width
                || slot.origin.x + slot.size.width < strip.origin.x
            {
                continue;
            }
            let image = Bounds {
                origin: point(
                    slot.center().x - image_width / 2.,
                    slot.center().y - image_height / 2.,
                ),
                size: size(image_width, image_height),
            };
            let radius = px(RADIUS);
            let (first, last) = (index == 0 && !zoomed, index + 1 == count && !zoomed);
            let corners = Corners {
                top_left: if first { radius } else { px(0.) },
                bottom_left: if first { radius } else { px(0.) },
                top_right: if last { radius } else { px(0.) },
                bottom_right: if last { radius } else { px(0.) },
            };
            window
                .paint_image(slot, image, corners, tile.clone(), 0, false)
                .ok();
        }
    });
}

/// A tick per frame along the top of the strip, once frames are far
/// enough apart to tell apart; every fifth one is longer.
fn paint_ticks(
    window: &mut Window,
    strip: Bounds<Pixels>,
    scale: &Scale,
    frame_interval: Duration,
    colors: &Colors,
) {
    let interval = frame_interval.as_secs_f64();
    if interval <= 0.0 || interval / scale.seconds_per_pixel() < f64::from(FRAME_TICK_SPACING) {
        return;
    }
    let first = (scale.start.as_secs_f64() / interval).ceil() as u64;
    let last = ((scale.start + scale.span).as_secs_f64() / interval).floor() as u64;
    for frame in first..=last {
        let x = scale.x(Duration::from_secs_f64(frame as f64 * interval));
        let height = if frame % 5 == 0 { px(9.) } else { px(5.) };
        let tick = Bounds {
            origin: point(x - px(0.5), strip.origin.y),
            size: size(px(1.), height),
        };
        window.paint_quad(fill(tick, colors.tick));
    }
}

/// A thin bar under the strip: the whole clip, with the part in view
/// highlighted.
fn paint_overview(
    window: &mut Window,
    bounds: Bounds<Pixels>,
    strip: Bounds<Pixels>,
    scale: &Scale,
    colors: &Colors,
) {
    let top = bounds.origin.y + bounds.size.height - px(OVERVIEW_HEIGHT + 1.);
    let track = Bounds {
        origin: point(strip.origin.x, top),
        size: size(strip.size.width, px(OVERVIEW_HEIGHT)),
    };
    window.paint_quad(fill(track, colors.overview).corner_radii(px(OVERVIEW_HEIGHT / 2.)));
    let total = scale.duration.as_secs_f64().max(f64::EPSILON);
    let from = (scale.start.as_secs_f64() / total) as f32;
    let to = ((scale.start + scale.span).as_secs_f64() / total) as f32;
    let view = Bounds {
        origin: point(strip.origin.x + strip.size.width * from, top),
        size: size(
            (strip.size.width * (to - from)).max(px(4.)),
            px(OVERVIEW_HEIGHT),
        ),
    };
    window.paint_quad(fill(view, colors.overview_view).corner_radii(px(OVERVIEW_HEIGHT / 2.)));
}

/// Bars mirrored around the middle of a dark band along the strip's bottom,
/// over the thumbnails and under the trim range.
fn paint_waveform(
    window: &mut Window,
    strip: Bounds<Pixels>,
    scale: &Scale,
    peaks: &[f32],
    colors: &Colors,
) {
    // Only the peaks of the part in view.
    let total = scale.duration.as_secs_f64().max(f64::EPSILON);
    let index =
        |time: Duration| ((time.as_secs_f64() / total) * peaks.len() as f64).round() as usize;
    let from = index(scale.start).min(peaks.len());
    let to = index(scale.start + scale.span).clamp(from, peaks.len());
    let peaks = if to > from { &peaks[from..to] } else { peaks };
    let band = Bounds {
        origin: point(
            strip.origin.x,
            strip.origin.y + strip.size.height - px(WAVEFORM_HEIGHT),
        ),
        size: size(strip.size.width, px(WAVEFORM_HEIGHT)),
    };
    let radius = px(RADIUS);
    window.paint_quad(fill(band, colors.waveform_band).corner_radii(Corners {
        bottom_left: radius,
        bottom_right: radius,
        ..Default::default()
    }));
    let columns = (f32::from(band.size.width) / WAVEFORM_STEP) as usize;
    let middle = band.center().y;
    let reach = px(WAVEFORM_HEIGHT / 2. - 2.);
    for (column, peak) in ffmpeg::resample_peaks(peaks, columns)
        .into_iter()
        .enumerate()
    {
        // Silence still shows as a hairline so the band reads as audio.
        let half = (reach * peak).max(px(0.5));
        let bar = Bounds {
            origin: point(
                band.origin.x + px(column as f32 * WAVEFORM_STEP),
                middle - half,
            ),
            size: size(px(WAVEFORM_STEP - 0.5), half * 2.),
        };
        window.paint_quad(fill(bar, colors.waveform));
    }
}

fn paint_selection(window: &mut Window, strip: Bounds<Pixels>, layout: &Layout, colors: &Colors) {
    let top = strip.origin.y;
    let height = strip.size.height;
    let left_edge = strip.origin.x;
    let right_edge = strip.origin.x + strip.size.width;

    // Darken what will be cut away.
    for gap in layout.gaps(left_edge, right_edge) {
        let cut = Bounds::from_corners(point(gap.start, top), point(gap.end, top + height));
        let at_left = gap.start <= left_edge;
        let at_right = gap.end >= right_edge;
        let radius = |round: bool| if round { px(RADIUS) } else { px(0.) };
        window.paint_quad(fill(cut, colors.dim).corner_radii(Corners {
            top_left: radius(at_left),
            bottom_left: radius(at_left),
            top_right: radius(at_right),
            bottom_right: radius(at_right),
        }));
    }

    // The other segments get a thin frame; only the active one has handles.
    for (index, span) in layout.spans.iter().enumerate() {
        if index == layout.active {
            continue;
        }
        let frame = Bounds::from_corners(
            point(span.start, top - px(FRAME_BORDER)),
            point(span.end, top + height + px(FRAME_BORDER)),
        );
        window.paint_quad(
            outline(frame, colors.selection.opacity(0.6), BorderStyle::Solid)
                .border_widths(px(FRAME_BORDER))
                .corner_radii(px(RADIUS / 2.)),
        );
    }

    let handles = layout.handles();

    // Frame the kept range, with grab handles on its ends.
    let frame = Bounds::from_corners(
        point(handles.start - px(HANDLE_WIDTH), top - px(FRAME_BORDER)),
        point(
            handles.end + px(HANDLE_WIDTH),
            top + height + px(FRAME_BORDER),
        ),
    );
    window.paint_quad(
        outline(frame, colors.selection, BorderStyle::Solid)
            .border_widths(Edges {
                top: px(FRAME_BORDER),
                bottom: px(FRAME_BORDER),
                left: px(HANDLE_WIDTH),
                right: px(HANDLE_WIDTH),
            })
            .corner_radii(px(RADIUS)),
    );
    for x in [
        handles.start - px(HANDLE_WIDTH / 2.),
        handles.end + px(HANDLE_WIDTH / 2.),
    ] {
        let grip = Bounds {
            origin: point(x - px(1.), strip.center().y - px(7.)),
            size: size(px(2.), px(14.)),
        };
        window.paint_quad(fill(grip, colors.handle_grip).corner_radii(px(1.)));
    }
}

fn paint_hover(window: &mut Window, strip: Bounds<Pixels>, x: Pixels, colors: &Colors) {
    let line = Bounds {
        origin: point(x - px(0.5), strip.origin.y),
        size: size(px(1.), strip.size.height),
    };
    window.paint_quad(fill(line, colors.hover));
}

fn paint_marker(window: &mut Window, strip: Bounds<Pixels>, x: Pixels, colors: &Colors) {
    let line = Bounds {
        origin: point(x - px(0.5), strip.origin.y),
        size: size(px(1.), strip.size.height),
    };
    window.paint_quad(fill(line, colors.marker));
    let flag = Bounds {
        origin: point(x - px(4.), strip.origin.y + strip.size.height),
        size: size(px(8.), px(6.)),
    };
    window.paint_quad(fill(flag, colors.marker).corner_radii(px(2.)));
}

fn paint_playhead(window: &mut Window, strip: Bounds<Pixels>, x: Pixels, colors: &Colors) {
    let line = Bounds {
        origin: point(x - px(1.), strip.origin.y - px(6.)),
        size: size(px(2.), strip.size.height + px(12.)),
    };
    window.paint_quad(fill(line, colors.playhead).corner_radii(px(1.)));
    let knob = Bounds {
        origin: point(x - px(KNOB_SIZE / 2.), strip.origin.y - px(KNOB_SIZE + 3.)),
        size: size(px(KNOB_SIZE), px(KNOB_SIZE)),
    };
    window.paint_quad(
        fill(knob, colors.playhead)
            .corner_radii(px(KNOB_SIZE / 2.))
            .border_widths(px(2.))
            .border_color(colors.knob_border),
    );
}

/// Listening at window level keeps a drag alive when the pointer leaves the
/// timeline.
fn pick(handles: Option<Handles>, position: Point<Pixels>, strip_top: Pixels) -> Drag {
    handles.map_or(Drag::Playhead, |handles| handles.pick(position, strip_top))
}

fn register_dragging(
    this: WeakEntity<Player>,
    hitbox: Hitbox,
    bounds: Bounds<Pixels>,
    scale: Scale,
    layout: Option<Layout>,
    strip: Bounds<Pixels>,
    window: &mut Window,
) {
    let handles = layout.as_ref().map(Layout::handles);
    let strip_top = strip.origin.y;
    window.on_mouse_event({
        let this = this.clone();
        let hitbox = hitbox.clone();
        move |event: &MouseDownEvent, phase, window, cx| {
            if phase != DispatchPhase::Bubble
                || event.button != MouseButton::Left
                || !hitbox.is_hovered(window)
            {
                return;
            }
            let drag = pick(handles, event.position, strip_top);
            // Pressing inside another segment makes it the active one.
            let segment = layout
                .as_ref()
                .filter(|_| drag == Drag::Playhead && strip.contains(&event.position))
                .and_then(|layout| layout.inactive_at(event.position.x));
            this.update(cx, |player, cx| {
                if let Some(index) = segment {
                    player.select_segment(index, window, cx);
                }
                player.begin_drag(drag, cx);
                // Grabbing a handle should not make it jump to the pointer.
                if drag == Drag::Playhead {
                    player.drag_to(scale.time(event.position.x), window, cx);
                }
            })
            .ok();
        }
    });
    window.on_mouse_event({
        let this = this.clone();
        let hitbox = hitbox.clone();
        move |event: &MouseMoveEvent, phase, window, cx| {
            if phase != DispatchPhase::Bubble {
                return;
            }
            if event.pressed_button == Some(MouseButton::Left) {
                this.update(cx, |player, cx| {
                    player.drag_to(scale.time(event.position.x), window, cx);
                })
                .ok();
                return;
            }
            let hovered = hitbox.is_hovered(window);
            this.update(cx, |player, cx| {
                if hovered {
                    let x = event.position.x - bounds.origin.x;
                    player.hover_at(scale.time(event.position.x), x, bounds.size.width, cx);
                } else {
                    player.end_hover(cx);
                }
            })
            .ok();
        }
    });
    window.on_mouse_event({
        let this = this.clone();
        let hitbox = hitbox.clone();
        move |event: &ScrollEvent, phase, window, cx| {
            if phase != DispatchPhase::Bubble || !hitbox.is_hovered(window) {
                return;
            }
            let delta = event.delta.pixel_delta(px(20.));
            let (dx, dy) = (f32::from(delta.x), f32::from(delta.y));
            this.update(cx, |player, cx| {
                if event.modifiers.shift || dx.abs() > dy.abs() {
                    // Scrolling right, or down with Shift, moves later.
                    let pixels = if dx.abs() > dy.abs() { -dx } else { -dy };
                    player.pan_timeline(f64::from(pixels) * scale.seconds_per_pixel(), cx);
                } else if dy != 0.0 {
                    let factor = WHEEL_ZOOM.powf(f64::from(dy) / 20.0);
                    player.zoom_timeline(factor, scale.time(event.position.x), cx);
                }
            })
            .ok();
            cx.stop_propagation();
        }
    });
    window.on_mouse_event(move |event: &MouseUpEvent, phase, window, cx| {
        if phase != DispatchPhase::Bubble || event.button != MouseButton::Left {
            return;
        }
        this.update(cx, |player, cx| player.end_drag(window, cx))
            .ok();
    });
}

#[cfg(test)]
mod tests {
    // Explicit imports: the `gpui_kit::*` glob would shadow `#[test]` with
    // GPUI's own test macro.
    use std::time::Duration;

    use gpui_kit::{Bounds, point, px, size};

    use super::{Drag, HEIGHT, INSET, Layout, STRIP_TOP, Scale, Span, View};
    use crate::trim::{Segments, TrimRange};

    fn scale() -> Scale {
        let bounds = Bounds {
            origin: point(px(0.), px(0.)),
            size: size(px(1000. + INSET * 2.), px(HEIGHT)),
        };
        Scale::new(bounds, Duration::from_secs(100), View::WHOLE)
    }

    #[test]
    fn zoomed_scale_maps_the_view_onto_the_strip() {
        let bounds = Bounds {
            origin: point(px(0.), px(0.)),
            size: size(px(1000. + INSET * 2.), px(HEIGHT)),
        };
        let view = View {
            start: Duration::from_secs(40),
            span: Some(Duration::from_secs(10)),
        };
        let scale = Scale::new(bounds, Duration::from_secs(100), view);
        assert_eq!(scale.x(Duration::from_secs(45)), px(INSET + 500.));
        assert_eq!(scale.time(px(INSET + 1000.)), Duration::from_secs(50));
        assert!(!scale.in_view(Duration::from_secs(60)));
        assert_eq!(scale.x(Duration::from_secs(60)), px(INSET + 1000.));
    }

    #[test]
    fn zooming_keeps_the_anchor_in_place_and_stays_inside_the_clip() {
        let duration = Duration::from_secs(100);
        let min = Duration::from_secs(1);
        let view = View::WHOLE.zoom(4.0, Duration::from_secs(50), duration, min);
        assert_eq!(view.span, Some(Duration::from_secs(25)));
        assert_eq!(view.start, Duration::from_millis(37_500));
        // Zooming in near the end does not scroll past it.
        let view = View::WHOLE.zoom(10.0, Duration::from_secs(100), duration, min);
        assert_eq!(view.start, Duration::from_secs(90));
        // Never closer than the minimum span, and back out to the whole.
        let close = view.zoom(1000.0, Duration::from_secs(95), duration, min);
        assert_eq!(close.span, Some(min));
        assert_eq!(
            close.zoom(0.001, Duration::ZERO, duration, min),
            View::WHOLE
        );
    }

    #[test]
    fn following_pages_the_view_to_the_playhead() {
        let duration = Duration::from_secs(100);
        let view = View {
            start: Duration::from_secs(10),
            span: Some(Duration::from_secs(10)),
        };
        assert_eq!(view.follow(Duration::from_secs(15), duration), view);
        assert_eq!(
            view.follow(Duration::from_secs(20), duration).start,
            Duration::from_secs(19)
        );
        assert_eq!(
            view.follow(Duration::from_secs(5), duration).start,
            Duration::from_secs(0)
        );
        assert_eq!(view.pan(1000.0, duration).start, Duration::from_secs(90));
    }

    #[test]
    fn maps_time_to_pixels_and_back() {
        let scale = scale();
        assert_eq!(scale.x(Duration::from_secs(25)), px(INSET + 250.));
        assert_eq!(scale.time(px(INSET + 500.)), Duration::from_secs(50));
        // Positions outside the track clamp to its ends.
        assert_eq!(scale.time(px(-50.)), Duration::ZERO);
        assert_eq!(scale.time(px(5000.)), Duration::from_secs(100));
    }

    #[test]
    fn handle_tabs_grab_trim_points_and_the_rest_moves_the_playhead() {
        let mut segments = Segments::default();
        segments.set_active(TrimRange::new(
            Duration::from_secs(10),
            Some(Duration::from_secs(60)),
        ));
        let handles = Layout::new(&scale(), &segments).handles();
        let strip_top = px(STRIP_TOP);
        let at = |x: f32, y: f32| point(px(INSET + x), px(y));
        let in_strip = STRIP_TOP + 10.;
        // The tabs sit just outside the kept range.
        assert_eq!(handles.pick(at(95., in_strip), strip_top), Drag::TrimStart);
        assert_eq!(handles.pick(at(605., in_strip), strip_top), Drag::TrimEnd);
        // Just inside the range, or above the strip, is the playhead.
        assert_eq!(handles.pick(at(102., in_strip), strip_top), Drag::Playhead);
        assert_eq!(handles.pick(at(98., 4.), strip_top), Drag::Playhead);
        assert_eq!(handles.pick(at(300., in_strip), strip_top), Drag::Playhead);
    }

    #[test]
    fn finds_gaps_and_inactive_segments() {
        let mut segments = Segments::default();
        segments.set_active(TrimRange::new(
            Duration::from_secs(10),
            Some(Duration::from_secs(20)),
        ));
        segments.add(Duration::from_secs(50), Some(Duration::from_secs(100)));
        segments.set_active(TrimRange::new(
            Duration::from_secs(50),
            Some(Duration::from_secs(60)),
        ));
        segments.select(0);
        let layout = Layout::new(&scale(), &segments);
        let span = |start: f32, end: f32| Span {
            start: px(INSET + start),
            end: px(INSET + end),
        };
        assert_eq!(
            layout.gaps(px(INSET), px(INSET + 1000.)),
            vec![span(0., 100.), span(200., 500.), span(600., 1000.)]
        );
        assert_eq!(layout.inactive_at(px(INSET + 550.)), Some(1));
        assert_eq!(layout.inactive_at(px(INSET + 150.)), None);
        assert_eq!(layout.inactive_at(px(INSET + 300.)), None);
    }
}
