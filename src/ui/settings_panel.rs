//! The settings dialog, opened with Ctrl+, or the gear in the title bar.

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::{ActiveTheme as _, Sizable as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::*;

use std::collections::BTreeMap;

use super::{
    app::PlicApp,
    keymap::{self, Command},
};
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
        let template = cx.new(|cx| {
            InputState::new(window, cx)
                .default_value(current.name_template.clone())
                .placeholder("{name}")
        });
        let ffmpeg = cx.new(|cx| {
            InputState::new(window, cx)
                .default_value(current.ffmpeg_folder.clone())
                .placeholder("plic's own copy")
        });
        // One field per command, holding its keys separated by commas.
        let keys: Vec<(&'static Command, Entity<InputState>)> = keymap::COMMANDS
            .iter()
            .map(|command| {
                let current = keymap::keys_for(command, &current.key_bindings).join(", ");
                let input = cx.new(|cx| {
                    InputState::new(window, cx)
                        .default_value(current)
                        .placeholder("none")
                });
                (command, input)
            })
            .collect();
        let focus = seek.read(cx).focus_handle(cx);
        window.defer(cx, move |window, cx| focus.focus(window, cx));

        let this = cx.entity().downgrade();
        window.open_dialog(cx, move |dialog, _, cx| {
            let (this, seek, archive, destination, template, ffmpeg, keys) = (
                this.clone(),
                seek.clone(),
                archive.clone(),
                destination.clone(),
                template.clone(),
                ffmpeg.clone(),
                keys.clone(),
            );
            let muted = cx.theme().muted_foreground;
            let field = |label: &'static str, hint: &'static str, input: &Entity<InputState>| {
                v_flex()
                    .gap_1()
                    .child(div().text_sm().font_weight(FontWeight::MEDIUM).child(label))
                    .child(Input::new(input).small())
                    .child(div().text_xs().text_color(muted).child(hint))
            };
            let key_rows = keys.iter().map(|(command, input)| {
                h_flex()
                    .gap_3()
                    .child(div().flex_1().min_w_0().text_sm().truncate().child(command.description))
                    .child(div().w(px(170.)).flex_none().child(Input::new(input).small()))
            });
            let key_list = v_flex()
                .gap_2()
                .child(div().text_sm().font_weight(FontWeight::MEDIUM).child("Key bindings"))
                .child(div().text_xs().text_color(muted).child(
                    "Separate alternative keys with commas, e.g. space, k. Use ctrl-, shift- and alt- for modifiers. Leave a field empty to unbind it.",
                ))
                .child(
                    v_flex()
                        .id("settings-keys")
                        .gap_1()
                        .max_h(px(220.))
                        .overflow_y_scroll()
                        .pr_2()
                        .children(key_rows),
                )
                .child(
                    Button::new("settings-keys-reset")
                        .ghost()
                        .small()
                        .label("Restore default keys")
                        .on_click({
                            let keys = keys.clone();
                            move |_, window, cx| {
                                for (command, input) in &keys {
                                    let defaults = command.defaults.join(", ");
                                    input.update(cx, |input, cx| {
                                        input.set_value(defaults, window, cx)
                                    });
                                }
                            }
                        }),
                );
            dialog
                .title("Settings")
                .w(px(480.))
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
                            "Subfolder, or \"Clips folder\", that exports go to when you have not chosen one.",
                            &destination,
                        ))
                        .child(field(
                            "Name template",
                            "Pre-fills export names. Use {name}, {folder}, {date} and {n}, which counts up past names already used. Empty keeps the clip's name.",
                            &template,
                        ))
                        .child(field(
                            "ffmpeg folder",
                            "A folder with your own ffmpeg and ffprobe, used instead of the copy plic downloads. Empty uses plic's copy.",
                            &ffmpeg,
                        ))
                        .child(key_list),
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
                                    let mut key_bindings = BTreeMap::new();
                                    for (command, input) in &keys {
                                        let typed: Vec<String> = input
                                            .read(cx)
                                            .value()
                                            .split(',')
                                            .map(str::to_owned)
                                            .collect();
                                        let cleaned = match keymap::clean_keys(&typed) {
                                            Ok(cleaned) => cleaned,
                                            Err(key) => {
                                                window.push_notification(
                                                    Notification::error(format!(
                                                        "{}: \"{key}\" is not a key",
                                                        command.description
                                                    )),
                                                    cx,
                                                );
                                                return;
                                            }
                                        };
                                        // Only changes are saved, so new
                                        // defaults still reach everyone else.
                                        if cleaned != command.defaults {
                                            key_bindings.insert(command.id.to_owned(), cleaned);
                                        }
                                    }
                                    let settings = Settings {
                                        seek_step_secs,
                                        archive_folder,
                                        default_destination: destination
                                            .read(cx)
                                            .value()
                                            .to_string(),
                                        name_template: template.read(cx).value().to_string(),
                                        ffmpeg_folder: ffmpeg.read(cx).value().to_string(),
                                        key_bindings,
                                    };
                                    if let Err(error) = crate::settings::update(settings) {
                                        window.push_notification(
                                            Notification::error(format!(
                                                "Could not save settings: {error:#}"
                                            )),
                                            cx,
                                        );
                                    }
                                    keymap::apply(cx);
                                    // Clearing the folder may call for plic's own copy.
                                    this.update(cx, |this, cx| {
                                        this.set_up_ffmpeg(window, cx);
                                        cx.notify()
                                    })
                                    .ok();
                                    window.close_dialog(cx);
                                }),
                        ),
                )
        });
    }
}
