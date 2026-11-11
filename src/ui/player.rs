//! The video viewer: shows frames, plays clips back and handles seeking.

use std::{
    cell::Cell,
    path::PathBuf,
    rc::Rc,
    sync::Arc,
    time::{Duration, Instant},
};

use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::{ActiveTheme as _, Disableable as _, Icon, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use super::{frame_image, timeline};
use crate::{
    ffmpeg::{self, FrameSize, MediaInfo},
    playback::{MAX_FRAME_RATE, Playback},
    timestamp,
};

/// Decode size used before the viewer has been laid out.
const DEFAULT_VIEWPORT: FrameSize = FrameSize::new(1280, 720);

/// How long the viewport must stay the same size before playback restarts
/// at the new resolution; avoids restarting on every step of a resize drag.
const RESIZE_SETTLE: Duration = Duration::from_millis(250);

/// The clip loaded into the player.
struct Source {
    path: PathBuf,
    info: MediaInfo,
}

impl Source {
    fn frame_rate(&self) -> f64 {
        self.info.frame_rate.unwrap_or(30.0).min(MAX_FRAME_RATE)
    }
}

/// Single-frame decodes for seeking while paused. Only one runs at a time;
/// newer requests replace older queued ones so scrubbing never lags behind.
#[derive(Default)]
struct StillRequests {
    in_flight: bool,
    queued: Option<Duration>,
}

pub struct Player {
    source: Option<Source>,
    /// Bumped whenever the source changes, so late decodes are discarded.
    generation: u64,
    playback: Option<Playback>,
    /// The playhead while no playback is running.
    position: Duration,
    frame: Option<Arc<RenderImage>>,
    /// Size the current frame was decoded at.
    frame_size: Option<FrameSize>,
    /// Frames replaced since the last render, to evict from the GPU atlas.
    stale_frames: Vec<Arc<RenderImage>>,
    stills: StillRequests,
    /// The video area in device pixels, measured during layout.
    viewport: Rc<Cell<Option<FrameSize>>>,
    viewport_changed_at: Option<Instant>,
    tick_scheduled: bool,
    /// Playback was running when a scrub started and resumes when it ends.
    resume_after_scrub: bool,
    pub(super) scrubbing: bool,
    error: Option<SharedString>,
}

impl Player {
    pub fn new() -> Self {
        Self {
            source: None,
            generation: 0,
            playback: None,
            position: Duration::ZERO,
            frame: None,
            frame_size: None,
            stale_frames: Vec::new(),
            stills: StillRequests::default(),
            viewport: Rc::new(Cell::new(None)),
            viewport_changed_at: None,
            tick_scheduled: false,
            resume_after_scrub: false,
            scrubbing: false,
            error: None,
        }
    }

    /// Loads a clip and shows its first frame.
    pub fn load(&mut self, path: PathBuf, info: MediaInfo, cx: &mut Context<Self>) {
        if self
            .source
            .as_ref()
            .is_some_and(|source| source.path == path)
        {
            return;
        }
        self.unload();
        self.source = Some(Source { path, info });
        self.request_still(Duration::ZERO, cx);
        cx.notify();
    }

    /// Shows nothing, e.g. while a clip's details are still loading.
    pub fn clear(&mut self, cx: &mut Context<Self>) {
        self.unload();
        cx.notify();
    }

    pub fn show_error(&mut self, message: SharedString, cx: &mut Context<Self>) {
        self.unload();
        self.error = Some(message);
        cx.notify();
    }

    fn unload(&mut self) {
        self.generation += 1;
        self.source = None;
        self.playback = None;
        self.position = Duration::ZERO;
        self.stills = StillRequests::default();
        self.frame_size = None;
        self.error = None;
        self.scrubbing = false;
        self.resume_after_scrub = false;
        if let Some(frame) = self.frame.take() {
            self.stale_frames.push(frame);
        }
    }

    pub fn has_source(&self) -> bool {
        self.source.is_some()
    }

    pub fn duration(&self) -> Option<Duration> {
        self.source.as_ref()?.info.duration
    }

    pub fn frame_interval(&self) -> Duration {
        self.source
            .as_ref()
            .map_or(Duration::from_millis(33), |source| {
                source.info.frame_interval()
            })
    }

    pub fn position(&self) -> Duration {
        self.playback
            .as_ref()
            .map_or(self.position, Playback::position)
    }

    pub fn is_playing(&self) -> bool {
        self.playback.as_ref().is_some_and(Playback::is_playing)
    }

    pub fn toggle_play(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.is_playing() {
            self.pause(cx);
        } else {
            self.play(window, cx);
        }
    }

    pub fn play(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(source) = &self.source else {
            return;
        };
        if let Some(playback) = &mut self.playback {
            playback.resume();
        } else {
            let mut from = self.position;
            // Pressing play at the end starts over.
            if let Some(duration) = source.info.duration
                && from + source.info.frame_interval() >= duration
            {
                from = Duration::ZERO;
            }
            self.start_playback(from, cx);
        }
        self.schedule_tick(window, cx);
        cx.notify();
    }

    pub fn pause(&mut self, cx: &mut Context<Self>) {
        if let Some(playback) = &mut self.playback {
            playback.pause();
            self.position = playback.position();
        }
        cx.notify();
    }

    fn start_playback(&mut self, from: Duration, cx: &mut Context<Self>) {
        let Some(source) = &self.source else {
            return;
        };
        let size = self.decode_size(source);
        match Playback::start(&source.path, from, size, source.frame_rate()) {
            Ok(playback) => {
                self.position = from;
                self.playback = Some(playback);
                self.error = None;
            }
            Err(error) => {
                self.playback = None;
                self.error = Some(format!("{error:#}").into());
                cx.notify();
            }
        }
    }

    /// Moves the playhead, keeping playback running if it was.
    pub fn seek(&mut self, to: Duration, window: &mut Window, cx: &mut Context<Self>) {
        let Some(source) = &self.source else {
            return;
        };
        let last_frame = source
            .info
            .duration
            .map(|duration| duration.saturating_sub(source.info.frame_interval()));
        let to = last_frame.map_or(to, |last| to.min(last));

        let was_playing = self.is_playing();
        self.playback = None;
        self.position = to;
        if was_playing && !self.scrubbing {
            self.start_playback(to, cx);
            self.schedule_tick(window, cx);
        } else {
            self.request_still(to, cx);
        }
        cx.notify();
    }

    pub fn seek_by(&mut self, delta: i64, window: &mut Window, cx: &mut Context<Self>) {
        let position = self.position();
        let target = if delta < 0 {
            position.saturating_sub(Duration::from_millis(delta.unsigned_abs()))
        } else {
            position + Duration::from_millis(delta as u64)
        };
        self.seek(target, window, cx);
    }

    /// Steps a single frame; pauses first so the step is visible.
    pub fn step(&mut self, frames: i32, window: &mut Window, cx: &mut Context<Self>) {
        if self.is_playing() {
            self.pause(cx);
        }
        let interval = self.frame_interval();
        let position = self.position();
        let target = if frames < 0 {
            position.saturating_sub(interval * frames.unsigned_abs())
        } else {
            position + interval * frames as u32
        };
        self.seek(target, window, cx);
    }

    pub fn begin_scrub(&mut self, cx: &mut Context<Self>) {
        self.resume_after_scrub = self.is_playing();
        if let Some(playback) = self.playback.take() {
            self.position = playback.position();
        }
        self.scrubbing = true;
        cx.notify();
    }

    pub fn end_scrub(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.scrubbing = false;
        if std::mem::take(&mut self.resume_after_scrub) {
            self.play(window, cx);
        }
        cx.notify();
    }

    fn decode_size(&self, source: &Source) -> FrameSize {
        let viewport = self.viewport.get().unwrap_or(DEFAULT_VIEWPORT);
        source
            .info
            .video_size
            .unwrap_or(DEFAULT_VIEWPORT)
            .fit_within(viewport)
    }

    /// Decodes the exact frame at `at` for display while paused.
    fn request_still(&mut self, at: Duration, cx: &mut Context<Self>) {
        let Some(source) = &self.source else {
            return;
        };
        if source.info.video_size.is_none() {
            return;
        }
        if self.stills.in_flight {
            self.stills.queued = Some(at);
            return;
        }
        self.stills.in_flight = true;

        let path = source.path.clone();
        let size = self.decode_size(source);
        let generation = self.generation;
        cx.spawn(async move |this, cx| {
            let frame = cx
                .background_spawn(async move { ffmpeg::extract_frame(&path, at, size) })
                .await;
            this.update(cx, |this, cx| {
                if this.generation != generation {
                    return;
                }
                this.stills.in_flight = false;
                // A frame from before playback resumed would jump backwards.
                if this.playback.is_none() {
                    match frame {
                        Ok(frame) => this.show_frame(frame),
                        Err(error) => this.error = Some(format!("{error:#}").into()),
                    }
                }
                if let Some(next) = this.stills.queued.take() {
                    this.request_still(next, cx);
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn show_frame(&mut self, frame: ffmpeg::RawFrame) {
        self.frame_size = Some(frame.size);
        if let Some(image) = frame_image::render_image(frame)
            && let Some(old) = self.frame.replace(image)
        {
            self.stale_frames.push(old);
        }
    }

    /// Runs once per display frame while playing.
    fn schedule_tick(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.tick_scheduled {
            return;
        }
        self.tick_scheduled = true;
        let this = cx.entity().downgrade();
        window.on_next_frame(move |window, cx| {
            this.update(cx, |this, cx| {
                this.tick_scheduled = false;
                this.tick(window, cx);
            })
            .ok();
        });
    }

    fn tick(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(playback) = &mut self.playback else {
            return;
        };
        if !playback.is_playing() {
            return;
        }

        let mut changed = false;
        if let Some(frame) = playback.poll_frame() {
            self.show_frame(frame);
            changed = true;
        }

        let finished = self.playback.as_ref().is_some_and(Playback::is_finished);
        if finished {
            self.position = self.duration().unwrap_or(self.position());
            self.playback = None;
            cx.notify();
            return;
        }

        self.restart_if_resized(cx);
        if changed {
            cx.notify();
        }
        self.schedule_tick(window, cx);
    }

    /// Restarts playback at the new decode size once a resize has settled.
    fn restart_if_resized(&mut self, cx: &mut Context<Self>) {
        let (Some(source), Some(playback)) = (&self.source, &self.playback) else {
            return;
        };
        let Some(changed_at) = self.viewport_changed_at else {
            return;
        };
        if changed_at.elapsed() < RESIZE_SETTLE {
            return;
        }
        self.viewport_changed_at = None;
        if self.decode_size(source) != playback.frame_size() {
            let position = playback.position();
            self.start_playback(position, cx);
        }
    }

    fn viewport_resized(&mut self, cx: &mut Context<Self>) {
        let Some(source) = &self.source else {
            return;
        };
        if self.playback.is_some() {
            self.viewport_changed_at = Some(Instant::now());
        } else if self.frame_size != Some(self.decode_size(source)) {
            self.request_still(self.position, cx);
        }
    }

    fn render_stage(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let frame = self.frame.clone();
        let aspect = self
            .source
            .as_ref()
            .and_then(|source| source.info.video_size)
            .or(self.frame_size)
            .map(FrameSize::aspect_ratio);
        let viewport = self.viewport.clone();
        let this = cx.entity().downgrade();

        let overlay = match (&self.error, &self.source) {
            (Some(error), _) => Some((IconName::CircleAlert, error.clone())),
            (None, Some(source)) if source.info.video_size.is_none() => {
                Some((IconName::Volume2, "This clip has no video".into()))
            }
            (None, None) => Some((IconName::Film, "Select a clip to preview it".into())),
            _ => None,
        };

        div()
            .relative()
            .flex_1()
            .min_h_0()
            .bg(black())
            .child(
                canvas(
                    move |bounds, window, cx| {
                        let scale = window.scale_factor();
                        let size = FrameSize::new(
                            (f32::from(bounds.size.width) * scale).round() as u32,
                            (f32::from(bounds.size.height) * scale).round() as u32,
                        );
                        if size.width > 0 && size.height > 0 && viewport.get() != Some(size) {
                            viewport.set(Some(size));
                            cx.defer(move |cx| {
                                this.update(cx, |this, cx| this.viewport_resized(cx)).ok();
                            });
                        }
                    },
                    move |bounds, (), window, _| {
                        if let (Some(frame), Some(aspect)) = (frame, aspect) {
                            let target = letterbox(bounds, aspect);
                            window
                                .paint_image(target, target, Corners::default(), frame, 0, false)
                                .ok();
                        }
                    },
                )
                .size_full(),
            )
            .when_some(overlay, |stage, (icon, message)| {
                stage.child(
                    v_flex()
                        .absolute()
                        .inset_0()
                        .items_center()
                        .justify_center()
                        .gap_2()
                        .text_color(theme.muted_foreground)
                        .child(Icon::new(icon).large())
                        .child(div().text_sm().max_w(px(480.)).text_center().child(message)),
                )
            })
    }

    fn render_transport(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let enabled = self.source.is_some();
        let playing = self.is_playing();
        let position = self.position();
        let duration = self.duration();

        h_flex()
            .h(px(44.))
            .px_3()
            .gap_2()
            .flex_none()
            .child(
                Button::new("play")
                    .ghost()
                    .icon(if playing {
                        IconName::Pause
                    } else {
                        IconName::Play
                    })
                    .tooltip(if playing {
                        "Pause (Space)"
                    } else {
                        "Play (Space)"
                    })
                    .disabled(!enabled)
                    .on_click(cx.listener(|this, _, window, cx| this.toggle_play(window, cx))),
            )
            .child(
                Button::new("step-back")
                    .ghost()
                    .small()
                    .icon(IconName::StepBack)
                    .tooltip("Previous frame (,)")
                    .disabled(!enabled)
                    .on_click(cx.listener(|this, _, window, cx| this.step(-1, window, cx))),
            )
            .child(
                Button::new("step-forward")
                    .ghost()
                    .small()
                    .icon(IconName::StepForward)
                    .tooltip("Next frame (.)")
                    .disabled(!enabled)
                    .on_click(cx.listener(|this, _, window, cx| this.step(1, window, cx))),
            )
            .child(
                h_flex()
                    .ml_2()
                    .gap_1()
                    .text_sm()
                    .font_family(theme.mono_font_family.clone())
                    .child(
                        div()
                            .text_color(theme.foreground)
                            .child(timestamp::format_precise(position)),
                    )
                    .child(div().text_color(theme.muted_foreground).child("/"))
                    .child(
                        div()
                            .text_color(theme.muted_foreground)
                            .child(duration.map_or("--:--".into(), timestamp::format_precise)),
                    ),
            )
    }
}

impl Render for Player {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        for frame in self.stale_frames.drain(..) {
            window.drop_image(frame).ok();
        }
        let (background, border) = (cx.theme().background, cx.theme().border);
        v_flex()
            .size_full()
            .bg(background)
            .child(self.render_stage(cx))
            .child(
                v_flex()
                    .flex_none()
                    .border_t_1()
                    .border_color(border)
                    .child(timeline::render(self, cx))
                    .child(self.render_transport(cx)),
            )
    }
}

/// The largest rectangle of the given aspect ratio centred in `bounds`.
fn letterbox(bounds: Bounds<Pixels>, aspect: f32) -> Bounds<Pixels> {
    let width = f32::from(bounds.size.width);
    let height = f32::from(bounds.size.height);
    let (fitted_width, fitted_height) = if width / height > aspect {
        (height * aspect, height)
    } else {
        (width, width / aspect)
    };
    Bounds {
        origin: point(
            bounds.origin.x + px((width - fitted_width) / 2.0),
            bounds.origin.y + px((height - fitted_height) / 2.0),
        ),
        size: size(px(fitted_width), px(fitted_height)),
    }
}
