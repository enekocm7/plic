//! The timeline under the video: a filmstrip with the trim range and the
//! playhead drawn over it.
//!
//! Press anywhere to move the playhead and drag to scrub; drag the handles
//! at either end of the highlighted range to trim.

use std::{sync::Arc, time::Duration};

use gpui_kit::component::ActiveTheme as _;
use gpui_kit::*;

use super::player::Player;
use crate::trim::TrimRange;

const STRIP_HEIGHT: f32 = 44.;
/// Room above the strip for the playhead knob.
const STRIP_TOP: f32 = 18.;
const HEIGHT: f32 = STRIP_TOP + STRIP_HEIGHT + 10.;
const KNOB_SIZE: f32 = 12.;
const INSET: f32 = 16.;
const RADIUS: f32 = 6.;
const HANDLE_WIDTH: f32 = 10.;
const FRAME_BORDER: f32 = 2.;
/// How close the pointer must be to a handle to grab it.
const HANDLE_REACH: f32 = 10.;

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
    pub trim: TrimRange,
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

/// Positions of the trim handles, used for both drawing and hit testing.
#[derive(Clone, Copy)]
struct Handles {
    start: Pixels,
    end: Pixels,
}

impl Handles {
    fn new(scale: &Scale, trim: TrimRange) -> Self {
        Self {
            start: scale.x(trim.start),
            end: scale.x(trim
                .resolved_end(Some(scale.duration))
                .unwrap_or(scale.duration)),
        }
    }

    /// Which handle, if any, a press at `x` grabs. Handles win over the
    /// playhead so a trim point can always be picked up.
    fn hit(&self, x: Pixels) -> Option<Drag> {
        let distance = |handle: Pixels| f32::from((x - handle).abs());
        let (to_start, to_end) = (distance(self.start), distance(self.end));
        if to_start.min(to_end) > HANDLE_REACH {
            return None;
        }
        // Inside a tiny range, grab the handle on the side being pressed.
        Some(
            if to_start < to_end || (to_start == to_end && x < self.start) {
                Drag::TrimStart
            } else {
                Drag::TrimEnd
            },
        )
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
            let handles = Handles::new(&scale, state.trim);

            paint_filmstrip(window, strip, &state, &colors);
            paint_selection(window, strip, handles, &colors);
            paint_playhead(window, strip, scale.x(state.position), &colors);

            let hovered_handle = hitbox
                .is_hovered(window)
                .then(|| handles.hit(window.mouse_position().x))
                .flatten();
            let cursor = match state.drag.or(hovered_handle) {
                Some(Drag::TrimStart | Drag::TrimEnd) => CursorStyle::ResizeLeftRight,
                _ => CursorStyle::PointingHand,
            };
            window.set_cursor_style(cursor, &hitbox);
            register_dragging(this.clone(), hitbox, scale, handles, window);
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

fn paint_selection(window: &mut Window, strip: Bounds<Pixels>, handles: Handles, colors: &Colors) {
    let top = strip.origin.y;
    let height = strip.size.height;
    let left_edge = strip.origin.x;
    let right_edge = strip.origin.x + strip.size.width;

    // Darken what will be cut away.
    if handles.start > left_edge {
        let cut = Bounds::from_corners(point(left_edge, top), point(handles.start, top + height));
        window.paint_quad(fill(cut, colors.dim).corner_radii(Corners {
            top_left: px(RADIUS),
            bottom_left: px(RADIUS),
            ..Default::default()
        }));
    }
    if handles.end < right_edge {
        let cut = Bounds::from_corners(point(handles.end, top), point(right_edge, top + height));
        window.paint_quad(fill(cut, colors.dim).corner_radii(Corners {
            top_right: px(RADIUS),
            bottom_right: px(RADIUS),
            ..Default::default()
        }));
    }

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
fn register_dragging(
    this: WeakEntity<Player>,
    hitbox: Hitbox,
    scale: Scale,
    handles: Handles,
    window: &mut Window,
) {
    window.on_mouse_event({
        let this = this.clone();
        move |event: &MouseDownEvent, phase, window, cx| {
            if phase != DispatchPhase::Bubble
                || event.button != MouseButton::Left
                || !hitbox.is_hovered(window)
            {
                return;
            }
            let drag = handles.hit(event.position.x).unwrap_or(Drag::Playhead);
            this.update(cx, |player, cx| {
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

    use super::{Drag, HEIGHT, Handles, INSET, Scale};
    use crate::trim::TrimRange;

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
    fn grabs_the_nearest_handle_within_reach() {
        let handles = Handles::new(
            &scale(),
            TrimRange::new(Duration::from_secs(10), Some(Duration::from_secs(60))),
        );
        assert_eq!(handles.hit(px(INSET + 104.)), Some(Drag::TrimStart));
        assert_eq!(handles.hit(px(INSET + 596.)), Some(Drag::TrimEnd));
        assert_eq!(handles.hit(px(INSET + 300.)), None);
    }
}
