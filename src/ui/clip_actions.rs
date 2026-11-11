//! The bar above the viewer: what the selected clip is, and what can be
//! done with the file itself.

use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariant, ButtonVariants as _};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Icon, Sizable as _, WindowExt as _, h_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use super::app::{ClipEntry, ClipStatus, PlicApp};
use super::library_panel::format_size;
use crate::{library, timestamp};

pub const HEADER_HEIGHT: f32 = 40.;

impl PlicApp {
    pub(super) fn open_externally(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(path) = self.current().map(|entry| entry.file.path.clone()) else {
            return;
        };
        // Leave the clip to the other player rather than playing both.
        self.player.update(cx, |player, cx| player.pause(cx));
        if let Err(error) = open::that_detached(&path) {
            window.push_notification(
                Notification::error(error.to_string()).title("Could not open the clip"),
                cx,
            );
        }
    }

    pub(super) fn show_in_folder(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(path) = self.current().map(|entry| entry.file.path.clone()) else {
            return;
        };
        if let Err(error) = library::show_in_file_manager(&path) {
            window.push_notification(
                Notification::error(format!("{error:#}")).title("Could not show the clip"),
                cx,
            );
        }
    }

    /// Asks before sending the selected clip to the recycle bin.
    pub(super) fn confirm_delete(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(entry) = self.current() else {
            return;
        };
        if entry.status == ClipStatus::Exporting {
            window.push_notification(
                Notification::warning("Cancel its export first.").title("This clip is exporting"),
                cx,
            );
            return;
        }
        let path = entry.file.path.clone();
        let name = entry.file.file_name();
        let this = cx.entity().downgrade();
        window.open_alert_dialog(cx, move |alert, _, cx| {
            let (this, path) = (this.clone(), path.clone());
            alert
                .icon(Icon::new(IconName::Trash).text_color(cx.theme().danger))
                .title(format!("Delete {name}?"))
                .description("It goes to the Recycle Bin, so you can still restore it from there.")
                .ok_text("Delete")
                .ok_variant(ButtonVariant::Danger)
                .show_cancel(true)
                .on_ok(move |_, window, cx| {
                    this.update(cx, |this, cx| this.trash_clip(path.clone(), window, cx))
                        .ok();
                    true
                })
        });
    }

    fn trash_clip(
        &mut self,
        path: std::path::PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // The viewer would keep the file open.
        if self.current().is_some_and(|entry| entry.file.path == path) {
            self.player.update(cx, |player, cx| player.clear(cx));
        }
        let task = cx.background_spawn({
            let path = path.clone();
            async move { library::retry_while_busy(|| library::trash_clip(&path)) }
        });
        cx.spawn_in(window, async move |this, cx| {
            let result = task.await;
            this.update_in(cx, |this, window, cx| {
                let index = this.index_of(&path);
                match result {
                    Ok(()) => {
                        if let Some(index) = index {
                            this.remove_clip(index, window, cx);
                        }
                    }
                    Err(error) => {
                        window.push_notification(
                            Notification::error(format!("{error:#}"))
                                .title("Could not delete the clip"),
                            cx,
                        );
                        if index.is_some() && index == this.selected {
                            this.show_selected_in_player(cx);
                        }
                    }
                }
            })
            .ok();
        })
        .detach();
    }

    pub(super) fn render_clip_header(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let entry = self.current();
        let enabled = entry.is_some();
        h_flex()
            .h(px(HEADER_HEIGHT))
            .flex_none()
            .px_4()
            .gap_3()
            .border_b_1()
            .border_color(theme.border)
            .when_some(entry, |bar, entry| {
                bar.child(
                    div()
                        .flex_shrink(1.)
                        .min_w_0()
                        .text_sm()
                        .font_weight(FontWeight::MEDIUM)
                        .truncate()
                        .child(entry.file.file_name()),
                )
                .child(
                    div()
                        .flex_shrink(1.)
                        .min_w_0()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .truncate()
                        .child(details(entry)),
                )
            })
            .child(div().flex_1())
            .child(
                h_flex()
                    .flex_none()
                    .gap_1()
                    .child(
                        Button::new("open-externally")
                            .ghost()
                            .small()
                            .icon(IconName::ExternalLink)
                            .tooltip("Open in the default player")
                            .disabled(!enabled)
                            .on_click(
                                cx.listener(|this, _, window, cx| this.open_externally(window, cx)),
                            ),
                    )
                    .child(
                        Button::new("show-in-folder")
                            .ghost()
                            .small()
                            .icon(IconName::FolderOpen)
                            .tooltip("Show in folder")
                            .disabled(!enabled)
                            .on_click(
                                cx.listener(|this, _, window, cx| this.show_in_folder(window, cx)),
                            ),
                    )
                    .child(
                        Button::new("delete-clip")
                            .ghost()
                            .small()
                            .icon(IconName::Trash)
                            .tooltip("Delete (Del)")
                            .disabled(!enabled)
                            .on_click(
                                cx.listener(|this, _, window, cx| this.confirm_delete(window, cx)),
                            ),
                    ),
            )
    }
}

/// `1920×1080 · 60 fps · H264 · 0:45 · 41 MB`
fn details(entry: &ClipEntry) -> SharedString {
    let mut parts = Vec::new();
    if let Some(info) = entry.info() {
        if let Some(size) = info.video_size {
            parts.push(format!("{}×{}", size.width, size.height));
        }
        if let Some(rate) = info.frame_rate {
            parts.push(format!("{} fps", (rate * 100.0).round() / 100.0));
        }
        if let Some(codec) = &info.video_codec {
            parts.push(codec.to_uppercase());
        }
        if !info.has_audio {
            parts.push("no audio".into());
        }
        if let Some(duration) = info.duration {
            parts.push(timestamp::format_coarse(duration));
        }
    }
    parts.push(format_size(entry.file.size));
    parts.join(" · ").into()
}
