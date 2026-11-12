//! The export panel: output name, trim range and destination of the
//! selected clip.
//!
//! The text fields and the timeline edit the same range, the clip's active
//! segment. Typing a valid time moves the handles; dragging a handle
//! rewrites the text. With several segments, a list picks the active one and
//! a switch decides whether they are joined or saved as separate files, in
//! which case each part can be given its own name.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    time::Duration,
};

use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::kbd::Kbd;
use gpui_kit::component::notification::Notification;
use gpui_kit::component::select::{SearchableVec, Select, SelectEvent, SelectState};
use gpui_kit::component::slider::{Slider, SliderEvent, SliderState};
use gpui_kit::component::switch::Switch;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Icon, Selectable as _, Sizable as _, WindowExt as _,
    h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use super::app::{ClipEntry, ClipStatus, PlicApp, display_name};
use super::library_panel::format_size;
use super::{AddSegment, SplitAtPlayhead};
use crate::{
    ffmpeg::{self, Animation, Codec, CropAspect, ExportMode, Format, Quality, Transform},
    library, timestamp,
    trim::{self, TrimRange},
};

pub const PANEL_WIDTH: f32 = 320.;

/// Destination folder names, filtered as the user types.
type FolderList = SearchableVec<SharedString>;

/// Resolutions offered as caps, by the picture's shorter side.
const RESOLUTIONS: [u32; 3] = [1080, 720, 480];

/// Frame rates offered as caps.
const FRAME_RATE_CAPS: [u32; 2] = [60, 30];

/// Sizes offered as export targets, in megabytes: common upload limits.
const TARGET_SIZES_MB: [u64; 4] = [10, 25, 50, 100];

/// The gain slider's range, in decibels either way.
const MAX_GAIN_DB: f32 = 20.0;

/// Keyframe lookups kept before the cache is cleared; dragging a handle
/// asks for a new one every few frames.
const KEYFRAME_CACHE_LIMIT: usize = 256;

/// How far a lossless start may move before the panel points it out; less
/// than a frame is just rounding.
const MOVE_NOTICE: Duration = Duration::from_millis(5);

/// Where a lossless cut that is asked to start at some point really starts.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Keyframe {
    Found(Duration),
    Failed(SharedString),
}

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
    /// Name of the active segment when segments are saved separately.
    part_name: Entity<InputState>,
    start: Entity<InputState>,
    end: Entity<InputState>,
    folder: Entity<SelectState<FolderList>>,
    /// Where the crop sits, from the left or top edge (0) to the other (1).
    crop_position: Entity<SliderState>,
    /// Gain applied to the exported sound, in decibels.
    gain: Entity<SliderState>,
    /// Why the typed times cannot be used, shown under the fields.
    time_error: Option<SharedString>,
    /// Why the last export attempt was refused, shown under the button.
    pub(super) export_error: Option<SharedString>,
    /// How exports are written. Kept while moving between clips.
    pub(super) mode: ExportMode,
    /// Codec and quality of re-encoded exports.
    pub(super) video: ffmpeg::VideoEncoding,
    /// Resolution and frame rate caps of re-encoded exports.
    pub(super) limits: ffmpeg::OutputLimits,
    /// MP4, GIF or WebP.
    pub(super) format: Format,
    /// Width and frame rate of GIF and WebP exports.
    pub(super) animation: Animation,
    /// Whether several segments become one file or one file each.
    pub(super) join: bool,
    /// Keyframe at or before a clip's start, by clip and requested start.
    keyframes: HashMap<(PathBuf, Duration), Keyframe>,
    /// Whether a keyframe lookup is running. Only one runs at a time; the
    /// next render asks for whatever the start is by then.
    keyframe_pending: bool,
    _subscriptions: Vec<Subscription>,
}

impl Editor {
    pub fn new(window: &mut Window, cx: &mut Context<PlicApp>) -> Self {
        let name = cx.new(|cx| InputState::new(window, cx).placeholder("Clip name"));
        let part_name = cx.new(|cx| InputState::new(window, cx));
        let start = cx.new(|cx| InputState::new(window, cx).placeholder("0:00"));
        let end = cx.new(|cx| InputState::new(window, cx).placeholder("End of video"));
        // Typing while the list is open filters the folders.
        let folder = cx.new(|cx| {
            SelectState::new(
                FolderList::new(Vec::<SharedString>::new()),
                None,
                window,
                cx,
            )
            .searchable(true)
        });

        let crop_position = cx.new(|_| {
            SliderState::new()
                .min(0.0)
                .max(1.0)
                .step(0.01)
                .default_value(0.5)
        });

        let gain = cx.new(|_| {
            SliderState::new()
                .min(-MAX_GAIN_DB)
                .max(MAX_GAIN_DB)
                .step(0.5)
                .default_value(0.0)
        });

        let subscriptions = vec![
            cx.subscribe_in(&crop_position, window, PlicApp::on_crop_position_event),
            cx.subscribe_in(&gain, window, PlicApp::on_gain_event),
            cx.subscribe_in(&name, window, PlicApp::on_name_event),
            cx.subscribe_in(&part_name, window, PlicApp::on_part_name_event),
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
            part_name,
            start,
            end,
            folder,
            crop_position,
            gain,
            time_error: None,
            export_error: None,
            mode: ExportMode::default(),
            video: ffmpeg::VideoEncoding::default(),
            limits: ffmpeg::OutputLimits::default(),
            format: Format::default(),
            animation: Animation::default(),
            join: true,
            keyframes: HashMap::new(),
            keyframe_pending: false,
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
        self.apply_name_template();
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
        let position = self
            .current()
            .map_or(0.5, |entry| entry.transform.crop_position);
        self.editor
            .crop_position
            .update(cx, |slider, cx| slider.set_value(position, window, cx));
        let gain = self
            .current()
            .map_or(0.0, |entry| entry.audio_adjust.gain_db);
        self.editor
            .gain
            .update(cx, |slider, cx| slider.set_value(gain, window, cx));
        self.editor.folder.update(cx, |select, cx| match &folder {
            Some(folder) => select.set_selected_value(folder, window, cx),
            None => select.set_selected_index(None, window, cx),
        });
    }

    /// Rewrites the start, end and part name fields from the selected
    /// clip's active segment.
    pub(super) fn sync_time_fields(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (part_name, placeholder) = self
            .current()
            .map(|entry| {
                let segments = &entry.segments;
                let position = segments.position(segments.active_index());
                (
                    segments.active_name().to_owned(),
                    trim::default_part_name(&entry.name, position),
                )
            })
            .unwrap_or_default();
        self.editor.part_name.update(cx, |input, cx| {
            input.set_placeholder(placeholder, window, cx);
            if input.value() != part_name {
                input.set_value(part_name, window, cx);
            }
        });
        let trim = self.current().map(|entry| entry.trim()).unwrap_or_default();
        let start = timestamp::format(trim.start);
        let end = trim.end.map(timestamp::format).unwrap_or_default();
        self.editor
            .start
            .update(cx, |input, cx| input.set_value(start, window, cx));
        self.editor
            .end
            .update(cx, |input, cx| input.set_value(end, window, cx));
    }

    /// Names the selected clip from the name template, once.
    fn apply_name_template(&mut self) {
        let template = crate::settings::get().name_template;
        let Some(index) = self.selected.filter(|_| !template.is_empty()) else {
            return;
        };
        let entry = &self.clips[index];
        if entry.templated {
            return;
        }
        let folder = self.folder.as_deref().map(display_name);
        let context = crate::naming::TemplateContext {
            name: &entry.file.stem(),
            folder: folder.as_deref().unwrap_or_default(),
            recorded: entry.file.modified,
        };
        let destination = self.destination(entry);
        let extension = self.editor.format.extension();
        let name = crate::naming::expand(&template, &context, |candidate| {
            destination
                .is_some_and(|folder| folder.join(format!("{candidate}.{extension}")).exists())
                || self.clips.iter().enumerate().any(|(other, clip)| {
                    other != index && clip.templated && clip.name.eq_ignore_ascii_case(candidate)
                })
        });
        let entry = &mut self.clips[index];
        entry.templated = true;
        if library::validate_file_stem(&name).is_ok() {
            entry.name = name;
        }
    }

    /// Replaces the destination choices after the folder list changed.
    pub(super) fn sync_folder_choices(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let names = FolderList::new(
            self.folders
                .iter()
                .map(|path| display_name(path))
                .collect::<Vec<_>>(),
        );
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
                let Some(entry) = self.current_mut() else {
                    return;
                };
                entry.name = name;
                // Unnamed parts are called after the clip.
                let segments = &entry.segments;
                let placeholder = trim::default_part_name(
                    &entry.name,
                    segments.position(segments.active_index()),
                );
                self.editor.part_name.update(cx, |input, cx| {
                    input.set_placeholder(placeholder, window, cx)
                });
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

    fn on_part_name_event(
        &mut self,
        input: &Entity<InputState>,
        event: &InputEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            InputEvent::Change => {
                let name = input.read(cx).value().to_string();
                let Some(entry) = self.current_mut() else {
                    return;
                };
                if entry.segments.active_name() == name {
                    return;
                }
                entry.segments.set_active_name(name.clone());
                self.player
                    .update(cx, |player, _| player.set_segment_name(name));
                self.editor.export_error = None;
                cx.notify();
            }
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
            .map(|entry| (entry.info().and_then(|info| info.duration), entry.trim()))
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
                    entry.segments.set_active(trim);
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
        _: &Entity<SelectState<FolderList>>,
        event: &SelectEvent<FolderList>,
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
            .or(self.default_destination())
            .or(self.folders.first().map(|path| path.as_path()))
    }

    /// The subfolder named in the settings, if it exists.
    fn default_destination(&self) -> Option<&Path> {
        let name = crate::settings::get().default_destination;
        if name.is_empty() {
            return None;
        }
        self.folders
            .iter()
            .find(|path| display_name(path) == name.as_str())
            .map(PathBuf::as_path)
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

    /// The keyframe a lossless cut of `path` starting at `start` begins on,
    /// looking it up in the background the first time it is asked for.
    /// `None` while that lookup runs.
    fn keyframe_for(
        &mut self,
        path: &Path,
        start: Duration,
        cx: &mut Context<Self>,
    ) -> Option<Keyframe> {
        if start.is_zero() {
            return Some(Keyframe::Found(Duration::ZERO));
        }
        let key = (path.to_path_buf(), start);
        if let Some(found) = self.editor.keyframes.get(&key) {
            return Some(found.clone());
        }
        if self.editor.keyframe_pending {
            return None;
        }
        self.editor.keyframe_pending = true;
        let lookup = cx.background_spawn({
            let path = key.0.clone();
            async move { ffmpeg::keyframe_at_or_before(&path, start) }
        });
        cx.spawn(async move |this, cx| {
            let found = match lookup.await {
                Ok(keyframe) => Keyframe::Found(keyframe),
                Err(error) => Keyframe::Failed(format!("{error:#}").into()),
            };
            this.update(cx, |this, cx| {
                let keyframes = &mut this.editor.keyframes;
                if keyframes.len() >= KEYFRAME_CACHE_LIMIT {
                    keyframes.clear();
                }
                keyframes.insert(key, found);
                this.editor.keyframe_pending = false;
                cx.notify();
            })
            .ok();
        })
        .detach();
        None
    }

    pub(super) fn add_segment(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let added = self.player.update(cx, |player, cx| player.add_segment(cx));
        if added {
            self.sync_time_fields(window, cx);
        } else if self.current().is_some() {
            window.push_notification(
                Notification::warning(
                    "A segment already starts at the playhead, or it is at the end.",
                )
                .title("Could not add a segment"),
                cx,
            );
        }
    }

    pub(super) fn split_at_playhead(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let split = self
            .player
            .update(cx, |player, cx| player.split_at_playhead(cx));
        if split {
            self.sync_time_fields(window, cx);
        } else if self.current().is_some() {
            window.push_notification(
                Notification::warning("Move the playhead inside a segment, away from its ends.")
                    .title("Nothing to split"),
                cx,
            );
        }
    }

    fn select_segment(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.player
            .update(cx, |player, cx| player.select_segment(index, window, cx));
        self.editor.time_error = None;
        self.sync_time_fields(window, cx);
    }

    fn remove_segment(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.player
            .update(cx, |player, cx| player.remove_segment(index, cx));
        self.editor.time_error = None;
        self.sync_time_fields(window, cx);
    }

    fn set_join(&mut self, join: bool, cx: &mut Context<Self>) {
        self.editor.join = join;
        self.editor.export_error = None;
        cx.notify();
    }

    /// The segment list, with the active one highlighted, and the choice
    /// between one joined file and one file per segment.
    fn render_segments(&self, entry: &ClipEntry, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let segments = &entry.segments;
        let extension = SharedString::from(format!(".{}", self.editor.format.extension()));
        let duration = entry.info().and_then(|info| info.duration);
        let active = segments.active_index();
        let multiple = segments.len() > 1;
        let separate = multiple && !self.editor.join;
        let parts = separate.then(|| segments.named_parts(&entry.name));
        let rows = segments
            .ranges()
            .iter()
            .enumerate()
            .map(|(index, range)| {
                let end = range
                    .resolved_end(duration)
                    .map_or("end".to_string(), timestamp::format);
                let is_active = index == active;
                let part = parts
                    .as_ref()
                    .map(|parts| parts[segments.position(index)].0.clone());
                h_flex()
                    .id(("segment", index))
                    .h(px(28.))
                    .pl_2()
                    .gap_2()
                    .rounded(theme.radius)
                    .text_sm()
                    .cursor_pointer()
                    .when(is_active, |row| row.bg(theme.muted))
                    .hover(|row| row.bg(theme.muted))
                    .child(
                        div()
                            .w(px(8.))
                            .h(px(8.))
                            .rounded_full()
                            .when(is_active, |dot| dot.bg(theme.yellow))
                            .when(!is_active, |dot| {
                                dot.border_1().border_color(theme.yellow.opacity(0.6))
                            }),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .font_family(theme.mono_font_family.clone())
                            .text_xs()
                            .child(format!("{} – {end}", timestamp::format(range.start))),
                    )
                    .when_some(part, |row, part| {
                        row.child(
                            div()
                                .flex_shrink(1.)
                                .min_w_0()
                                .max_w(px(120.))
                                .truncate()
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .child(part),
                        )
                    })
                    .child(
                        Button::new(("remove-segment", index))
                            .ghost()
                            .xsmall()
                            .icon(IconName::X)
                            .tooltip("Remove this segment")
                            .on_click(cx.listener(move |this, _, window, cx| {
                                cx.stop_propagation();
                                this.remove_segment(index, window, cx)
                            })),
                    )
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.select_segment(index, window, cx)
                    }))
            })
            .collect::<Vec<_>>();

        v_flex()
            .gap_1()
            .when(multiple, |list| list.children(rows))
            .child(
                h_flex()
                    .gap_1()
                    .child(
                        Button::new("add-segment")
                            .ghost()
                            .small()
                            .flex_1()
                            .icon(IconName::Plus)
                            .label("Add segment")
                            .tooltip_with_action(
                                "Start a segment at the playhead",
                                &AddSegment,
                                Some(super::CONTEXT),
                            )
                            .on_click(
                                cx.listener(|this, _, window, cx| this.add_segment(window, cx)),
                            ),
                    )
                    .child(
                        Button::new("split-segment")
                            .ghost()
                            .small()
                            .flex_1()
                            .icon(IconName::Scissors)
                            .label("Split")
                            .tooltip_with_action(
                                "Split the segment at the playhead",
                                &SplitAtPlayhead,
                                Some(super::CONTEXT),
                            )
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.split_at_playhead(window, cx)
                            })),
                    ),
            )
            .when(multiple, |list| {
                list.child(
                    h_flex()
                        .pt_1()
                        .gap_2()
                        .child(
                            v_flex()
                                .flex_1()
                                .child(div().text_sm().child("Join into one file"))
                                .child(div().text_xs().text_color(theme.muted_foreground).child(
                                    if self.editor.join {
                                        "Segments play back to back, in order"
                                    } else {
                                        "Each segment is saved as name-1, name-2, …"
                                    },
                                )),
                        )
                        .child(
                            Switch::new("join-segments")
                                .checked(self.editor.join)
                                .on_click(cx.listener(|this, checked: &bool, _, cx| {
                                    this.set_join(*checked, cx)
                                })),
                        ),
                )
            })
            .when(separate, |list| {
                list.child(
                    v_flex()
                        .pt_1()
                        .gap_1p5()
                        .child(
                            div()
                                .text_xs()
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(theme.muted_foreground)
                                .child("Name of the selected part"),
                        )
                        .child(
                            Input::new(&self.editor.part_name).small().suffix(
                                div()
                                    .text_xs()
                                    .text_color(theme.muted_foreground)
                                    .child(extension.clone()),
                            ),
                        ),
                )
            })
    }

    fn on_crop_position_event(
        &mut self,
        _: &Entity<SliderState>,
        event: &SliderEvent,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (SliderEvent::Change(value) | SliderEvent::Release(value)) = event;
        let position = value.start();
        self.change_transform(|transform| transform.crop_position = position, cx);
    }

    /// Edits the selected clip's transform and updates the viewer's crop
    /// overlay to match.
    fn change_transform(&mut self, edit: impl FnOnce(&mut Transform), cx: &mut Context<Self>) {
        let Some(entry) = self.current_mut() else {
            return;
        };
        edit(&mut entry.transform);
        let crop = entry
            .info()
            .and_then(|info| info.video_size)
            .and_then(|size| entry.transform.crop_rect(size));
        self.player
            .update(cx, |player, cx| player.set_crop(crop, cx));
        self.editor.export_error = None;
        cx.notify();
    }

    /// Crop, rotation and flips for the selected clip.
    fn render_frame_controls(&self, entry: &ClipEntry, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let transform = entry.transform;
        let crop_button = |id: &'static str, label: &'static str, aspect: Option<CropAspect>| {
            Button::new(id)
                .ghost()
                .xsmall()
                .flex_1()
                .label(label)
                .selected(transform.crop == aspect)
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.change_transform(|transform| transform.crop = aspect, cx)
                }))
        };
        let mut crops = vec![crop_button("crop-none", "Full", None)];
        for aspect in CropAspect::ALL {
            let id = match aspect {
                CropAspect::Wide => "crop-wide",
                CropAspect::Tall => "crop-tall",
                CropAspect::Square => "crop-square",
                CropAspect::Portrait => "crop-portrait",
            };
            crops.push(crop_button(id, aspect.label(), Some(aspect)));
        }
        let degrees = transform.rotation.degrees();

        v_flex()
            .gap_1p5()
            .child(
                h_flex()
                    .justify_between()
                    .child(
                        div()
                            .text_xs()
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(theme.muted_foreground)
                            .child("Frame"),
                    )
                    .when(!transform.is_identity(), |row| {
                        row.child(
                            Button::new("reset-frame")
                                .ghost()
                                .xsmall()
                                .label("Reset")
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.change_transform(
                                        |transform| *transform = Transform::identity(),
                                        cx,
                                    );
                                    this.editor
                                        .crop_position
                                        .update(cx, |slider, cx| slider.set_value(0.5, window, cx));
                                })),
                        )
                    }),
            )
            .child(h_flex().gap_0p5().children(crops))
            .when(transform.crop.is_some(), |column| {
                column.child(
                    h_flex()
                        .gap_2()
                        .child(
                            div()
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .child("Position"),
                        )
                        .child(
                            div()
                                .flex_1()
                                .child(Slider::new(&self.editor.crop_position)),
                        ),
                )
            })
            .child(
                h_flex()
                    .gap_0p5()
                    .child(
                        Button::new("rotate-ccw")
                            .ghost()
                            .xsmall()
                            .icon(IconName::RotateCcw)
                            .tooltip("Rotate left")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.change_transform(
                                    |transform| {
                                        transform.rotation = transform.rotation.counter_clockwise()
                                    },
                                    cx,
                                )
                            })),
                    )
                    .child(
                        Button::new("rotate-cw")
                            .ghost()
                            .xsmall()
                            .icon(IconName::RotateCw)
                            .tooltip("Rotate right")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.change_transform(
                                    |transform| transform.rotation = transform.rotation.clockwise(),
                                    cx,
                                )
                            })),
                    )
                    .child(
                        Button::new("flip-horizontal")
                            .ghost()
                            .xsmall()
                            .icon(IconName::FlipHorizontal2)
                            .tooltip("Flip horizontally")
                            .selected(transform.flip_horizontal)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.change_transform(
                                    |transform| transform.flip_horizontal ^= true,
                                    cx,
                                )
                            })),
                    )
                    .child(
                        Button::new("flip-vertical")
                            .ghost()
                            .xsmall()
                            .icon(IconName::FlipVertical2)
                            .tooltip("Flip vertically")
                            .selected(transform.flip_vertical)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.change_transform(
                                    |transform| transform.flip_vertical ^= true,
                                    cx,
                                )
                            })),
                    )
                    .child(div().flex_1())
                    .when(degrees != 0, |row| {
                        row.child(
                            div()
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .child(format!("Rotated {degrees}°")),
                        )
                    }),
            )
    }

    fn on_gain_event(
        &mut self,
        _: &Entity<SliderState>,
        event: &SliderEvent,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (SliderEvent::Change(value) | SliderEvent::Release(value)) = event;
        let gain = value.start();
        if let Some(entry) = self.current_mut() {
            entry.audio_adjust.gain_db = gain;
            cx.notify();
        }
    }

    fn reset_gain(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(entry) = self.current_mut() {
            entry.audio_adjust.gain_db = 0.0;
        }
        self.editor
            .gain
            .update(cx, |slider, cx| slider.set_value(0.0, window, cx));
        cx.notify();
    }

    fn set_normalize(&mut self, normalize: bool, cx: &mut Context<Self>) {
        if let Some(entry) = self.current_mut() {
            entry.audio_adjust.normalize = normalize;
            cx.notify();
        }
    }

    /// Adds or removes one audio track of the selected clip.
    fn toggle_audio_track(&mut self, track: usize, keep: bool, cx: &mut Context<Self>) {
        let Some(entry) = self.current_mut() else {
            return;
        };
        let mut tracks = entry.export_tracks();
        tracks.retain(|&other| other != track);
        if keep {
            tracks.push(track);
            tracks.sort_unstable();
        }
        entry.audio_tracks = Some(tracks);
        self.editor.export_error = None;
        cx.notify();
    }

    /// Which of the clip's audio tracks are exported.
    fn render_audio_controls(&self, entry: &ClipEntry, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let tracks = entry
            .info()
            .map(|info| info.audio_tracks.clone())
            .unwrap_or_default();
        let kept = entry.export_tracks();
        let summary = match (tracks.len(), kept.len()) {
            (_, 0) => "No audio",
            (1, _) => "Original sound",
            (_, 1) => "One track",
            _ => "Mixed into one track",
        };
        let rows = tracks.iter().enumerate().map(|(index, track)| {
            let label = if tracks.len() == 1 {
                "Keep the sound".to_string()
            } else {
                track.describe(index)
            };
            Checkbox::new(("audio-track", index))
                .small()
                .label(label)
                .checked(kept.contains(&index))
                .on_click(cx.listener(move |this, checked: &bool, _, cx| {
                    this.toggle_audio_track(index, *checked, cx)
                }))
        });
        v_flex()
            .gap_1p5()
            .child(
                h_flex()
                    .justify_between()
                    .child(
                        div()
                            .text_xs()
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(theme.muted_foreground)
                            .child("Audio"),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(summary),
                    ),
            )
            .children(rows)
            .when(!kept.is_empty(), |column| {
                let adjust = entry.audio_adjust;
                column
                    .child(
                        h_flex()
                            .gap_2()
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(theme.muted_foreground)
                                    .child("Gain"),
                            )
                            .child(div().flex_1().child(Slider::new(&self.editor.gain)))
                            .child(
                                Button::new("reset-gain")
                                    .ghost()
                                    .xsmall()
                                    .w(px(60.))
                                    .label(format!("{:+.1} dB", adjust.gain_db))
                                    .tooltip("Reset to 0 dB")
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.reset_gain(window, cx)
                                    })),
                            ),
                    )
                    .child(
                        Checkbox::new("normalize-loudness")
                            .small()
                            .label(format!(
                                "Normalize loudness to {} LUFS",
                                ffmpeg::TARGET_LOUDNESS
                            ))
                            .checked(adjust.normalize)
                            .on_click(cx.listener(|this, checked: &bool, _, cx| {
                                this.set_normalize(*checked, cx)
                            })),
                    )
            })
    }

    fn set_codec(&mut self, codec: Codec, cx: &mut Context<Self>) {
        if self.editor.video.codec != codec {
            self.editor.video.codec = codec;
            // Find a working encoder before the export needs one.
            ffmpeg::warm_up_encoder(codec);
            cx.notify();
        }
    }

    fn set_quality(&mut self, quality: Quality, cx: &mut Context<Self>) {
        self.editor.video.quality = quality;
        cx.notify();
    }

    fn set_format(&mut self, format: Format, cx: &mut Context<Self>) {
        self.editor.format = format;
        self.editor.export_error = None;
        cx.notify();
    }

    fn set_animation(&mut self, edit: impl FnOnce(&mut Animation), cx: &mut Context<Self>) {
        edit(&mut self.editor.animation);
        cx.notify();
    }

    /// The MP4 / GIF / WebP choice.
    fn render_format(&self, enabled: bool, cx: &mut Context<Self>) -> impl IntoElement {
        let format = self.editor.format;
        h_flex().gap_0p5().children(Format::ALL.map(|choice| {
            Button::new(SharedString::from(format!("format-{}", choice.label())))
                .ghost()
                .xsmall()
                .flex_1()
                .label(choice.label())
                .selected(format == choice)
                .disabled(!enabled)
                .on_click(cx.listener(move |this, _, _, cx| this.set_format(choice, cx)))
        }))
    }

    /// Width and frame rate of an animated export.
    fn render_animation(&self, enabled: bool, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let animation = self.editor.animation;
        let row = |label: &'static str| {
            h_flex().gap_2().child(
                div()
                    .w(px(48.))
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(label),
            )
        };
        let widths = Animation::WIDTHS.map(|width| {
            Button::new(SharedString::from(format!("anim-width-{width}")))
                .ghost()
                .xsmall()
                .flex_1()
                .label(width.to_string())
                .selected(animation.width == width)
                .disabled(!enabled)
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.set_animation(|animation| animation.width = width, cx)
                }))
        });
        let rates = Animation::FRAME_RATES.map(|fps| {
            Button::new(SharedString::from(format!("anim-fps-{fps}")))
                .ghost()
                .xsmall()
                .flex_1()
                .label(format!("{fps} fps"))
                .selected(animation.fps == fps)
                .disabled(!enabled)
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.set_animation(|animation| animation.fps = fps, cx)
                }))
        });
        v_flex()
            .gap_1()
            .child(row("Width").child(h_flex().flex_1().gap_0p5().children(widths)))
            .child(row("Rate").child(h_flex().flex_1().gap_0p5().children(rates)))
            .child(
                div()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child("Silent and looping. Keep it short: size grows fast."),
            )
    }

    fn set_limits(&mut self, edit: impl FnOnce(&mut ffmpeg::OutputLimits), cx: &mut Context<Self>) {
        edit(&mut self.editor.limits);
        self.editor.export_error = None;
        cx.notify();
    }

    fn set_target_size(&mut self, megabytes: Option<u64>, cx: &mut Context<Self>) {
        self.editor.video.target_size = megabytes.map(|megabytes| megabytes * 1024 * 1024);
        self.editor.export_error = None;
        cx.notify();
    }

    /// Codec, quality and target size for re-encoded exports. `length` is
    /// how long the export will be, to check a target size against.
    fn render_encoding(
        &self,
        enabled: bool,
        length: Option<Duration>,
        with_audio: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let theme = cx.theme().clone();
        let video = self.editor.video;
        let row = |label: &'static str| {
            h_flex().gap_2().child(
                div()
                    .w(px(48.))
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(label),
            )
        };
        let codecs = Codec::ALL.map(|codec| {
            Button::new(SharedString::from(format!("codec-{}", codec.label())))
                .ghost()
                .xsmall()
                .flex_1()
                .label(codec.label())
                .tooltip(codec.description())
                .selected(video.codec == codec)
                .disabled(!enabled)
                .on_click(cx.listener(move |this, _, _, cx| this.set_codec(codec, cx)))
        });
        let sized = video.target_size.is_some();
        let qualities = Quality::ALL.map(|quality| {
            Button::new(SharedString::from(format!("quality-{}", quality.label())))
                .ghost()
                .xsmall()
                .flex_1()
                .label(quality.label())
                .selected(video.quality == quality && !sized)
                .disabled(!enabled || sized)
                .on_click(cx.listener(move |this, _, _, cx| this.set_quality(quality, cx)))
        });
        let target_megabytes = video.target_size.map(|bytes| bytes / (1024 * 1024));
        let sizes = std::iter::once(None)
            .chain(TARGET_SIZES_MB.iter().copied().map(Some))
            .map(|megabytes| {
                Button::new(SharedString::from(format!(
                    "target-{}",
                    megabytes.unwrap_or(0)
                )))
                .ghost()
                .xsmall()
                .flex_1()
                .label(megabytes.map_or("Any".to_string(), |megabytes| format!("{megabytes} MB")))
                .selected(target_megabytes == megabytes)
                .disabled(!enabled)
                .on_click(cx.listener(move |this, _, _, cx| this.set_target_size(megabytes, cx)))
            });
        // A target too small for the length cannot be met; say so now.
        let size_problem = video
            .target_size
            .zip(length)
            .and_then(|(target, length)| ffmpeg::target_bitrates(target, length, with_audio).err());
        let limits = self.editor.limits;
        let resolutions = std::iter::once(None)
            .chain(RESOLUTIONS.iter().copied().map(Some))
            .map(|lines| {
                Button::new(SharedString::from(format!("lines-{}", lines.unwrap_or(0))))
                    .ghost()
                    .xsmall()
                    .flex_1()
                    .label(lines.map_or("Source".to_string(), |lines| format!("{lines}p")))
                    .selected(limits.max_lines == lines)
                    .disabled(!enabled)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.set_limits(|limits| limits.max_lines = lines, cx)
                    }))
            });
        let rates = std::iter::once(None)
            .chain(FRAME_RATE_CAPS.iter().copied().map(Some))
            .map(|fps| {
                Button::new(SharedString::from(format!("fps-{}", fps.unwrap_or(0))))
                    .ghost()
                    .xsmall()
                    .flex_1()
                    .label(fps.map_or("Source".to_string(), |fps| format!("{fps} fps")))
                    .selected(limits.max_fps == fps)
                    .disabled(!enabled)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.set_limits(|limits| limits.max_fps = fps, cx)
                    }))
            });
        v_flex()
            .gap_1()
            .child(row("Codec").child(h_flex().flex_1().gap_0p5().children(codecs)))
            .child(row("Size").child(h_flex().flex_1().gap_0p5().children(resolutions)))
            .child(row("Rate").child(h_flex().flex_1().gap_0p5().children(rates)))
            .child(row("Quality").child(h_flex().flex_1().gap_0p5().children(qualities)))
            .child(row("File").child(h_flex().flex_1().gap_0p5().children(sizes)))
            .when_some(size_problem, |column, problem| {
                column.child(
                    div()
                        .text_xs()
                        .text_color(theme.danger)
                        .child(format!("{problem:#}")),
                )
            })
    }

    fn set_mode(&mut self, lossless: bool, cx: &mut Context<Self>) {
        self.editor.mode = if lossless {
            ExportMode::StreamCopy
        } else {
            ExportMode::Reencode
        };
        self.editor.export_error = None;
        cx.notify();
    }

    pub(super) fn render_export_panel(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        // Cropping and rotating are filters, so they always re-encode.
        let transformed = self
            .current()
            .is_some_and(|entry| !entry.transform.is_identity());
        let animated = self.editor.format.is_animation();
        let lossless = self.editor.mode == ExportMode::StreamCopy && !transformed && !animated;
        let extension = SharedString::from(format!(".{}", self.editor.format.extension()));
        // Resolved before `entry` borrows `self` for the rest of the render.
        let keyframe = match self.current() {
            Some(entry) if lossless && entry.info().is_some() => {
                let (path, start) = (entry.file.path.clone(), entry.trim().start);
                Some(self.keyframe_for(&path, start, cx))
            }
            _ => None,
        };
        let theme = cx.theme().clone();
        let entry = self.current();
        let enabled = entry.is_some_and(|entry| entry.info().is_some());
        let exporting = entry.is_some_and(|entry| entry.status == ClipStatus::Exporting);
        let duration = entry.and_then(|entry| entry.info()?.duration);
        let trim = entry.map(|entry| entry.trim()).unwrap_or_default();
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

        // A lossless cut starts on the keyframe, so it runs a little longer.
        let effective_start = match &keyframe {
            Some(Some(Keyframe::Found(start))) => *start,
            _ => trim.start,
        };
        let segment_count = entry.map_or(1, |entry| entry.segments.len());
        let length = entry.and_then(|entry| {
            let others = entry.segments.total_length(duration)?;
            let active = trim.length(duration)?;
            let effective = TrimRange::new(effective_start, trim.end).length(duration)?;
            Some(others - active + effective)
        });
        // With a target size the file lands just under it; otherwise the
        // source's bitrate is the best cheap guess.
        let target = self.editor.video.target_size.filter(|_| !lossless);
        let animation_size = entry
            .and_then(|entry| entry.info()?.video_size.map(|size| (entry, size)))
            .zip(length)
            .filter(|_| animated)
            .map(|((entry, size), length)| {
                let size = entry.transform.output_size(size);
                format_size(
                    self.editor
                        .animation
                        .estimated_size(self.editor.format, size, length),
                )
            });
        let estimate = match target.filter(|_| !animated) {
            _ if animated => animation_size,
            Some(target) => Some(format_size(target)),
            None => entry
                .zip(duration)
                .zip(length)
                .map(|((entry, duration), length)| {
                    format_size(ffmpeg::estimated_size(entry.file.size, duration, length))
                }),
        };
        let with_audio = entry.is_some_and(|entry| !entry.export_tracks().is_empty());
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
                    if segment_count > 1 && !self.editor.join {
                        format!("in {segment_count} files")
                    } else if segment_count == 1 && trim.is_full(Some(duration)) {
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
                        .tooltip(move |window, cx| {
                            Tooltip::new(if target.is_some() {
                                "Encoded to fit this size"
                            } else {
                                "Estimated from the original's bitrate"
                            })
                            .build(window, cx)
                        }),
                )
            });

        let snapped = keyframe.map(|keyframe| {
            let note = |color: Hsla, text: String| div().text_xs().text_color(color).child(text);
            match keyframe {
                None => note(
                    theme.muted_foreground,
                    "Finding the keyframe the cut starts on…".into(),
                ),
                Some(Keyframe::Found(start)) if trim.start.saturating_sub(start) >= MOVE_NOTICE => {
                    note(
                        theme.warning,
                        format!(
                            "Starts at {}, the keyframe {} earlier. The end stays as set.",
                            timestamp::format_precise(start),
                            timestamp::format_precise(trim.start - start),
                        ),
                    )
                }
                Some(Keyframe::Found(_)) => note(
                    theme.muted_foreground,
                    "Starts on a keyframe, exactly as set.".into(),
                ),
                Some(Keyframe::Failed(error)) => note(theme.danger, error.to_string()),
            }
        });

        let mode_row = h_flex()
            .gap_2()
            .child(
                v_flex()
                    .flex_1()
                    .child(div().text_sm().child("Lossless"))
                    .child(div().text_xs().text_color(theme.muted_foreground).child(
                        if transformed {
                            "Off while the frame is cropped, rotated or flipped"
                        } else {
                            "Instant copy, cut on a keyframe"
                        },
                    )),
            )
            .child(
                Switch::new("lossless")
                    .checked(lossless)
                    .disabled(!enabled || transformed)
                    .tooltip(
                        "Copy the video without re-encoding: instant and with no quality loss, \
                         but the start moves back to the nearest keyframe",
                    )
                    .on_click(
                        cx.listener(|this, checked: &bool, _, cx| this.set_mode(*checked, cx)),
                    ),
            );

        let separate = segment_count > 1 && !self.editor.join;
        let output_preview = match (&destination, output_name.is_empty()) {
            (Some(folder), false) if separate => Some(format!(
                "{folder}{}{segment_count} files",
                std::path::MAIN_SEPARATOR
            )),
            (Some(folder), false) => Some(format!(
                "{folder}{}{output_name}{extension}",
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
                                            .child(extension.clone()),
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
                        .when_some(entry, |panel, entry| {
                            panel.child(self.render_segments(entry, cx))
                        })
                        .when_some(entry.filter(|_| enabled), |panel, entry| {
                            panel.child(self.render_frame_controls(entry, cx))
                        })
                        .when_some(
                            entry.filter(|entry| {
                                !animated && entry.info().is_some_and(|info| info.has_audio)
                            }),
                            |panel, entry| panel.child(self.render_audio_controls(entry, cx)),
                        )
                        .child(summary)
                        .child(self.render_format(enabled, cx))
                        .when(animated, |panel| {
                            panel.child(self.render_animation(enabled, cx))
                        })
                        .when(!animated, |panel| {
                            panel.child(v_flex().gap_1p5().child(mode_row).children(snapped).when(
                                !lossless,
                                |column| {
                                    column.child(
                                        self.render_encoding(enabled, length, with_audio, cx),
                                    )
                                },
                            ))
                        })
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
                                                    .search_placeholder("Find a folder")
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
