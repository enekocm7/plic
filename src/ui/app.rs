//! The main window: clip library, viewer and export panel.

use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _, DropdownButton};
use gpui_kit::component::menu::PopupMenuItem;
use gpui_kit::component::{
    ActiveTheme as _, Icon, Sizable as _, Theme, ThemeMode, TitleBar, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use super::{
    AddSegment, CONTEXT, CopyLastExport, DeleteClip, ExitFullscreen, ExportClip, JumpToEnd,
    JumpToIn, JumpToOut, JumpToStart, MarkIn, MarkOut, NextMarker, OpenFolder, PreviousMarker,
    Reload, ResetTrim, SaveFrame, SaveFrameJpeg, SeekBackward, SeekForward, SelectNextClip,
    SelectPreviousClip, ShowSettings, ShowShortcuts, ShuttleBackward, ShuttleForward,
    SplitAtPlayhead, StepBackward, StepForward, ToggleFullscreen, ToggleLoop, ToggleMarker,
    ToggleMute, TogglePlay, ToggleTheme, TrimToMarkers,
    clip_filter::ClipFilter,
    editor::Editor,
    frame_image,
    jobs::{ExportJob, FinishedExport},
    player::{Player, PlayerEvent},
    shortcuts,
};
use crate::{
    ffmpeg::{self, AudioAdjust, FrameSize, MediaInfo, RawFrame, Transform},
    library::{self, ClipFile, Library},
    trim::{Segments, TrimRange},
    updates::Update,
    watcher::FolderWatcher,
    worker::WorkerPool,
};

/// How long the folder must stay quiet before it is rescanned, so
/// recordings that are still being written are not listed early.
const WATCH_SETTLE: Duration = Duration::from_secs(2);

/// How long after a change to a clip's draft it is written to disk.
const DRAFT_SAVE_DELAY: Duration = Duration::from_secs(1);

/// Thumbnails are decoded at twice their on-screen size for HiDPI displays.
const THUMBNAIL_SIZE: FrameSize = FrameSize::new(160, 90);

/// Metadata that is loaded in the background after the folder is listed.
pub enum Details {
    Loading,
    Ready(MediaInfo),
    Failed(SharedString),
}

/// Where a clip is in the export workflow.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClipStatus {
    Pending,
    Exporting,
    /// Exported, but the original could not be moved out of the way.
    Exported,
}

/// A clip in the library together with everything the UI tracks about it.
/// The export draft (name, range, folder) is kept while browsing other clips.
pub struct ClipEntry {
    pub file: ClipFile,
    pub details: Details,
    pub thumbnail: Option<Arc<RenderImage>>,
    pub name: String,
    pub segments: Segments,
    /// Crop, rotation and flips applied to the export.
    pub transform: Transform,
    /// Audio tracks to export, by `0:a:N` index; `None` keeps them all.
    pub audio_tracks: Option<Vec<usize>>,
    /// Gain and loudness normalization for the exported sound.
    pub audio_adjust: AudioAdjust,
    /// Destination chosen for this clip, if it differs from the default.
    pub folder: Option<PathBuf>,
    pub status: ClipStatus,
    /// Whether the name template has already filled in this clip's name.
    pub templated: bool,
}

impl ClipEntry {
    /// The segment being edited.
    pub fn trim(&self) -> TrimRange {
        self.segments.active()
    }

    fn new(file: ClipFile) -> Self {
        Self {
            name: file.stem(),
            file,
            details: Details::Loading,
            thumbnail: None,
            segments: Segments::default(),
            transform: Transform::identity(),
            audio_tracks: None,
            audio_adjust: AudioAdjust::default(),
            folder: None,
            status: ClipStatus::Pending,
            templated: false,
        }
    }

    /// A clip to edit, with the trims, name and destination it was left
    /// with last time.
    fn restored(file: ClipFile) -> Self {
        let mut entry = Self::new(file);
        let Some(draft) = crate::drafts::get(&entry.file.path)
            .filter(|draft| draft.matches_size(entry.file.size))
        else {
            return entry;
        };
        if let Some(segments) = draft.segments() {
            entry.segments = segments;
        }
        if let Some(name) = draft.name {
            entry.name = name;
            // A name from last time is not overwritten by the template.
            entry.templated = true;
        }
        entry.folder = draft.folder.filter(|folder| folder.is_dir());
        entry
    }

    /// What is worth remembering about this clip until it is exported.
    fn draft(&self) -> Option<crate::drafts::Draft> {
        let name = (self.name != self.file.stem()).then(|| self.name.clone());
        crate::drafts::Draft::new(self.file.size, &self.segments, name, self.folder.clone())
    }

    /// The audio tracks to export, in stream order.
    pub fn export_tracks(&self) -> Vec<usize> {
        let count = self.info().map_or(0, |info| info.audio_tracks.len());
        match &self.audio_tracks {
            Some(tracks) => tracks
                .iter()
                .copied()
                .filter(|&track| track < count)
                .collect(),
            None => (0..count).collect(),
        }
    }

    pub fn info(&self) -> Option<&MediaInfo> {
        match &self.details {
            Details::Ready(info) => Some(info),
            _ => None,
        }
    }
}

pub struct PlicApp {
    pub(super) focus: FocusHandle,
    /// The viewer fills the window, hiding everything else.
    fullscreen: bool,
    pub(super) player: Entity<Player>,
    pub(super) editor: Editor,
    /// Search box, ordering and status filter of the clip list.
    pub(super) filter: ClipFilter,
    /// The clips folder.
    pub(super) folder: Option<PathBuf>,
    /// Keeps the clips folder watched; dropped when another folder opens.
    watch: Option<(FolderWatcher, Task<()>)>,
    pub(super) clips: Vec<ClipEntry>,
    /// Subfolders that exported clips can be saved into.
    pub(super) folders: Vec<PathBuf>,
    /// Destination of the most recent export, the default for the next one.
    pub(super) last_folder: Option<PathBuf>,
    /// The selected clip still to be edited. At most one of this and
    /// `settled_selected` is set.
    pub(super) selected: Option<usize>,
    pub(super) list_scroll: UniformListScrollHandle,
    /// Originals already exported, shown in the collapsible Settled list.
    pub(super) settled: Vec<ClipEntry>,
    pub(super) settled_selected: Option<usize>,
    pub(super) settled_open: bool,
    pub(super) settled_scroll: UniformListScrollHandle,
    /// Whether the settled clips' details have been requested; they are only
    /// loaded once the list is first opened.
    settled_loaded: bool,
    pub(super) load_error: Option<SharedString>,
    workers: WorkerPool,
    /// Bumped on every folder load so results for an old folder are ignored.
    generation: u64,
    /// Images to evict from the GPU atlas on the next frame.
    stale_images: Vec<Arc<RenderImage>>,
    pub(super) jobs: Vec<ExportJob>,
    /// How many exports may encode at once.
    pub(super) parallel_exports: usize,
    pub(super) next_job_id: u64,
    /// Batches of separate-part exports in which a part failed or was
    /// cancelled, so the original is kept when the rest finish.
    pub(super) failed_batches: HashSet<u64>,
    /// Recently exported files, newest first, for copying or showing.
    pub(super) finished: Vec<FinishedExport>,
    /// A newer release found by the update check, shown as a banner.
    pub(super) update: Option<Update>,
    /// Whether an update check is in flight.
    pub(super) checking_update: bool,
    /// GitHub is asked on its own thread so a slow network never delays the
    /// UI.
    pub(super) updates_worker: WorkerPool,
    /// Whether changed drafts are about to be written.
    saving_drafts: bool,
}

impl PlicApp {
    pub fn new(folder: Option<PathBuf>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus = cx.focus_handle();
        // Deferred so that setting up the editor's widgets cannot take it.
        window.defer(cx, {
            let focus = focus.clone();
            move |window, cx| focus.focus(window, cx)
        });
        let player = cx.new(Player::new);
        cx.observe_window_bounds(window, |_, window, _| super::record_bounds(window))
            .detach();
        cx.subscribe_in(&player, window, Self::on_player_event)
            .detach();
        let mut app = Self {
            focus,
            fullscreen: false,
            player,
            editor: Editor::new(window, cx),
            filter: ClipFilter::new(window, cx),
            folder: None,
            watch: None,
            clips: Vec::new(),
            folders: Vec::new(),
            last_folder: None,
            selected: None,
            list_scroll: UniformListScrollHandle::new(),
            settled: Vec::new(),
            settled_selected: None,
            settled_open: false,
            settled_scroll: UniformListScrollHandle::new(),
            settled_loaded: false,
            load_error: None,
            workers: WorkerPool::new("inspect", 3),
            generation: 0,
            stale_images: Vec::new(),
            jobs: Vec::new(),
            parallel_exports: 2,
            next_job_id: 0,
            failed_batches: HashSet::new(),
            finished: Vec::new(),
            update: None,
            checking_update: false,
            updates_worker: WorkerPool::new("update", 1),
            saving_drafts: false,
        };
        if let Some(folder) = folder {
            app.load_folder(folder, window, cx);
        }
        // A development build is not a release, so it does not ask GitHub on
        // every `mise run dev`.
        if !cfg!(debug_assertions) {
            app.check_for_updates(false, window, cx);
        }
        app
    }

    fn load_folder(&mut self, folder: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        self.generation += 1;
        let old_entries = std::mem::take(&mut self.clips)
            .into_iter()
            .chain(std::mem::take(&mut self.settled));
        self.stale_images
            .extend(old_entries.filter_map(|entry| entry.thumbnail));
        match Library::scan(&folder) {
            Ok(library) => {
                self.clips = library.clips.into_iter().map(ClipEntry::restored).collect();
                self.settled = library.settled.into_iter().map(ClipEntry::new).collect();
                self.folders = library.folders;
                self.load_error = None;
            }
            Err(error) => {
                self.folders.clear();
                self.load_error = Some(format!("{error:#}").into());
            }
        }
        // Clips that are still exporting stay marked as such.
        for entry in &mut self.clips {
            if self.jobs.iter().any(|job| job.source() == entry.file.path) {
                entry.status = ClipStatus::Exporting;
            }
        }
        if self.folder.as_ref() != Some(&folder) {
            self.last_folder = None;
        }
        crate::state::update(|state| state.remember_folder(&folder));
        crate::state::save();
        self.watch_folder(&folder, window, cx);
        self.folder = Some(folder);
        self.sync_folder_choices(window, cx);
        self.selected = None;
        self.settled_selected = None;
        self.select(0, window, cx);
        if self.clips.is_empty() {
            self.show_selected_in_player(cx);
            self.sync_editor(window, cx);
        }
        let paths: Vec<_> = self
            .clips
            .iter()
            .map(|entry| entry.file.path.clone())
            .collect();
        for path in paths {
            self.load_details(path, cx);
        }
        self.settled_loaded = false;
        if self.settled_open {
            self.load_settled_details(cx);
        }
        cx.notify();
    }

    /// Picks up recordings that appear in the folder, and notices ones that
    /// disappear, without touching the drafts of the other clips.
    fn watch_folder(&mut self, folder: &Path, window: &mut Window, cx: &mut Context<Self>) {
        self.watch = None;
        let Ok((watcher, mut changes)) = FolderWatcher::start(folder) else {
            return;
        };
        let task = cx.spawn_in(window, async move |this, cx| {
            use futures::StreamExt as _;
            while changes.next().await.is_some() {
                // Wait for a quiet moment: writes arrive in a stream.
                loop {
                    cx.background_executor().timer(WATCH_SETTLE).await;
                    let mut more = false;
                    while changes.try_recv().is_ok() {
                        more = true;
                    }
                    if !more {
                        break;
                    }
                }
                let refreshed = this.update_in(cx, |this, window, cx| {
                    this.refresh_folder(window, cx);
                });
                if refreshed.is_err() {
                    return;
                }
            }
        });
        self.watch = Some((watcher, task));
    }

    /// Merges a fresh scan into the lists: new recordings are added and
    /// vanished ones dropped, while every other clip keeps its draft.
    fn refresh_folder(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(folder) = self.folder.clone() else {
            return;
        };
        let Ok(library) = Library::scan(&folder) else {
            return;
        };
        let mut changed = false;

        let gone: Vec<PathBuf> = self
            .clips
            .iter()
            .filter(|entry| entry.status == ClipStatus::Pending)
            .filter(|entry| {
                !library
                    .clips
                    .iter()
                    .any(|file| file.path == entry.file.path)
            })
            .map(|entry| entry.file.path.clone())
            .collect();
        for path in gone {
            if let Some(index) = self.index_of(&path) {
                let entry = self.take_clip(index, window, cx);
                self.stale_images.extend(entry.thumbnail);
                changed = true;
            }
        }

        for file in library.clips {
            if self.index_of(&file.path).is_some() || self.settled_index_of(&file.path).is_some() {
                continue;
            }
            let path = file.path.clone();
            let key = file.file_name().to_lowercase();
            let position = self
                .clips
                .partition_point(|clip| clip.file.file_name().to_lowercase() < key);
            self.clips.insert(position, ClipEntry::restored(file));
            if let Some(selected) = &mut self.selected
                && *selected >= position
            {
                *selected += 1;
            }
            self.load_details(path, cx);
            changed = true;
        }

        if library.folders != self.folders {
            self.folders = library.folders;
            self.sync_folder_choices(window, cx);
            changed = true;
        }
        if changed {
            // A clip may have appeared in an empty library.
            if self.selected.is_none() && self.settled_selected.is_none() {
                self.select(0, window, cx);
            }
            cx.notify();
        }
    }

    fn load_settled_details(&mut self, cx: &mut Context<Self>) {
        if std::mem::replace(&mut self.settled_loaded, true) {
            return;
        }
        let paths: Vec<_> = self
            .settled
            .iter()
            .filter(|entry| matches!(entry.details, Details::Loading))
            .map(|entry| entry.file.path.clone())
            .collect();
        for path in paths {
            self.load_details(path, cx);
        }
    }

    pub(super) fn toggle_settled(&mut self, cx: &mut Context<Self>) {
        self.settled_open = !self.settled_open;
        if self.settled_open {
            self.load_settled_details(cx);
        }
        cx.notify();
    }

    /// Probes a clip and decodes its thumbnail on the worker pool.
    fn load_details(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        let generation = self.generation;
        let job = self.workers.run({
            let path = path.clone();
            move || inspect(&path)
        });
        cx.spawn(async move |this, cx| {
            let Ok((details, thumbnail)) = job.await else {
                return;
            };
            let thumbnail = thumbnail.and_then(frame_image::render_image);
            this.update(cx, |this, cx| {
                if this.generation != generation {
                    return;
                }
                let Some(entry) = this.entry_mut(&path) else {
                    return;
                };
                entry.details = details;
                entry.thumbnail = thumbnail;
                if this.viewed().is_some_and(|entry| entry.file.path == path) {
                    this.show_selected_in_player(cx);
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    pub(super) fn select(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        if index >= self.clips.len() || self.selected == Some(index) {
            return;
        }
        self.selected = Some(index);
        self.settled_selected = None;
        if let Some(row) = self
            .visible(super::library_panel::List::Clips, cx)
            .iter()
            .position(|&visible| visible == index)
        {
            self.list_scroll
                .scroll_to_item(row, ScrollStrategy::Nearest);
        }
        self.show_selected_in_player(cx);
        self.sync_editor(window, cx);
        cx.notify();
    }

    /// Previews an exported original. The export panel stays hidden, as
    /// there is nothing left to export.
    pub(super) fn select_settled(
        &mut self,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if index >= self.settled.len() || self.settled_selected == Some(index) {
            return;
        }
        self.settled_selected = Some(index);
        self.selected = None;
        if let Some(row) = self
            .visible(super::library_panel::List::Settled, cx)
            .iter()
            .position(|&visible| visible == index)
        {
            self.settled_scroll
                .scroll_to_item(row, ScrollStrategy::Nearest);
        }
        self.show_selected_in_player(cx);
        self.sync_editor(window, cx);
        cx.notify();
    }

    /// Takes a clip out of the list, e.g. once it has been exported and moved
    /// away, and selects its neighbour if it was selected.
    pub(super) fn take_clip(
        &mut self,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> ClipEntry {
        let entry = self.clips.remove(index);
        match self.selected {
            Some(selected) if selected > index => self.selected = Some(selected - 1),
            Some(selected) if selected == index => {
                self.selected = None;
                if self.clips.is_empty() {
                    self.show_selected_in_player(cx);
                    self.sync_editor(window, cx);
                } else {
                    self.select(index.min(self.clips.len() - 1), window, cx);
                }
            }
            _ => {}
        }
        cx.notify();
        entry
    }

    /// Takes an exported original out of the Settled list.
    pub(super) fn take_settled(
        &mut self,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> ClipEntry {
        let entry = self.settled.remove(index);
        match self.settled_selected {
            Some(selected) if selected > index => self.settled_selected = Some(selected - 1),
            Some(selected) if selected == index => {
                self.settled_selected = None;
                if self.settled.is_empty() {
                    self.show_selected_in_player(cx);
                    self.sync_editor(window, cx);
                } else {
                    self.select_settled(index.min(self.settled.len() - 1), window, cx);
                }
            }
            _ => {}
        }
        cx.notify();
        entry
    }

    /// Removes a clip from whichever list holds it and frees its thumbnail.
    pub(super) fn forget(&mut self, path: &Path, window: &mut Window, cx: &mut Context<Self>) {
        let entry = if let Some(index) = self.index_of(path) {
            self.take_clip(index, window, cx)
        } else if let Some(index) = self.settled_index_of(path) {
            self.take_settled(index, window, cx)
        } else {
            return;
        };
        self.stale_images.extend(entry.thumbnail);
    }

    /// Files an exported clip under Settled, now living at `archived`.
    pub(super) fn settle(&mut self, mut entry: ClipEntry, archived: PathBuf) {
        entry.file.path = archived;
        entry.status = ClipStatus::Pending;
        entry.segments = Segments::default();
        entry.transform = Transform::identity();
        entry.audio_tracks = None;
        entry.audio_adjust = AudioAdjust::default();
        self.settled.insert(0, entry);
        if let Some(selected) = &mut self.settled_selected {
            *selected += 1;
        }
    }

    /// Puts a restored original back among the clips to edit, and selects it.
    pub(super) fn unsettle(
        &mut self,
        index: usize,
        restored: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let mut entry = self.take_settled(index, window, cx);
        entry.file.path = restored;
        entry.name = entry.file.stem();
        let key = entry.file.file_name().to_lowercase();
        let position = self
            .clips
            .partition_point(|clip| clip.file.file_name().to_lowercase() < key);
        self.clips.insert(position, entry);
        if let Some(selected) = &mut self.selected
            && *selected >= position
        {
            *selected += 1;
        }
        self.selected = None;
        self.select(position, window, cx);
    }

    /// Loads the selected clip into the player once its details are known.
    pub(super) fn show_selected_in_player(&mut self, cx: &mut Context<Self>) {
        let Some(entry) = self.viewed() else {
            self.player.update(cx, |player, cx| player.clear(cx));
            return;
        };
        let path = entry.file.path.clone();
        let trimmable = !self.is_viewing_settled();
        let crop = trimmable
            .then(|| {
                let size = entry.info()?.video_size?;
                entry.transform.crop_rect(size)
            })
            .flatten();
        self.player.update(cx, |player, cx| {
            player.set_trimmable(trimmable, cx);
            player.set_crop(crop, cx);
        });
        match &entry.details {
            Details::Ready(info) => {
                let (info, segments) = (info.clone(), entry.segments.clone());
                self.player
                    .update(cx, |player, cx| player.load(path, info, segments, cx));
            }
            Details::Loading => self.player.update(cx, |player, cx| player.clear(cx)),
            Details::Failed(error) => {
                let message = format!("Could not read this video: {error}").into();
                self.player
                    .update(cx, |player, cx| player.show_error(message, cx));
            }
        }
    }

    fn on_player_event(
        &mut self,
        _: &Entity<Player>,
        event: &PlayerEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            PlayerEvent::SegmentsChanged(segments) => {
                if let Some(entry) = self.current_mut() {
                    entry.segments = segments.clone();
                    self.sync_time_fields(window, cx);
                    cx.notify();
                }
            }
        }
    }

    fn export_clip(&mut self, _: &ExportClip, window: &mut Window, cx: &mut Context<Self>) {
        self.export_current(window, cx);
    }

    fn delete_clip(&mut self, _: &DeleteClip, window: &mut Window, cx: &mut Context<Self>) {
        self.confirm_delete(window, cx);
    }

    fn mark_in(&mut self, _: &MarkIn, _: &mut Window, cx: &mut Context<Self>) {
        self.player.update(cx, |player, cx| player.mark_in(cx));
    }

    fn mark_out(&mut self, _: &MarkOut, _: &mut Window, cx: &mut Context<Self>) {
        self.player.update(cx, |player, cx| player.mark_out(cx));
    }

    fn add_segment_action(&mut self, _: &AddSegment, window: &mut Window, cx: &mut Context<Self>) {
        self.add_segment(window, cx);
    }

    fn save_frame_png(&mut self, _: &SaveFrame, window: &mut Window, cx: &mut Context<Self>) {
        self.save_frame(ffmpeg::ImageFormat::Png, window, cx);
    }

    fn save_frame_jpeg(&mut self, _: &SaveFrameJpeg, window: &mut Window, cx: &mut Context<Self>) {
        self.save_frame(ffmpeg::ImageFormat::Jpeg, window, cx);
    }

    fn copy_last_export(
        &mut self,
        _: &CopyLastExport,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match self.finished.first().map(|finished| finished.path.clone()) {
            Some(path) => self.copy_export(&path, window, cx),
            None => gpui_kit::component::WindowExt::push_notification(
                window,
                gpui_kit::component::notification::Notification::info("Export a clip first.")
                    .title("Nothing to copy"),
                cx,
            ),
        }
    }

    fn split_action(&mut self, _: &SplitAtPlayhead, window: &mut Window, cx: &mut Context<Self>) {
        self.split_at_playhead(window, cx);
    }

    fn reset_trim(&mut self, _: &ResetTrim, _: &mut Window, cx: &mut Context<Self>) {
        self.player.update(cx, |player, cx| player.reset_trim(cx));
    }

    fn toggle_loop(&mut self, _: &ToggleLoop, _: &mut Window, cx: &mut Context<Self>) {
        self.player.update(cx, |player, cx| player.toggle_loop(cx));
    }

    fn jump_to_in(&mut self, _: &JumpToIn, window: &mut Window, cx: &mut Context<Self>) {
        self.player
            .update(cx, |player, cx| player.jump_to_in(window, cx));
    }

    fn jump_to_out(&mut self, _: &JumpToOut, window: &mut Window, cx: &mut Context<Self>) {
        self.player
            .update(cx, |player, cx| player.jump_to_out(window, cx));
    }

    fn toggle_play(&mut self, _: &TogglePlay, window: &mut Window, cx: &mut Context<Self>) {
        self.player
            .update(cx, |player, cx| player.toggle_play(window, cx));
    }

    fn seek_backward(&mut self, _: &SeekBackward, window: &mut Window, cx: &mut Context<Self>) {
        self.player
            .update(cx, |player, cx| player.seek_by(-seek_step_ms(), window, cx));
    }

    fn seek_forward(&mut self, _: &SeekForward, window: &mut Window, cx: &mut Context<Self>) {
        self.player
            .update(cx, |player, cx| player.seek_by(seek_step_ms(), window, cx));
    }

    fn shuttle_backward(
        &mut self,
        _: &ShuttleBackward,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.player
            .update(cx, |player, cx| player.shuttle_backward(window, cx));
    }

    fn shuttle_forward(&mut self, _: &ShuttleForward, window: &mut Window, cx: &mut Context<Self>) {
        self.player
            .update(cx, |player, cx| player.shuttle_forward(window, cx));
    }

    fn step_backward(&mut self, _: &StepBackward, window: &mut Window, cx: &mut Context<Self>) {
        self.player
            .update(cx, |player, cx| player.step(-1, window, cx));
    }

    fn step_forward(&mut self, _: &StepForward, window: &mut Window, cx: &mut Context<Self>) {
        self.player
            .update(cx, |player, cx| player.step(1, window, cx));
    }

    fn toggle_mute(&mut self, _: &ToggleMute, _: &mut Window, cx: &mut Context<Self>) {
        self.player.update(cx, |player, cx| player.toggle_mute(cx));
    }

    fn toggle_marker(&mut self, _: &ToggleMarker, _: &mut Window, cx: &mut Context<Self>) {
        self.player
            .update(cx, |player, cx| player.toggle_marker(cx));
    }

    fn next_marker(&mut self, _: &NextMarker, window: &mut Window, cx: &mut Context<Self>) {
        self.player
            .update(cx, |player, cx| player.jump_to_next_marker(window, cx));
    }

    fn previous_marker(&mut self, _: &PreviousMarker, window: &mut Window, cx: &mut Context<Self>) {
        self.player
            .update(cx, |player, cx| player.jump_to_previous_marker(window, cx));
    }

    fn trim_to_markers(&mut self, _: &TrimToMarkers, _: &mut Window, cx: &mut Context<Self>) {
        self.player
            .update(cx, |player, cx| player.trim_between_markers(cx));
    }

    fn toggle_fullscreen(
        &mut self,
        _: &ToggleFullscreen,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.set_fullscreen(!self.fullscreen, window, cx);
    }

    fn exit_fullscreen(&mut self, _: &ExitFullscreen, window: &mut Window, cx: &mut Context<Self>) {
        if self.fullscreen {
            self.set_fullscreen(false, window, cx);
        } else {
            cx.propagate();
        }
    }

    fn set_fullscreen(&mut self, fullscreen: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.fullscreen == fullscreen {
            return;
        }
        self.fullscreen = fullscreen;
        if window.is_fullscreen() != fullscreen {
            window.toggle_fullscreen();
        }
        self.player
            .update(cx, |player, cx| player.set_fullscreen(fullscreen, cx));
        self.focus.focus(window, cx);
        cx.notify();
    }

    fn jump_to_start(&mut self, _: &JumpToStart, window: &mut Window, cx: &mut Context<Self>) {
        self.player
            .update(cx, |player, cx| player.seek(Duration::ZERO, window, cx));
    }

    fn jump_to_end(&mut self, _: &JumpToEnd, window: &mut Window, cx: &mut Context<Self>) {
        self.player.update(cx, |player, cx| {
            if let Some(duration) = player.duration() {
                player.seek(duration, window, cx);
            }
        });
    }

    fn select_previous(
        &mut self,
        _: &SelectPreviousClip,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.select_neighbour(-1, window, cx);
    }

    fn select_next(&mut self, _: &SelectNextClip, window: &mut Window, cx: &mut Context<Self>) {
        self.select_neighbour(1, window, cx);
    }

    /// Moves the selection one row up or down the list as it is shown, in
    /// whichever list the selection is in.
    fn select_neighbour(&mut self, step: isize, window: &mut Window, cx: &mut Context<Self>) {
        use super::library_panel::List;
        let (list, selected) = match (self.settled_selected, self.selected) {
            (Some(selected), _) => (List::Settled, Some(selected)),
            (None, selected) => (List::Clips, selected),
        };
        let rows = self.visible(list, cx);
        let current = selected.and_then(|selected| rows.iter().position(|&row| row == selected));
        let target = match current {
            Some(current) => current
                .saturating_add_signed(step)
                .min(rows.len().saturating_sub(1)),
            None => 0,
        };
        let Some(&index) = rows.get(target) else {
            return;
        };
        match list {
            List::Clips => self.select(index, window, cx),
            List::Settled => self.select_settled(index, window, cx),
        }
    }

    fn open_folder(&mut self, _: &OpenFolder, window: &mut Window, cx: &mut Context<Self>) {
        let paths = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Open clips folder".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(paths))) = paths.await else {
                return;
            };
            if let Some(folder) = paths.into_iter().next() {
                this.update_in(cx, |this, window, cx| this.load_folder(folder, window, cx))
                    .ok();
            }
        })
        .detach();
    }

    fn reload(&mut self, _: &Reload, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(folder) = self.folder.clone() {
            let viewed = self.viewed().map(|entry| entry.file.path.clone());
            self.load_folder(folder, window, cx);
            if let Some(path) = viewed {
                if let Some(index) = self.index_of(&path) {
                    self.select(index, window, cx);
                } else if let Some(index) = self.settled_index_of(&path) {
                    self.select_settled(index, window, cx);
                }
            }
        }
    }

    fn show_settings(&mut self, _: &ShowSettings, window: &mut Window, cx: &mut Context<Self>) {
        self.open_settings(window, cx);
    }

    fn show_shortcuts(&mut self, _: &ShowShortcuts, window: &mut Window, cx: &mut Context<Self>) {
        shortcuts::open(window, cx);
    }

    /// Opens what was dropped on the window: a folder replaces the library,
    /// and a video file opens the folder it lives in with that clip
    /// selected. Nothing is moved.
    fn handle_drop(
        &mut self,
        dropped: &ExternalPaths,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let paths = dropped.paths();
        if let Some(folder) = paths.iter().find(|path| path.is_dir()) {
            self.load_folder(folder.clone(), window, cx);
            return;
        }
        let Some(first) = paths
            .iter()
            .find(|path| path.is_file() && library::is_video(path))
        else {
            return;
        };
        if let Some(parent) = first.parent()
            && self.folder.as_deref() != Some(parent)
        {
            self.load_folder(parent.to_path_buf(), window, cx);
        }
        if let Some(index) = self.index_of(first) {
            self.select(index, window, cx);
        } else if let Some(index) = self.settled_index_of(first) {
            self.settled_open = true;
            self.load_settled_details(cx);
            self.select_settled(index, window, cx);
        }
    }

    fn toggle_theme(&mut self, _: &ToggleTheme, window: &mut Window, cx: &mut Context<Self>) {
        let mode = if cx.theme().mode.is_dark() {
            ThemeMode::Light
        } else {
            ThemeMode::Dark
        };
        crate::state::update(|state| state.dark_theme = mode == ThemeMode::Dark);
        Theme::change(mode, Some(window), cx);
    }

    /// Keeps the selected clip's draft for the next run. Only clips still
    /// to be edited have one; exported clips' drafts are dropped.
    fn remember_draft(&mut self, cx: &mut Context<Self>) {
        if let Some(entry) = self
            .current()
            .filter(|entry| entry.status == ClipStatus::Pending)
        {
            crate::drafts::put(&entry.file.path, entry.draft());
        }
        if crate::drafts::is_dirty() && !self.saving_drafts {
            self.saving_drafts = true;
            cx.spawn(async move |this, cx| {
                cx.background_executor().timer(DRAFT_SAVE_DELAY).await;
                cx.background_spawn(async { crate::drafts::save() }).await;
                this.update(cx, |this, _| this.saving_drafts = false).ok();
            })
            .detach();
        }
    }

    pub(super) fn current(&self) -> Option<&ClipEntry> {
        self.clips.get(self.selected?)
    }

    pub(super) fn current_mut(&mut self) -> Option<&mut ClipEntry> {
        self.clips.get_mut(self.selected?)
    }

    /// The clip shown in the viewer: the selected clip or settled original.
    pub(super) fn viewed(&self) -> Option<&ClipEntry> {
        match self.settled_selected {
            Some(index) => self.settled.get(index),
            None => self.current(),
        }
    }

    pub(super) fn is_viewing_settled(&self) -> bool {
        self.settled_selected.is_some()
    }

    fn entry_mut(&mut self, path: &Path) -> Option<&mut ClipEntry> {
        self.clips
            .iter_mut()
            .chain(&mut self.settled)
            .find(|entry| entry.file.path == path)
    }

    pub(super) fn index_of(&self, path: &Path) -> Option<usize> {
        self.clips.iter().position(|entry| entry.file.path == path)
    }

    pub(super) fn settled_index_of(&self, path: &Path) -> Option<usize> {
        self.settled
            .iter()
            .position(|entry| entry.file.path == path)
    }

    fn render_title_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let folder = self
            .folder
            .as_deref()
            .map(|folder| SharedString::from(folder.display().to_string()));
        TitleBar::new().child(
            h_flex()
                .w_full()
                .pr_2()
                .gap_3()
                .justify_between()
                .child(
                    h_flex()
                        .gap_2()
                        .min_w_0()
                        .child(
                            Icon::new(IconName::Clapperboard)
                                .small()
                                .text_color(theme.primary),
                        )
                        .child(
                            div()
                                .text_sm()
                                .font_weight(FontWeight::SEMIBOLD)
                                .child("plic"),
                        )
                        .when_some(folder, |this, folder| {
                            this.child(
                                div()
                                    .text_xs()
                                    .text_color(theme.muted_foreground)
                                    .truncate()
                                    .child(folder),
                            )
                        }),
                )
                .child(
                    h_flex()
                        .id("title-actions")
                        .gap_1()
                        // The whole title bar is a drag area for the OS; without
                        // this, Windows treats clicks on these buttons as the
                        // start of a window move and they never fire.
                        .occlude()
                        .child({
                            let app = cx.entity().downgrade();
                            let current = self.folder.clone();
                            DropdownButton::new("open-folder")
                                .ghost()
                                .small()
                                .button(
                                    Button::new("open-folder-button")
                                        .icon(IconName::FolderOpen)
                                        .label("Open folder")
                                        .tooltip("Open a clips folder (Ctrl+O)")
                                        .on_click(cx.listener(|this, _, window, cx| {
                                            this.open_folder(&OpenFolder, window, cx)
                                        })),
                                )
                                .dropdown_menu(move |menu, _, _| {
                                    let recent: Vec<PathBuf> = crate::state::get()
                                        .recent_folders
                                        .into_iter()
                                        .filter(|folder| Some(folder) != current.as_ref())
                                        .collect();
                                    if recent.is_empty() {
                                        return menu.label("No recent folders");
                                    }
                                    recent.into_iter().fold(menu, |menu, folder| {
                                        let app = app.clone();
                                        let label = folder.display().to_string();
                                        menu.item(PopupMenuItem::new(label).on_click(
                                            move |_, window, cx| {
                                                let folder = folder.clone();
                                                app.update(cx, |app, cx| {
                                                    app.load_folder(folder, window, cx)
                                                })
                                                .ok();
                                            },
                                        ))
                                    })
                                })
                        })
                        .child(
                            Button::new("reload")
                                .ghost()
                                .small()
                                .icon(IconName::RefreshCw)
                                .tooltip("Rescan the folder (F5)")
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.reload(&Reload, window, cx)
                                })),
                        )
                        .child(
                            Button::new("shortcuts")
                                .ghost()
                                .small()
                                .icon(IconName::Keyboard)
                                .tooltip("Keyboard shortcuts (?)")
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.show_shortcuts(&ShowShortcuts, window, cx)
                                })),
                        )
                        .child(
                            Button::new("settings")
                                .ghost()
                                .small()
                                .icon(IconName::Settings)
                                .tooltip("Settings (Ctrl+,)")
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.show_settings(&ShowSettings, window, cx)
                                })),
                        )
                        .child(
                            Button::new("check-updates")
                                .ghost()
                                .small()
                                .icon(IconName::Download)
                                .loading(self.checking_update)
                                .tooltip("Check for updates")
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.check_for_updates(true, window, cx)
                                })),
                        )
                        .child(
                            Button::new("theme")
                                .ghost()
                                .small()
                                .icon(if theme.mode.is_dark() {
                                    IconName::Sun
                                } else {
                                    IconName::Moon
                                })
                                .tooltip("Toggle light and dark (Ctrl+T)")
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.toggle_theme(&ToggleTheme, window, cx)
                                })),
                        ),
                ),
        )
    }
}

impl Render for PlicApp {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        for image in self.stale_images.drain(..) {
            window.drop_image(image).ok();
        }
        // Every edit to a draft ends in a render, so this catches them all.
        self.remember_draft(cx);
        let theme = cx.theme();
        v_flex()
            .id("plic")
            .key_context(CONTEXT)
            .track_focus(&self.focus)
            .size_full()
            .bg(theme.background)
            .text_color(theme.foreground)
            .on_action(cx.listener(Self::select_previous))
            .on_action(cx.listener(Self::select_next))
            .on_action(cx.listener(Self::open_folder))
            .on_action(cx.listener(Self::reload))
            .on_action(cx.listener(Self::toggle_theme))
            .on_action(cx.listener(Self::toggle_play))
            .on_action(cx.listener(Self::seek_backward))
            .on_action(cx.listener(Self::seek_forward))
            .on_action(cx.listener(Self::shuttle_backward))
            .on_action(cx.listener(Self::shuttle_forward))
            .on_action(cx.listener(Self::step_backward))
            .on_action(cx.listener(Self::step_forward))
            .on_action(cx.listener(Self::jump_to_start))
            .on_action(cx.listener(Self::jump_to_end))
            .on_action(cx.listener(Self::toggle_mute))
            .on_action(cx.listener(Self::toggle_marker))
            .on_action(cx.listener(Self::next_marker))
            .on_action(cx.listener(Self::previous_marker))
            .on_action(cx.listener(Self::trim_to_markers))
            .on_action(cx.listener(Self::toggle_fullscreen))
            .on_action(cx.listener(Self::exit_fullscreen))
            .on_action(cx.listener(Self::mark_in))
            .on_action(cx.listener(Self::mark_out))
            .on_action(cx.listener(Self::reset_trim))
            .on_action(cx.listener(Self::add_segment_action))
            .on_action(cx.listener(Self::split_action))
            .on_action(cx.listener(Self::copy_last_export))
            .on_action(cx.listener(Self::save_frame_png))
            .on_action(cx.listener(Self::save_frame_jpeg))
            .on_action(cx.listener(Self::toggle_loop))
            .on_action(cx.listener(Self::jump_to_in))
            .on_action(cx.listener(Self::jump_to_out))
            .on_action(cx.listener(Self::export_clip))
            .on_action(cx.listener(Self::delete_clip))
            .on_action(cx.listener(Self::show_shortcuts))
            .on_action(cx.listener(Self::show_settings))
            .on_drop(cx.listener(|this, dropped: &ExternalPaths, window, cx| {
                this.handle_drop(dropped, window, cx)
            }))
            .when(!self.fullscreen, |this| {
                this.child(self.render_title_bar(cx))
                    .when(self.update.is_some(), |this| {
                        this.child(self.render_update_banner(cx))
                    })
            })
            .child(
                h_flex()
                    .flex_1()
                    .min_h_0()
                    .when(!self.fullscreen, |row| row.child(self.render_library(cx)))
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .h_full()
                            // Clicking the video leaves a text field, so the
                            // single-key shortcuts work again.
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|this, _, window, cx| this.focus.focus(window, cx)),
                            )
                            .when(self.viewed().is_some() && !self.fullscreen, |column| {
                                column.child(self.render_clip_header(cx))
                            })
                            .child(div().flex_1().min_h_0().child(self.player.clone())),
                    )
                    // Kept while exports run, so their progress stays visible.
                    .when(
                        !self.fullscreen && (self.current().is_some() || !self.jobs.is_empty()),
                        |row| row.child(self.render_export_panel(cx)),
                    ),
            )
    }
}

/// How far the arrow keys move the playhead, in milliseconds.
fn seek_step_ms() -> i64 {
    i64::from(crate::settings::get().seek_step_secs) * 1_000
}

pub(super) fn display_name(path: &Path) -> SharedString {
    library::display_name(path).into()
}

/// Reads a clip's metadata and decodes a thumbnail from early in the clip.
fn inspect(path: &Path) -> (Details, Option<RawFrame>) {
    let info = match ffmpeg::probe(path) {
        Ok(info) => info,
        Err(error) => return (Details::Failed(format!("{error:#}").into()), None),
    };
    // A second in skips fade-ins and black intro frames, but stay inside
    // very short clips.
    let at = info.duration.map_or(Duration::ZERO, |duration| {
        (duration / 10).min(Duration::from_secs(1))
    });
    let thumbnail = info
        .video_size
        .and_then(|size| ffmpeg::extract_keyframe(path, at, size.fit_within(THUMBNAIL_SIZE)).ok());
    (Details::Ready(info), thumbnail)
}
