//! The timeline under the video: a filmstrip with the trim range and the
//! playhead drawn over it.
//!
//! Press anywhere to move the playhead and drag to scrub; drag the handles
//! at either end of the highlighted range to trim. A clip can have several
//! segments: the active one has handles, and pressing inside another one
//! makes it active.

use std::{sync::Arc, time::Duration};

use gpui_kit::component::ActiveTheme as _;
use gpui_kit::*;

use super::player::Player;
use crate::trim::{Segments, TrimRange};

const STRIP_HEIGHT: f32 = 44.;
/// Room above the strip for the playhead knob.
const STRIP_TOP: f32 = 18.;
const HEIGHT: f32 = STRIP_TOP + STRIP_HEIGHT + 10.;
const KNOB_SIZE: f32 = 12.;
const INSET: f32 = 16.;
const RADIUS: f32 = 6.;
const HANDLE_WIDTH: f32 = 10.;
const FRAME_BORDER: f32 = 2.;
/// Extra room around a handle tab that still grabs it.
const GRAB_SLACK: f32 = 3.;

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
    pub drag: Option<Drag>,
    pub aspect_ratio: f32,
}

/// Horizontal mapping between media time and pixels.
#[derive(Clone, Copy)]
struct Scale {
    left: Pixels,
    width: Pixels,
    duration: Duration,
}

impl Scale {
    fn new(bounds: Bounds<Pixels>, duration: Duration) -> Self {
        Self {
            left: bounds.origin.x + px(INSET),
            width: (bounds.size.width - px(INSET * 2.)).max(px(1.)),
            duration,
        }
    }

    fn x(&self, time: Duration) -> Pixels {
        let fraction = time.as_secs_f64() / self.duration.as_secs_f64().max(f64::EPSILON);
        self.left + self.width * fraction.clamp(0.0, 1.0) as f32
    }

    fn time(&self, x: Pixels) -> Duration {
        let fraction = f32::from(x - self.left) / f32::from(self.width);
        self.duration.mul_f64(fraction.clamp(0.0, 1.0) as f64)
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
        handle_grip: black().opacity(0.55),
        playhead: white(),
        knob_border: black().opacity(0.35),
    };
    let this = cx.entity().downgrade();

    canvas(
        move |bounds, window, _| window.insert_hitbox(bounds, HitboxBehavior::Normal),
        move |bounds, hitbox, window, _| {
            let Some(duration) = state.duration.filter(|duration| !duration.is_zero()) else {
                return;
            };
            let scale = Scale::new(bounds, duration);
            let strip = Bounds {
                origin: point(scale.left, bounds.origin.y + px(STRIP_TOP)),
                size: size(scale.width, px(STRIP_HEIGHT)),
            };
            let layout = state
                .segments
                .as_ref()
                .map(|segments| Layout::new(&scale, segments));
            let handles = layout.as_ref().map(Layout::handles);

            paint_filmstrip(window, strip, &state, &colors);
            if let Some(layout) = &layout {
                paint_selection(window, strip, layout, &colors);
            }
            paint_playhead(window, strip, scale.x(state.position), &colors);

            let hovered = hitbox
                .is_hovered(window)
                .then(|| pick(handles, window.mouse_position(), strip.origin.y));
            let cursor = match state.drag.or(hovered) {
                Some(Drag::TrimStart | Drag::TrimEnd) => CursorStyle::ResizeLeftRight,
                _ => CursorStyle::PointingHand,
            };
            window.set_cursor_style(cursor, &hitbox);
            register_dragging(this.clone(), hitbox, scale, layout, strip, window);
        },
    )
    .w_full()
    .h(px(HEIGHT))
}

struct Colors {
    strip: Hsla,
    dim: Hsla,
    selection: Hsla,
    handle_grip: Hsla,
    playhead: Hsla,
    knob_border: Hsla,
}

fn paint_filmstrip(
    window: &mut Window,
    strip: Bounds<Pixels>,
    state: &TimelineState,
    colors: &Colors,
) {
    window.paint_quad(fill(strip, colors.strip).corner_radii(px(RADIUS)));
    let count = state.filmstrip.len();
    if count == 0 {
        return;
    }
    let tile_width = strip.size.width / count as f32;
    // Tiles keep the video's aspect ratio and are cropped to their slot.
    let image_width = (strip.size.height * state.aspect_ratio).max(tile_width);
    let image_height = image_width / state.aspect_ratio;
    for (index, tile) in state.filmstrip.iter().enumerate() {
        let Some(tile) = tile else { continue };
        let slot = Bounds {
            origin: point(strip.origin.x + tile_width * index as f32, strip.origin.y),
            size: size(tile_width, strip.size.height),
        };
        let image = Bounds {
            origin: point(
                slot.center().x - image_width / 2.,
                slot.center().y - image_height / 2.,
            ),
            size: size(image_width, image_height),
        };
        let radius = px(RADIUS);
        let corners = Corners {
            top_left: if index == 0 { radius } else { px(0.) },
            bottom_left: if index == 0 { radius } else { px(0.) },
            top_right: if index + 1 == count { radius } else { px(0.) },
            bottom_right: if index + 1 == count { radius } else { px(0.) },
        };
        window
            .paint_image(slot, image, corners, tile.clone(), 0, false)
            .ok();
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
    scale: Scale,
    layout: Option<Layout>,
    strip: Bounds<Pixels>,
    window: &mut Window,
) {
    let handles = layout.as_ref().map(Layout::handles);
    let strip_top = strip.origin.y;
    window.on_mouse_event({
        let this = this.clone();
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
        move |event: &MouseMoveEvent, phase, window, cx| {
            if phase != DispatchPhase::Bubble || event.pressed_button != Some(MouseButton::Left) {
                return;
            }
            this.update(cx, |player, cx| {
                player.drag_to(scale.time(event.position.x), window, cx);
            })
            .ok();
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

    use super::{Drag, HEIGHT, INSET, Layout, STRIP_TOP, Scale, Span};
    use crate::trim::{Segments, TrimRange};

    fn scale() -> Scale {
        let bounds = Bounds {
            origin: point(px(0.), px(0.)),
            size: size(px(1000. + INSET * 2.), px(HEIGHT)),
        };
        Scale::new(bounds, Duration::from_secs(100))
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
