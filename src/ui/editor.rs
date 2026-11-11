//! The export panel: output name, trim range and destination of the
//! selected clip.
//!
//! The text fields and the timeline edit the same range. Typing a valid
//! time moves the handles; dragging a handle rewrites the text.

use std::path::Path;

use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::kbd::Kbd;
use gpui_kit::component::notification::Notification;
use gpui_kit::component::select::{Select, SelectEvent, SelectState};
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Icon, Sizable as _, WindowExt as _, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use super::app::{ClipEntry, ClipStatus, PlicApp, display_name};
use super::library_panel::format_size;
use crate::{ffmpeg, library, timestamp, trim::TrimRange};

pub const PANEL_WIDTH: f32 = 320.;

/// Which time field an edit came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TimeField {
    Start,
    End,
}

/// The export panel's widgets. Their values belong to the selected clip and
/// are swapped whenever the selection changes.
pub struct Editor {
    name: Entity<InputState>,
    start: Entity<InputState>,
    end: Entity<InputState>,
    folder: Entity<SelectState<Vec<SharedString>>>,
    /// Why the typed times cannot be used, shown under the fields.
    time_error: Option<SharedString>,
    /// Why the last export attempt was refused, shown under the button.
    pub(super) export_error: Option<SharedString>,
    _subscriptions: Vec<Subscription>,
}

impl Editor {
    pub fn new(window: &mut Window, cx: &mut Context<PlicApp>) -> Self {
        let name = cx.new(|cx| InputState::new(window, cx).placeholder("Clip name"));
        let start = cx.new(|cx| InputState::new(window, cx).placeholder("0:00"));
        let end = cx.new(|cx| InputState::new(window, cx).placeholder("End of video"));
        let folder = cx.new(|cx| SelectState::new(Vec::<SharedString>::new(), None, window, cx));

        let subscriptions = vec![
            cx.subscribe_in(&name, window, PlicApp::on_name_event),
            cx.subscribe_in(&start, window, |this, _, event, window, cx| {
                this.on_time_event(TimeField::Start, event, window, cx)
            }),
            cx.subscribe_in(&end, window, |this, _, event, window, cx| {
                this.on_time_event(TimeField::End, event, window, cx)
            }),
            cx.subscribe_in(&folder, window, PlicApp::on_folder_event),
        ];

        Self {
            name,
            start,
            end,
            folder,
            time_error: None,
            export_error: None,
            _subscriptions: subscriptions,
        }
    }

    /// The output name as typed, without surrounding whitespace.
    pub fn name(&self, cx: &App) -> String {
        self.name.read(cx).value().trim().to_owned()
    }
}

impl PlicApp {
    /// Fills the panel with the selected clip's draft.
    pub(super) fn sync_editor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (name, folder) = match self.current() {
            Some(entry) => (
                entry.name.clone(),
                self.destination(entry).map(display_name),
            ),
            None => (String::new(), None),
        };
        self.editor.time_error = None;
        self.editor.export_error = None;
        self.editor
            .name
            .update(cx, |input, cx| input.set_value(name, window, cx));
        self.sync_time_fields(window, cx);
        self.editor.folder.update(cx, |select, cx| match &folder {
            Some(folder) => select.set_selected_value(folder, window, cx),
            None => select.set_selected_index(None, window, cx),
        });
    }

    /// Rewrites the start and end fields from the selected clip's range.
    pub(super) fn sync_time_fields(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let trim = self.current().map(|entry| entry.trim).unwrap_or_default();
        let start = timestamp::format(trim.start);
        let end = trim.end.map(timestamp::format).unwrap_or_default();
        self.editor
            .start
            .update(cx, |input, cx| input.set_value(start, window, cx));
        self.editor
            .end
            .update(cx, |input, cx| input.set_value(end, window, cx));
    }

    /// Replaces the destination choices after the folder list changed.
    pub(super) fn sync_folder_choices(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let names: Vec<SharedString> = self.folders.iter().map(|path| display_name(path)).collect();
        self.editor
            .folder
            .update(cx, |select, cx| select.set_items(names, window, cx));
    }

    fn on_name_event(
        &mut self,
        input: &Entity<InputState>,
        event: &InputEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            InputEvent::Change => {
                let name = input.read(cx).value().to_string();
                if let Some(entry) = self.current_mut() {
                    entry.name = name;
                }
                self.editor.export_error = None;
                cx.notify();
            }
            // Ctrl+Enter arrives here too, but the ExportClip binding handles it.
            InputEvent::PressEnter {
                secondary: false, ..
            } => self.export_current(window, cx),
            _ => {}
        }
    }

    fn on_time_event(
        &mut self,
        field: TimeField,
        event: &InputEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            InputEvent::Change => self.apply_typed_times(field, cx),
            // Show the canonical form once the user is done typing.
            InputEvent::Blur if self.editor.time_error.is_none() => {
                self.sync_time_fields(window, cx)
            }
            // Ctrl+Enter arrives here too, but the ExportClip binding handles it.
            InputEvent::PressEnter {
                secondary: false, ..
            } => self.export_current(window, cx),
            _ => {}
        }
    }

    /// Parses both time fields and, if they form a valid range, applies it.
    fn apply_typed_times(&mut self, field: TimeField, cx: &mut Context<Self>) {
        let start_text = self.editor.start.read(cx).value();
        let end_text = self.editor.end.read(cx).value();
        let Some(duration_and_trim) = self
            .current()
            .map(|entry| (entry.info().and_then(|info| info.duration), entry.trim))
        else {
            return;
        };
        let (duration, current) = duration_and_trim;

        let parsed = match field {
            TimeField::Start => timestamp::parse(&start_text)
                .map(|start| TrimRange::new(start, current.end))
                .ok_or(SharedString::from(
                    "Start must look like 1:30, 0:07.5 or 1:02:03",
                )),
            TimeField::End if end_text.trim().is_empty() => Ok(TrimRange::new(current.start, None)),
            TimeField::End => timestamp::parse(&end_text)
                .map(|end| TrimRange::new(current.start, Some(end)))
                .ok_or(SharedString::from(
                    "End must look like 1:30, 0:07.5 or 1:02:03, or be empty",
                )),
        };
        let trim = parsed.and_then(|trim| {
            trim.validate(duration)
                .map(|()| trim)
                .map_err(|error| error.to_string().into())
        });

        match trim {
            Ok(trim) => {
                self.editor.time_error = None;
                self.editor.export_error = None;
                if let Some(entry) = self.current_mut() {
                    entry.trim = trim;
                }
                self.player
                    .update(cx, |player, cx| player.set_trim(trim, cx));
            }
            Err(message) => self.editor.time_error = Some(message),
        }
        cx.notify();
    }

    fn on_folder_event(
        &mut self,
        _: &Entity<SelectState<Vec<SharedString>>>,
        event: &SelectEvent<Vec<SharedString>>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let SelectEvent::Confirm(Some(name)) = event else {
            return;
        };
        let Some(folder) = self
            .folders
            .iter()
            .find(|path| display_name(path) == *name)
            .cloned()
        else {
            return;
        };
        self.last_folder = Some(folder.clone());
        if let Some(entry) = self.current_mut() {
            entry.folder = Some(folder);
        }
        self.editor.export_error = None;
        cx.notify();
    }

    /// Where the clip will be exported: its own choice, else the folder used
    /// last, else the first subfolder.
    pub(super) fn destination<'a>(&'a self, entry: &'a ClipEntry) -> Option<&'a Path> {
        let known = |path: &&Path| self.folders.iter().any(|folder| folder == path);
        entry
            .folder
            .as_deref()
            .filter(known)
            .or(self.last_folder.as_deref().filter(known))
            .or(self.folders.first().map(|path| path.as_path()))
    }

    /// Sets the start or end field to the playhead.
    fn time_from_playhead(
        &mut self,
        field: TimeField,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.player.update(cx, |player, cx| match field {
            TimeField::Start => player.mark_in(cx),
            TimeField::End => player.mark_out(cx),
        });
        self.sync_time_fields(window, cx);
    }

    fn prompt_new_folder(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.folder.is_none() {
            return;
        }
        let input = cx.new(|cx| InputState::new(window, cx).placeholder("Folder name"));
        cx.subscribe_in(
            &input,
            window,
            |this, input, event: &InputEvent, window, cx| {
                if matches!(event, InputEvent::PressEnter { .. }) {
                    let name = input.read(cx).value().to_string();
                    if this.create_folder(&name, window, cx) {
                        window.close_dialog(cx);
                    }
                }
            },
        )
        .detach();
        // Focus once the dialog is up, or opening it would take focus back.
        let focus = input.read(cx).focus_handle(cx);
        window.defer(cx, move |window, cx| focus.focus(window, cx));

        let this = cx.entity().downgrade();
        window.open_dialog(cx, move |dialog, _, cx| {
            let (this, input) = (this.clone(), input.clone());
            dialog
                .title("New destination folder")
                .w(px(380.))
                .child(
                    v_flex()
                        .gap_2()
                        .child(
                            div()
                                .text_sm()
                                .text_color(cx.theme().muted_foreground)
                                .child("Exported clips can be saved into any subfolder of the clips folder."),
                        )
                        .child(Input::new(&input)),
                )
                .footer(
                    h_flex()
                        .w_full()
                        .justify_end()
                        .gap_2()
                        .child(
                            Button::new("new-folder-cancel")
                                .outline()
                                .label("Cancel")
                                .on_click(|_, window, cx| window.close_dialog(cx)),
                        )
                        .child(
                            Button::new("new-folder-create")
                                .primary()
                                .label("Create")
                                .on_click(move |_, window, cx| {
                                    let name = input.read(cx).value().to_string();
                                    let created = this
                                        .update(cx, |this, cx| this.create_folder(&name, window, cx))
                                        .unwrap_or(false);
                                    if created {
                                        window.close_dialog(cx);
                                    }
                                }),
                        ),
                )
        });
    }

    /// Creates a destination folder and makes it the selected clip's
    /// destination. Problems are reported as notifications.
    fn create_folder(&mut self, name: &str, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let Some(root) = &self.folder else {
            return false;
        };
        let name = name.trim();
        if let Err(error) = library::validate_file_stem(name) {
            window.push_notification(Notification::error(error.to_string()), cx);
            return false;
        }
        let path = root.join(name);
        if let Err(error) = std::fs::create_dir(&path) {
            window.push_notification(
                Notification::error(format!("Could not create {name}: {error}")),
                cx,
            );
            return false;
        }

        self.folders.push(path.clone());
        self.folders
            .sort_by_cached_key(|folder| display_name(folder).to_lowercase());
        self.last_folder = Some(path.clone());
        if let Some(entry) = self.current_mut() {
            entry.folder = Some(path);
        }
        self.sync_folder_choices(window, cx);
        let name = SharedString::from(name.to_owned());
        self.editor.folder.update(cx, |select, cx| {
            select.set_selected_value(&name, window, cx)
        });
        self.editor.export_error = None;
        cx.notify();
        true
    }

    pub(super) fn render_export_panel(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let entry = self.current();
        let enabled = entry.is_some_and(|entry| entry.info().is_some());
        let exporting = entry.is_some_and(|entry| entry.status == ClipStatus::Exporting);
        let duration = entry.and_then(|entry| entry.info()?.duration);
        let trim = entry.map(|entry| entry.trim).unwrap_or_default();
        let destination = entry
            .and_then(|entry| self.destination(entry))
            .map(display_name);
        let output_name = self.editor.name(cx);
        let has_folders = !self.folders.is_empty();

        let label = |text: &'static str| {
            div()
                .text_xs()
                .font_weight(FontWeight::MEDIUM)
                .text_color(theme.muted_foreground)
                .child(text)
        };

        let time_field = |id: &'static str,
                          field: TimeField,
                          state: &Entity<InputState>,
                          tooltip: &'static str,
                          cx: &mut Context<Self>| {
            h_flex()
                .gap_1()
                .child(
                    div()
                        .flex_1()
                        .child(Input::new(state).small().disabled(!enabled)),
                )
                .child(
                    Button::new(id)
                        .ghost()
                        .small()
                        .icon(match field {
                            TimeField::Start => IconName::ArrowRightToLine,
                            TimeField::End => IconName::ArrowLeftToLine,
                        })
                        .tooltip(tooltip)
                        .disabled(!enabled)
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.time_from_playhead(field, window, cx)
                        })),
                )
        };

        let length = trim.length(duration);
        let estimate = entry
            .zip(duration)
            .zip(length)
            .map(|((entry, duration), length)| {
                format_size(ffmpeg::estimated_size(entry.file.size, duration, length))
            });
        // One compact line: kept length, out of how much, and the likely size.
        let summary = h_flex()
            .h(px(36.))
            .px_3()
            .gap_2()
            .rounded(theme.radius)
            .bg(theme.muted)
            .text_sm()
            .child(
                Icon::new(IconName::Timer)
                    .small()
                    .text_color(theme.muted_foreground),
            )
            .child(
                div()
                    .font_family(theme.mono_font_family.clone())
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(length.map_or("--:--".into(), timestamp::format_precise)),
            )
            .when_some(duration, |row, duration| {
                row.child(div().text_xs().text_color(theme.muted_foreground).child(
                    if trim.is_full(Some(duration)) {
                        "whole clip".to_string()
                    } else {
                        format!("of {}", timestamp::format_coarse(duration))
                    },
                ))
            })
            .child(div().flex_1())
            .when_some(estimate, |row, estimate| {
                row.child(
                    div()
                        .id("estimated-size")
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(format!("≈ {estimate}"))
                        .tooltip(|window, cx| {
                            Tooltip::new("Estimated from the original's bitrate").build(window, cx)
                        }),
                )
            });

        let output_preview = match (&destination, output_name.is_empty()) {
            (Some(folder), false) => Some(format!(
                "{folder}{}{output_name}.mp4",
                std::path::MAIN_SEPARATOR
            )),
            _ => None,
        };

        v_flex()
            .w(px(PANEL_WIDTH))
            .flex_none()
            .h_full()
            .bg(theme.sidebar)
            .border_l_1()
            .border_color(theme.sidebar_border)
            .child(
                h_flex().h(px(40.)).px_4().flex_none().child(
                    div()
                        .text_xs()
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(theme.muted_foreground)
                        .child("EXPORT"),
                ),
            )
            .when(entry.is_some(), |panel| {
                panel.child(
                    v_flex()
                        .px_4()
                        .gap_4()
                        .child(
                            v_flex().gap_1p5().child(label("File name")).child(
                                Input::new(&self.editor.name)
                                    .small()
                                    .disabled(!enabled)
                                    .suffix(
                                        div()
                                            .text_xs()
                                            .text_color(theme.muted_foreground)
                                            .child(".mp4"),
                                    ),
                            ),
                        )
                        .child(
                            v_flex()
                                .gap_1p5()
                                .child(
                                    h_flex()
                                        .gap_2()
                                        .child(
                                            v_flex()
                                                .flex_1()
                                                .gap_1p5()
                                                .child(label("Start"))
                                                .child(time_field(
                                                    "start-from-playhead",
                                                    TimeField::Start,
                                                    &self.editor.start,
                                                    "Start at the playhead (I)",
                                                    cx,
                                                )),
                                        )
                                        .child(
                                            v_flex().flex_1().gap_1p5().child(label("End")).child(
                                                time_field(
                                                    "end-from-playhead",
                                                    TimeField::End,
                                                    &self.editor.end,
                                                    "End at the playhead (O)",
                                                    cx,
                                                ),
                                            ),
                                        ),
                                )
                                .when_some(self.editor.time_error.clone(), |column, error| {
                                    column.child(
                                        div().text_xs().text_color(theme.danger).child(error),
                                    )
                                }),
                        )
                        .child(summary)
                        .child(
                            v_flex()
                                .gap_1p5()
                                .child(label("Save to"))
                                .child(
                                    h_flex()
                                        .gap_1()
                                        .child(
                                            div().flex_1().min_w_0().child(
                                                Select::new(&self.editor.folder)
                                                    .small()
                                                    .icon(IconName::Folder)
                                                    .placeholder(if has_folders {
                                                        "Choose a folder"
                                                    } else {
                                                        "No subfolders yet"
                                                    })
                                                    .disabled(!enabled || !has_folders),
                                            ),
                                        )
                                        .child(
                                            Button::new("new-folder")
                                                .ghost()
                                                .small()
                                                .icon(IconName::FolderPlus)
                                                .tooltip("New destination folder")
                                                .disabled(self.folder.is_none())
                                                .on_click(cx.listener(|this, _, window, cx| {
                                                    this.prompt_new_folder(window, cx)
                                                })),
                                        ),
                                )
                                .when_some(output_preview, |column, preview| {
                                    column.child(
                                        div()
                                            .text_xs()
                                            .text_color(theme.muted_foreground)
                                            .truncate()
                                            .child(preview),
                                    )
                                }),
                        )
                        .child(
                            v_flex()
                                .gap_2()
                                .child(
                                    Button::new("export")
                                        .primary()
                                        .w_full()
                                        .icon(IconName::Scissors)
                                        .label(if exporting {
                                            "Exporting…"
                                        } else {
                                            "Export clip"
                                        })
                                        .loading(exporting)
                                        .disabled(!enabled || exporting)
                                        .on_click(cx.listener(|this, _, window, cx| {
                                            this.export_current(window, cx)
                                        })),
                                )
                                .child(
                                    h_flex()
                                        .justify_center()
                                        .gap_1()
                                        .text_xs()
                                        .text_color(theme.muted_foreground)
                                        .child("or press")
                                        .child(Kbd::new(
                                            Keystroke::parse("ctrl-enter")
                                                .expect("valid keystroke"),
                                        )),
                                )
                                .when_some(self.editor.export_error.clone(), |column, error| {
                                    column.child(
                                        div().text_xs().text_color(theme.danger).child(error),
                                    )
                                }),
                        ),
                )
            })
            .child(div().flex_1())
            .child(self.render_queue(cx))
    }
}
