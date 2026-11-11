//! The scrubbable timeline under the video.

use std::time::Duration;

use gpui_kit::component::ActiveTheme as _;
use gpui_kit::*;

use super::player::Player;

const HEIGHT: f32 = 36.;
const TRACK_HEIGHT: f32 = 6.;
const INSET: f32 = 16.;
const PLAYHEAD_RADIUS: f32 = 7.;

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

pub fn render(player: &Player, cx: &mut Context<Player>) -> impl IntoElement {
    let theme = cx.theme();
    let duration = player.duration();
    let position = player.position();
    let track = theme.muted;
    let played = theme.primary;
    let playhead = theme.foreground;
    let this = cx.entity().downgrade();

    canvas(
        move |bounds, window, _| window.insert_hitbox(bounds, HitboxBehavior::Normal),
        move |bounds, hitbox, window, _| {
            let Some(duration) = duration.filter(|duration| !duration.is_zero()) else {
                return;
            };
            let scale = Scale::new(bounds, duration);
            let center_y = bounds.center().y;

            let track_bounds = Bounds {
                origin: point(scale.left, center_y - px(TRACK_HEIGHT / 2.)),
                size: size(scale.width, px(TRACK_HEIGHT)),
            };
            window.paint_quad(fill(track_bounds, track).corner_radii(px(TRACK_HEIGHT / 2.)));

            let head_x = scale.x(position);
            let mut played_bounds = track_bounds;
            played_bounds.size.width = head_x - scale.left;
            window.paint_quad(fill(played_bounds, played).corner_radii(px(TRACK_HEIGHT / 2.)));

            let head = Bounds {
                origin: point(head_x - px(PLAYHEAD_RADIUS), center_y - px(PLAYHEAD_RADIUS)),
                size: size(px(PLAYHEAD_RADIUS * 2.), px(PLAYHEAD_RADIUS * 2.)),
            };
            window.paint_quad(fill(head, playhead).corner_radii(px(PLAYHEAD_RADIUS)));

            window.set_cursor_style(CursorStyle::PointingHand, &hitbox);
            register_scrubbing(this.clone(), hitbox, scale, window);
        },
    )
    .w_full()
    .h(px(HEIGHT))
}

/// Press on the timeline to jump; keep the button down to scrub. Listening
/// at window level keeps the drag alive when the pointer leaves the bar.
fn register_scrubbing(this: WeakEntity<Player>, hitbox: Hitbox, scale: Scale, window: &mut Window) {
    window.on_mouse_event({
        let this = this.clone();
        move |event: &MouseDownEvent, phase, window, cx| {
            if phase != DispatchPhase::Bubble
                || event.button != MouseButton::Left
                || !hitbox.is_hovered(window)
            {
                return;
            }
            this.update(cx, |player, cx| {
                player.begin_scrub(cx);
                player.seek(scale.time(event.position.x), window, cx);
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
                if player.scrubbing {
                    player.seek(scale.time(event.position.x), window, cx);
                }
            })
            .ok();
        }
    });
    window.on_mouse_event(move |event: &MouseUpEvent, phase, window, cx| {
        if phase != DispatchPhase::Bubble || event.button != MouseButton::Left {
            return;
        }
        this.update(cx, |player, cx| {
            if player.scrubbing {
                player.end_scrub(window, cx);
            }
        })
        .ok();
    });
}
