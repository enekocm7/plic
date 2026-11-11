//! The main window: clip library, viewer and export panel.

use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::{
    ActiveTheme as _, Icon, Sizable as _, Theme, ThemeMode, TitleBar, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use super::{
    CONTEXT, JumpToEnd, JumpToStart, OpenFolder, Reload, SeekBackward, SeekForward, SelectNextClip,
    SelectPreviousClip, StepBackward, StepForward, TogglePlay, ToggleTheme, frame_image,
    player::Player,
};
use crate::{
    ffmpeg::{self, FrameSize, MediaInfo, RawFrame},
    library::{self, ClipFile, Library},
    worker::WorkerPool,
};

/// How far the arrow keys move the playhead, in milliseconds.
const SEEK_STEP_MS: i64 = 5_000;

/// Thumbnails are decoded at twice their on-screen size for HiDPI displays.
const THUMBNAIL_SIZE: FrameSize = FrameSize::new(160, 90);

/// Metadata that is loaded in the background after the folder is listed.
pub enum Details {
    Loading,
    Ready(MediaInfo),
    Failed(SharedString),
}

/// A clip in the library together with everything the UI tracks about it.
pub struct ClipEntry {
    pub file: ClipFile,
    pub details: Details,
    pub thumbnail: Option<Arc<RenderImage>>,
}

impl ClipEntry {
    fn new(file: ClipFile) -> Self {
        Self {
            file,
            details: Details::Loading,
            thumbnail: None,
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
    focus: FocusHandle,
    player: Entity<Player>,
    folder: Option<PathBuf>,
    pub(super) clips: Vec<ClipEntry>,
    folders: Vec<PathBuf>,
    pub(super) selected: Option<usize>,
    pub(super) list_scroll: UniformListScrollHandle,
    load_error: Option<SharedString>,
    workers: WorkerPool,
    /// Bumped on every folder load so results for an old folder are ignored.
    generation: u64,
    /// Images to evict from the GPU atlas on the next frame.
    stale_images: Vec<Arc<RenderImage>>,
}

impl PlicApp {
    pub fn new(folder: Option<PathBuf>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus = cx.focus_handle();
        focus.focus(window, cx);
        let player = cx.new(|_| Player::new());
        let mut app = Self {
            focus,
            player,
            folder: None,
            clips: Vec::new(),
            folders: Vec::new(),
            selected: None,
            list_scroll: UniformListScrollHandle::new(),
            load_error: None,
            workers: WorkerPool::new("inspect", 3),
            generation: 0,
            stale_images: Vec::new(),
        };
        if let Some(folder) = folder {
            app.load_folder(folder, cx);
        }
        app
    }

    fn load_folder(&mut self, folder: PathBuf, cx: &mut Context<Self>) {
        self.generation += 1;
        let old_clips = std::mem::take(&mut self.clips);
        self.stale_images
            .extend(old_clips.into_iter().filter_map(|entry| entry.thumbnail));
        match Library::scan(&folder) {
            Ok(library) => {
                self.clips = library.clips.into_iter().map(ClipEntry::new).collect();
                self.folders = library.folders;
                self.load_error = None;
            }
            Err(error) => {
                self.folders.clear();
                self.load_error = Some(format!("{error:#}").into());
            }
        }
        self.folder = Some(folder);
        self.selected = None;
        self.select(0, cx);
        for index in 0..self.clips.len() {
            self.load_details(index, cx);
        }
        cx.notify();
    }

    /// Probes a clip and decodes its thumbnail on the worker pool.
    fn load_details(&mut self, index: usize, cx: &mut Context<Self>) {
        let path = self.clips[index].file.path.clone();
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
                let Some(index) = this.index_of(&path) else {
                    return;
                };
                let entry = &mut this.clips[index];
                entry.details = details;
                entry.thumbnail = thumbnail;
                if this.selected == Some(index) {
                    this.show_selected_in_player(cx);
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    pub(super) fn select(&mut self, index: usize, cx: &mut Context<Self>) {
        if index >= self.clips.len() || self.selected == Some(index) {
            return;
        }
        self.selected = Some(index);
        self.list_scroll
            .scroll_to_item(index, ScrollStrategy::Nearest);
        self.show_selected_in_player(cx);
        cx.notify();
    }

    /// Loads the selected clip into the player once its details are known.
    fn show_selected_in_player(&mut self, cx: &mut Context<Self>) {
        let Some(entry) = self.current() else {
            self.player.update(cx, |player, cx| player.clear(cx));
            return;
        };
        let path = entry.file.path.clone();
        match &entry.details {
            Details::Ready(info) => {
                let info = info.clone();
                self.player
                    .update(cx, |player, cx| player.load(path, info, cx));
            }
            Details::Loading => self.player.update(cx, |player, cx| player.clear(cx)),
            Details::Failed(error) => {
                let message = format!("Could not read this video: {error}").into();
                self.player
                    .update(cx, |player, cx| player.show_error(message, cx));
            }
        }
    }

    fn toggle_play(&mut self, _: &TogglePlay, window: &mut Window, cx: &mut Context<Self>) {
        self.player
            .update(cx, |player, cx| player.toggle_play(window, cx));
    }

    fn seek_backward(&mut self, _: &SeekBackward, window: &mut Window, cx: &mut Context<Self>) {
        self.player
            .update(cx, |player, cx| player.seek_by(-SEEK_STEP_MS, window, cx));
    }

    fn seek_forward(&mut self, _: &SeekForward, window: &mut Window, cx: &mut Context<Self>) {
        self.player
            .update(cx, |player, cx| player.seek_by(SEEK_STEP_MS, window, cx));
    }

    fn step_backward(&mut self, _: &StepBackward, window: &mut Window, cx: &mut Context<Self>) {
        self.player
            .update(cx, |player, cx| player.step(-1, window, cx));
    }

    fn step_forward(&mut self, _: &StepForward, window: &mut Window, cx: &mut Context<Self>) {
        self.player
            .update(cx, |player, cx| player.step(1, window, cx));
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

    fn select_previous(&mut self, _: &SelectPreviousClip, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(selected) = self.selected {
            self.select(selected.saturating_sub(1), cx);
        }
    }

    fn select_next(&mut self, _: &SelectNextClip, _: &mut Window, cx: &mut Context<Self>) {
        let next = self.selected.map_or(0, |selected| selected + 1);
        self.select(next, cx);
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
                this.update(cx, |this, cx| this.load_folder(folder, cx))
                    .ok();
            }
        })
        .detach();
    }

    fn reload(&mut self, _: &Reload, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(folder) = self.folder.clone() {
            let current = self.current().map(|entry| entry.file.path.clone());
            self.load_folder(folder, cx);
            if let Some(index) = current.and_then(|path| self.index_of(&path)) {
                self.selected = None;
                self.select(index, cx);
            }
        }
    }

    fn toggle_theme(&mut self, _: &ToggleTheme, window: &mut Window, cx: &mut Context<Self>) {
        let mode = if cx.theme().mode.is_dark() {
            ThemeMode::Light
        } else {
            ThemeMode::Dark
        };
        Theme::change(mode, Some(window), cx);
    }

    pub(super) fn current(&self) -> Option<&ClipEntry> {
        self.clips.get(self.selected?)
    }

    fn index_of(&self, path: &Path) -> Option<usize> {
        self.clips.iter().position(|entry| entry.file.path == path)
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
                        .gap_1()
                        .child(
                            Button::new("open-folder")
                                .ghost()
                                .small()
                                .icon(IconName::FolderOpen)
                                .label("Open folder")
                                .tooltip("Open a clips folder (Ctrl+O)")
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.open_folder(&OpenFolder, window, cx)
                                })),
                        )
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
            .on_action(cx.listener(Self::step_backward))
            .on_action(cx.listener(Self::step_forward))
            .on_action(cx.listener(Self::jump_to_start))
            .on_action(cx.listener(Self::jump_to_end))
            .child(self.render_title_bar(cx))
            .child(
                h_flex()
                    .flex_1()
                    .min_h_0()
                    .child(self.render_library(cx))
                    .child(div().flex_1().min_w_0().h_full().child(self.player.clone())),
            )
    }
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
