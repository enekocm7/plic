//! The settings dialog, opened with Ctrl+, or the gear in the title bar.
//! Settings are split into sections picked from a sidebar; each setting
//! and each section can be put back to its default.

use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Icon, Selectable as _, Sizable as _, WindowExt as _,
    h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use std::collections::BTreeMap;

use super::{
    app::PlicApp,
    keymap::{self, Command},
    shortcuts,
};
use crate::{
    ffmpeg::Quality,
    library,
    settings::{DEFAULT_ARCHIVE_FOLDER, DEFAULT_SEEK_STEP_SECS, Settings},
};

/// A page of the settings dialog.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Section {
    Playback,
    Library,
    Export,
    Ffmpeg,
    Keyboard,
}

impl Section {
    const ALL: [Self; 5] = [
        Self::Playback,
        Self::Library,
        Self::Export,
        Self::Ffmpeg,
        Self::Keyboard,
    ];

    fn title(self) -> &'static str {
        match self {
            Self::Playback => "Playback",
            Self::Library => "Library",
            Self::Export => "Export",
            Self::Ffmpeg => "ffmpeg",
            Self::Keyboard => "Keyboard",
        }
    }

    fn description(self) -> &'static str {
        match self {
            Self::Playback => "How the player moves through a clip.",
            Self::Library => "Where clips go once you are done with them.",
            Self::Export => "How exported files are named and encoded.",
            Self::Ffmpeg => "The tools plic uses to read and write video.",
            Self::Keyboard => "The keys for each command. Changes apply when you save.",
        }
    }

    fn icon(self) -> IconName {
        match self {
            Self::Playback => IconName::Play,
            Self::Library => IconName::Folder,
            Self::Export => IconName::Download,
            Self::Ffmpeg => IconName::Clapperboard,
            Self::Keyboard => IconName::Keyboard,
        }
    }
}

/// A text setting: its field and the value it resets to.
struct Field {
    input: Entity<InputState>,
    default: &'static str,
}

impl Field {
    fn changed(&self, cx: &App) -> bool {
        self.input.read(cx).value().trim() != self.default
    }

    fn reset(&self, window: &mut Window, cx: &mut App) {
        let default = self.default;
        self.input
            .update(cx, |input, cx| input.set_value(default, window, cx));
    }

    fn value(&self, cx: &App) -> String {
        self.input.read(cx).value().to_string()
    }
}

/// A command's keys as typed in its field.
struct KeyField {
    command: &'static Command,
    input: Entity<InputState>,
}

impl KeyField {
    fn typed(&self, cx: &App) -> Vec<String> {
        keymap::split_keys(&self.input.read(cx).value())
    }

    fn changed(&self, cx: &App) -> bool {
        self.typed(cx) != self.command.defaults
    }

    fn reset(&self, window: &mut Window, cx: &mut App) {
        let defaults = self.command.defaults.join(", ");
        self.input
            .update(cx, |input, cx| input.set_value(defaults, window, cx));
    }
}

/// What is wrong with a command's keys, if anything.
fn key_problem(field: &KeyField, all: &[(&'static Command, Vec<String>)]) -> Option<String> {
    let typed = all
        .iter()
        .find(|(command, _)| command.id == field.command.id)
        .map(|(_, keys)| keys.as_slice())
        .unwrap_or_default();
    for key in typed {
        if keymap::parse_keys(key).is_none() {
            return Some(format!("\u{201c}{key}\u{201d} is not a key"));
        }
        let others = all
            .iter()
            .map(|(command, keys)| (*command, keys.as_slice()));
        if let Some(other) = keymap::conflict(field.command, key, others) {
            return Some(format!("{key} is also used by \u{201c}{other}\u{201d}"));
        }
    }
    None
}

pub struct SettingsView {
    section: Section,
    seek: Field,
    archive: Field,
    destination: Field,
    template: Field,
    ffmpeg: Field,
    quality: Quality,
    keys: Vec<KeyField>,
    key_search: Entity<InputState>,
    app: WeakEntity<PlicApp>,
    _subscriptions: Vec<Subscription>,
}

impl SettingsView {
    fn new(
        section: Section,
        app: WeakEntity<PlicApp>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let current = crate::settings::get();
        let mut subscriptions = Vec::new();
        let mut watch = |input: Entity<InputState>, cx: &mut Context<Self>| {
            subscriptions.push(cx.subscribe(&input, |_, _, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    cx.notify();
                }
            }));
            input
        };
        let mut field =
            |value: String, default: &'static str, placeholder: &str, cx: &mut Context<Self>| {
                let placeholder = SharedString::from(placeholder.to_owned());
                let input = cx.new(|cx| {
                    InputState::new(window, cx)
                        .default_value(value)
                        .placeholder(placeholder)
                });
                Field {
                    input: watch(input, cx),
                    default,
                }
            };
        static SEEK_DEFAULT: std::sync::OnceLock<String> = std::sync::OnceLock::new();
        let seek_default = SEEK_DEFAULT.get_or_init(|| DEFAULT_SEEK_STEP_SECS.to_string());
        let seek = field(
            current.seek_step_secs.to_string(),
            seek_default,
            seek_default,
            cx,
        );
        let archive = field(
            current.archive_folder.clone(),
            DEFAULT_ARCHIVE_FOLDER,
            DEFAULT_ARCHIVE_FOLDER,
            cx,
        );
        let destination = field(
            current.default_destination.clone(),
            "",
            "The folder used last",
            cx,
        );
        let template = field(current.name_template.clone(), "", "{name}", cx);
        let ffmpeg = field(current.ffmpeg_folder.clone(), "", "plic's own copy", cx);
        let keys = keymap::COMMANDS
            .iter()
            .map(|command| {
                let value = keymap::keys_for(command, &current.key_bindings).join(", ");
                let input = cx.new(|cx| {
                    InputState::new(window, cx)
                        .default_value(value)
                        .placeholder("Not bound")
                });
                KeyField {
                    command,
                    input: watch(input, cx),
                }
            })
            .collect();
        let key_search =
            cx.new(|cx| InputState::new(window, cx).placeholder("Search commands or keys"));
        let key_search = watch(key_search, cx);
        Self {
            section,
            seek,
            archive,
            destination,
            template,
            ffmpeg,
            quality: current.export_quality,
            keys,
            key_search,
            app,
            _subscriptions: subscriptions,
        }
    }

    fn fields(&self, section: Section) -> Vec<&Field> {
        match section {
            Section::Playback => vec![&self.seek],
            Section::Library => vec![&self.archive, &self.destination],
            Section::Export => vec![&self.template],
            Section::Ffmpeg => vec![&self.ffmpeg],
            Section::Keyboard => vec![],
        }
    }

    fn section_changed(&self, section: Section, cx: &App) -> bool {
        self.fields(section).iter().any(|field| field.changed(cx))
            || (section == Section::Export && self.quality != Quality::default())
            || (section == Section::Keyboard && self.keys.iter().any(|key| key.changed(cx)))
    }

    fn reset_section(&mut self, section: Section, window: &mut Window, cx: &mut Context<Self>) {
        for field in self.fields(section) {
            field.reset(window, cx);
        }
        match section {
            Section::Export => self.quality = Quality::default(),
            Section::Keyboard => {
                for key in &self.keys {
                    key.reset(window, cx);
                }
            }
            _ => {}
        }
        cx.notify();
    }

    fn typed_keys(&self, cx: &App) -> Vec<(&'static Command, Vec<String>)> {
        self.keys
            .iter()
            .map(|key| (key.command, key.typed(cx)))
            .collect()
    }

    /// Checks and saves the settings. Returns whether they were saved.
    fn save(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let seek_step_secs = self
            .seek
            .value(cx)
            .trim()
            .parse()
            .unwrap_or(DEFAULT_SEEK_STEP_SECS);
        let archive_folder = self.archive.value(cx);
        if !archive_folder.trim().is_empty()
            && let Err(error) = library::validate_file_stem(archive_folder.trim())
        {
            self.section = Section::Library;
            cx.notify();
            window.push_notification(Notification::error(format!("Archive folder: {error}")), cx);
            return false;
        }
        let typed = self.typed_keys(cx);
        if let Some((key, problem)) = self
            .keys
            .iter()
            .find_map(|key| key_problem(key, &typed).map(|problem| (key, problem)))
        {
            self.section = Section::Keyboard;
            cx.notify();
            window.push_notification(
                Notification::error(format!("{}: {problem}", key.command.description)),
                cx,
            );
            return false;
        }
        let mut key_bindings = BTreeMap::new();
        for (command, keys) in typed {
            // Only changes are saved, so new defaults still reach everyone
            // else.
            if keys != command.defaults {
                key_bindings.insert(command.id.to_owned(), keys);
            }
        }
        let settings = Settings {
            seek_step_secs,
            archive_folder,
            default_destination: self.destination.value(cx),
            name_template: self.template.value(cx),
            ffmpeg_folder: self.ffmpeg.value(cx),
            export_quality: self.quality,
            key_bindings,
        };
        if let Err(error) = crate::settings::update(settings) {
            window.push_notification(
                Notification::error(format!("Could not save settings: {error:#}")),
                cx,
            );
        }
        keymap::apply(cx);
        // Clearing the folder may call for plic's own copy.
        self.app
            .update(cx, |app, cx| {
                app.set_up_ffmpeg(window, cx);
                cx.notify()
            })
            .ok();
        true
    }

    fn sidebar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let (muted, active, hover, foreground) = (
            theme.muted_foreground,
            theme.accent,
            theme.list_hover,
            theme.foreground,
        );
        let radius = theme.radius;
        v_flex()
            .w(px(168.))
            .flex_none()
            .gap_0p5()
            .pr_3()
            .border_r_1()
            .border_color(theme.border)
            .children(Section::ALL.map(|section| {
                let selected = section == self.section;
                let changed = self.section_changed(section, cx);
                h_flex()
                    .id(SharedString::from(format!(
                        "settings-nav-{}",
                        section.title()
                    )))
                    .gap_2()
                    .px_2()
                    .py_1p5()
                    .rounded(radius)
                    .text_sm()
                    .cursor_pointer()
                    .when(selected, |item| {
                        item.bg(active).font_weight(FontWeight::MEDIUM)
                    })
                    .when(!selected, |item| {
                        item.text_color(muted).hover(|item| item.bg(hover))
                    })
                    .child(Icon::new(section.icon()).small().text_color(if selected {
                        foreground
                    } else {
                        muted
                    }))
                    .child(div().flex_1().child(section.title()))
                    .when(changed, |item| {
                        // A dot marks sections that differ from the defaults.
                        item.child(div().size(px(6.)).rounded_full().bg(muted))
                    })
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.section = section;
                        cx.notify();
                    }))
            }))
    }

    fn reset_button(
        id: impl Into<SharedString>,
        changed: bool,
        on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> AnyElement {
        // A fixed slot, so controls line up whether or not it is shown.
        h_flex()
            .w(px(26.))
            .flex_none()
            .justify_center()
            .when(changed, |slot| {
                slot.child(
                    Button::new(ElementId::Name(id.into()))
                        .ghost()
                        .xsmall()
                        .icon(IconName::RotateCcw)
                        .tooltip("Reset to default")
                        .on_click(on_click),
                )
            })
            .into_any_element()
    }

    /// A setting: its label and hint on the left, its control on the right.
    fn row(
        label: &'static str,
        hint: &'static str,
        control: impl IntoElement,
        reset: AnyElement,
        cx: &App,
    ) -> impl IntoElement {
        let theme = cx.theme();
        h_flex()
            .items_start()
            .gap_4()
            .py_3()
            .border_b_1()
            .border_color(theme.border)
            .child(
                v_flex()
                    .w(px(220.))
                    .flex_none()
                    .gap_0p5()
                    .child(div().text_sm().font_weight(FontWeight::MEDIUM).child(label))
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(hint),
                    ),
            )
            .child(
                h_flex()
                    .flex_1()
                    .min_w_0()
                    .gap_1()
                    .child(div().flex_1().min_w_0().child(control))
                    .child(reset),
            )
    }

    fn text_row(
        &self,
        id: &'static str,
        label: &'static str,
        hint: &'static str,
        field: &Field,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let input = field.input.clone();
        let default = field.default;
        let reset = Self::reset_button(id, field.changed(cx), move |_, window, cx| {
            input.update(cx, |input, cx| input.set_value(default, window, cx));
        });
        Self::row(label, hint, Input::new(&field.input).small(), reset, cx)
    }

    fn quality_row(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let buttons = h_flex()
            .p_0p5()
            .gap_0p5()
            .rounded(theme.radius)
            .bg(theme.muted)
            .children(Quality::ALL.map(|choice| {
                Button::new(SharedString::from(format!(
                    "settings-quality-{}",
                    choice.label()
                )))
                .ghost()
                .small()
                .flex_1()
                .label(choice.label())
                .selected(self.quality == choice)
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.quality = choice;
                    cx.notify();
                }))
            }));
        let reset = Self::reset_button(
            "settings-quality-reset",
            self.quality != Quality::default(),
            cx.listener(|this, _, _, cx| {
                this.quality = Quality::default();
                cx.notify();
            }),
        );
        Self::row(
            "Quality",
            "How hard re-encoded exports are compressed. Small makes smaller files, High keeps more detail.",
            buttons,
            reset,
            cx,
        )
    }

    fn keyboard(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let query = self.key_search.read(cx).value().trim().to_lowercase();
        let typed = self.typed_keys(cx);
        let theme = cx.theme();
        let (muted, danger, hover) = (theme.muted_foreground, theme.danger, theme.list_hover);
        let mut cards = Vec::new();
        for group in keymap::GROUPS {
            let rows: Vec<AnyElement> = self
                .keys
                .iter()
                .zip(&typed)
                .filter(|(key, (_, keys))| {
                    key.command.group == group && shortcuts::matches(&query, key.command, keys)
                })
                .map(|(key, _)| {
                    let problem = key_problem(key, &typed);
                    let fixed: Vec<&str> = keymap::fixed_keys(key.command).collect();
                    let input = key.input.clone();
                    let defaults = key.command.defaults.join(", ");
                    let reset = Self::reset_button(
                        format!("settings-key-reset-{}", key.command.id),
                        key.changed(cx),
                        move |_, window, cx| {
                            input.update(cx, |input, cx| {
                                input.set_value(defaults.clone(), window, cx)
                            });
                        },
                    );
                    v_flex()
                        .px_3()
                        .py_1()
                        .hover(|row| row.bg(hover))
                        .child(
                            h_flex()
                                .gap_2()
                                .child(
                                    v_flex()
                                        .flex_1()
                                        .min_w_0()
                                        .child(
                                            div()
                                                .text_sm()
                                                .truncate()
                                                .child(key.command.description),
                                        )
                                        .when(!fixed.is_empty(), |label| {
                                            label.child(
                                                h_flex()
                                                    .gap_1()
                                                    .text_xs()
                                                    .text_color(muted)
                                                    .child("Also")
                                                    .child(shortcuts::keycaps(&[], &fixed, muted))
                                                    .child("(built in)"),
                                            )
                                        }),
                                )
                                .child(
                                    div()
                                        .w(px(180.))
                                        .flex_none()
                                        .child(Input::new(&key.input).small()),
                                )
                                .child(reset),
                        )
                        .when_some(problem, |row, problem| {
                            row.child(
                                h_flex()
                                    .gap_1()
                                    .pt_1()
                                    .text_xs()
                                    .text_color(danger)
                                    .child(Icon::new(IconName::CircleAlert).xsmall())
                                    .child(problem),
                            )
                        })
                        .into_any_element()
                })
                .collect();
            if !rows.is_empty() {
                cards.push(shortcuts::group_card(group, rows.len(), rows, cx));
            }
        }
        let empty = cards.is_empty();
        v_flex()
            .gap_3()
            .when(empty, |list| {
                list.child(
                    div()
                        .py_8()
                        .text_sm()
                        .text_color(muted)
                        .text_center()
                        .child(format!("No commands match \u{201c}{query}\u{201d}")),
                )
            })
            .children(cards)
    }
}

impl SettingsView {
    /// The search box and help above the key list, kept in view while the
    /// list scrolls.
    fn keyboard_header(&self, cx: &App) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        v_flex()
            .gap_2()
            .pb_3()
            .pr_2()
            .child(
                Input::new(&self.key_search)
                    .small()
                    .prefix(Icon::new(IconName::Search).small().text_color(muted))
                    .cleanable(true),
            )
            .child(
                div().text_xs().text_color(muted).child(
                    "Separate alternative keys with commas, e.g. space, k. Use ctrl-, shift- and alt- for modifiers, and a space for sequences like g g. Leave a field empty to unbind it.",
                ),
            )
    }
}

impl Render for SettingsView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let section = self.section;
        let theme = cx.theme();
        let muted = theme.muted_foreground;
        let height = (window.viewport_size().height - px(230.)).clamp(px(300.), px(540.));
        let body: AnyElement = match section {
            Section::Playback => v_flex()
                .child(self.text_row(
                    "settings-seek-reset",
                    "Seek step",
                    "Seconds the left and right arrow keys move the playhead. From 1 to 600.",
                    &self.seek,
                    cx,
                ))
                .into_any_element(),
            Section::Library => v_flex()
                .child(self.text_row(
                    "settings-archive-reset",
                    "Archive folder",
                    "Exported originals move to this folder inside the clips folder.",
                    &self.archive,
                    cx,
                ))
                .child(self.text_row(
                    "settings-destination-reset",
                    "Default destination",
                    "Subfolder, or \"Clips folder\", that exports go to when you have not chosen one. Empty uses the one used last.",
                    &self.destination,
                    cx,
                ))
                .into_any_element(),
            Section::Export => v_flex()
                .child(self.text_row(
                    "settings-template-reset",
                    "Name template",
                    "Pre-fills export names. Use {name}, {folder}, {date} and {n}, which counts up past names already used. Empty keeps the clip's name.",
                    &self.template,
                    cx,
                ))
                .child(self.quality_row(cx))
                .into_any_element(),
            Section::Ffmpeg => v_flex()
                .child(self.text_row(
                    "settings-ffmpeg-reset",
                    "ffmpeg folder",
                    "A folder with your own ffmpeg and ffprobe, used instead of the copy plic downloads. Empty uses plic's copy.",
                    &self.ffmpeg,
                    cx,
                ))
                .into_any_element(),
            Section::Keyboard => self.keyboard(cx).into_any_element(),
        };
        let changed = self.section_changed(section, cx);
        h_flex()
            .h(height)
            .items_start()
            .child(self.sidebar(cx))
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .pl_5()
                    .child(
                        h_flex()
                            .gap_3()
                            .pb_3()
                            .child(
                                v_flex()
                                    .flex_1()
                                    .min_w_0()
                                    .gap_0p5()
                                    .child(
                                        div()
                                            .text_base()
                                            .font_weight(FontWeight::SEMIBOLD)
                                            .child(section.title()),
                                    )
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(muted)
                                            .child(section.description()),
                                    ),
                            )
                            .child(
                                Button::new("settings-reset-section")
                                    .ghost()
                                    .small()
                                    .icon(IconName::RotateCcw)
                                    .label("Reset section")
                                    .disabled(!changed)
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        this.reset_section(section, window, cx)
                                    })),
                            ),
                    )
                    .when(section == Section::Keyboard, |pane| {
                        pane.child(self.keyboard_header(cx))
                    })
                    .child(
                        div()
                            .id("settings-body")
                            .flex_1()
                            .min_h_0()
                            .overflow_y_scroll()
                            .pr_2()
                            .child(body),
                    ),
            )
    }
}

impl PlicApp {
    pub(super) fn open_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.open_settings_at(Section::Playback, window, cx);
    }

    pub(super) fn open_settings_at(
        &mut self,
        section: Section,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let app = cx.entity().downgrade();
        let view = cx.new(|cx| SettingsView::new(section, app, window, cx));
        window.open_dialog(cx, move |dialog, _, _| {
            let save = view.clone();
            dialog
                .title("Settings")
                .w(px(780.))
                .child(view.clone())
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
                                    if save.update(cx, |view, cx| view.save(window, cx)) {
                                        window.close_dialog(cx);
                                    }
                                }),
                        ),
                )
        });
    }
}
