//! The settings dialog, opened with Ctrl+, or the gear in the title bar.

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::{ActiveTheme as _, Sizable as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::*;

use super::app::PlicApp;
use crate::{library, settings::Settings};

impl PlicApp {
    pub(super) fn open_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let current = crate::settings::get();
        let seek = cx.new(|cx| {
            InputState::new(window, cx)
                .default_value(current.seek_step_secs.to_string())
                .placeholder("5")
        });
        let archive = cx.new(|cx| {
            InputState::new(window, cx)
                .default_value(current.archive_folder.clone())
                .placeholder(crate::settings::DEFAULT_ARCHIVE_FOLDER)
        });
        let destination = cx.new(|cx| {
            InputState::new(window, cx)
                .default_value(current.default_destination.clone())
                .placeholder("The folder used last")
        });
        let focus = seek.read(cx).focus_handle(cx);
        window.defer(cx, move |window, cx| focus.focus(window, cx));

        let this = cx.entity().downgrade();
        window.open_dialog(cx, move |dialog, _, cx| {
            let (this, seek, archive, destination) = (
                this.clone(),
                seek.clone(),
                archive.clone(),
                destination.clone(),
            );
            let muted = cx.theme().muted_foreground;
            let field = |label: &'static str, hint: &'static str, input: &Entity<InputState>| {
                v_flex()
                    .gap_1()
                    .child(div().text_sm().font_weight(FontWeight::MEDIUM).child(label))
                    .child(Input::new(input).small())
                    .child(div().text_xs().text_color(muted).child(hint))
            };
            dialog
                .title("Settings")
                .w(px(420.))
                .child(
                    v_flex()
                        .gap_4()
                        .child(field(
                            "Seek step (seconds)",
                            "How far the arrow keys move the playhead.",
                            &seek,
                        ))
                        .child(field(
                            "Archive folder",
                            "Exported originals move to this folder inside the clips folder.",
                            &archive,
                        ))
                        .child(field(
                            "Default destination",
                            "Subfolder that exports go to when you have not chosen one.",
                            &destination,
                        )),
                )
                .footer(
                    h_flex()
                        .w_full()
                        .justify_end()
                        .gap_2()
                        .child(
                            Button::new("settings-cancel")
                                .outline()
                                .label("Cancel")
                                .on_click(|_, window, cx| window.close_dialog(cx)),
                        )
                        .child(
                            Button::new("settings-save")
                                .primary()
                                .label("Save")
                                .on_click(move |_, window, cx| {
                                    let seek_step_secs = seek
                                        .read(cx)
                                        .value()
                                        .trim()
                                        .parse()
                                        .unwrap_or(crate::settings::DEFAULT_SEEK_STEP_SECS);
                                    let archive_folder = archive.read(cx).value().to_string();
                                    if !archive_folder.trim().is_empty()
                                        && let Err(error) =
                                            library::validate_file_stem(archive_folder.trim())
                                    {
                                        window.push_notification(
                                            Notification::error(format!("Archive folder: {error}")),
                                            cx,
                                        );
                                        return;
                                    }
                                    let settings = Settings {
                                        seek_step_secs,
                                        archive_folder,
                                        default_destination: destination
                                            .read(cx)
                                            .value()
                                            .to_string(),
                                    };
                                    if let Err(error) = crate::settings::update(settings) {
                                        window.push_notification(
                                            Notification::error(format!(
                                                "Could not save settings: {error:#}"
                                            )),
                                            cx,
                                        );
                                    }
                                    this.update(cx, |_, cx| cx.notify()).ok();
                                    window.close_dialog(cx);
                                }),
                        ),
                )
        });
    }
}
