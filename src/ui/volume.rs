//! A slim volume slider: a thin track with a small thumb that grows while
//! hovered. Drag or click to set the level, or scroll over it.

use gpui_kit::component::ActiveTheme as _;
use gpui_kit::*;

use super::player::Player;

const WIDTH: f32 = 88.;
const HEIGHT: f32 = 24.;
const TRACK_HEIGHT: f32 = 4.;
const THUMB: f32 = 10.;
const THUMB_HOVERED: f32 = 14.;
/// How much one notch of the mouse wheel changes the volume.
const WHEEL_STEP: f32 = 0.05;

/// `level` is in `0.0..=1.0`; `muted` draws the track as empty.
pub fn render(level: f32, muted: bool, cx: &mut Context<Player>) -> impl IntoElement {
    let theme = cx.theme();
    let (track, fill, thumb, ring) = (
        theme.muted,
        if muted {
            theme.muted_foreground
        } else {
            theme.foreground
        },
        theme.foreground,
        theme.background,
    );
    let shown = if muted { 0.0 } else { level };
    let this = cx.entity().downgrade();

    canvas(
        move |bounds, window, _| window.insert_hitbox(bounds, HitboxBehavior::Normal),
        move |bounds, hitbox, window, _| {
            let left = bounds.origin.x + px(THUMB_HOVERED / 2.);
            let width = (bounds.size.width - px(THUMB_HOVERED)).max(px(1.));
            let center_y = bounds.center().y;
            let level_at = move |x: Pixels| (f32::from(x - left) / f32::from(width)).clamp(0., 1.);

            let rail = Bounds {
                origin: point(left, center_y - px(TRACK_HEIGHT / 2.)),
                size: size(width, px(TRACK_HEIGHT)),
            };
            window.paint_quad(fill_quad(rail, track));
            let filled = Bounds {
                size: size(width * shown, px(TRACK_HEIGHT)),
                ..rail
            };
            window.paint_quad(fill_quad(filled, fill));

            let hovered = hitbox.is_hovered(window);
            let diameter = if hovered { THUMB_HOVERED } else { THUMB };
            let thumb_bounds = Bounds {
                origin: point(
                    left + width * shown - px(diameter / 2.),
                    center_y - px(diameter / 2.),
                ),
                size: size(px(diameter), px(diameter)),
            };
            window.paint_quad(
                gpui_kit::fill(thumb_bounds, thumb)
                    .corner_radii(px(diameter / 2.))
                    .border_widths(px(2.))
                    .border_color(ring),
            );
            window.set_cursor_style(CursorStyle::PointingHand, &hitbox);

            // Window-level listeners keep a drag going outside the slider.
            window.on_mouse_event({
                let (this, hitbox) = (this.clone(), hitbox.clone());
                move |event: &MouseDownEvent, phase, window, cx| {
                    if phase == DispatchPhase::Bubble
                        && event.button == MouseButton::Left
                        && hitbox.is_hovered(window)
                    {
                        this.update(cx, |player, cx| {
                            player.dragging_volume = true;
                            player.set_volume(level_at(event.position.x), cx)
                        })
                        .ok();
                    }
                }
            });
            window.on_mouse_event({
                let this = this.clone();
                move |event: &MouseMoveEvent, phase, _, cx| {
                    if phase != DispatchPhase::Bubble
                        || event.pressed_button != Some(MouseButton::Left)
                    {
                        return;
                    }
                    this.update(cx, |player, cx| {
                        if player.dragging_volume {
                            player.set_volume(level_at(event.position.x), cx)
                        }
                    })
                    .ok();
                }
            });
            window.on_mouse_event({
                let this = this.clone();
                move |event: &MouseUpEvent, phase, _, cx| {
                    if phase == DispatchPhase::Bubble && event.button == MouseButton::Left {
                        this.update(cx, |player, _| player.dragging_volume = false)
                            .ok();
                    }
                }
            });
            window.on_mouse_event({
                let this = this.clone();
                move |event: &ScrollEvent, phase, window, cx| {
                    if phase != DispatchPhase::Bubble || !hitbox.is_hovered(window) {
                        return;
                    }
                    let delta = event.delta.pixel_delta(px(20.)).y;
                    let notches = f32::from(delta) / 20.;
                    this.update(cx, |player, cx| {
                        let level = (player.volume() + notches * WHEEL_STEP).clamp(0., 1.);
                        player.set_volume(level, cx)
                    })
                    .ok();
                }
            });
        },
    )
    .w(px(WIDTH))
    .h(px(HEIGHT))
}

fn fill_quad(bounds: Bounds<Pixels>, color: Hsla) -> PaintQuad {
    gpui_kit::fill(bounds, color).corner_radii(px(TRACK_HEIGHT / 2.))
}
