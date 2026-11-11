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
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Icon, Selectable as _, Sizable as _, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use gpui_kit::component::slider::{Slider, SliderEvent, SliderState};

use super::{
    frame_image,
    timeline::{self, Drag, TimelineState},
};
use crate::{
    audio::AudioOutput,
    ffmpeg::{self, FrameSize, MediaInfo},
    playback::{MAX_FRAME_RATE, Playback},
    timestamp,
    trim::{self, TrimRange},
    worker::WorkerPool,
};

/// Decode size used before the viewer has been laid out.
const DEFAULT_VIEWPORT: FrameSize = FrameSize::new(1280, 720);

/// How long the viewport must stay the same size before playback restarts
/// at the new resolution; avoids restarting on every step of a resize drag.
const RESIZE_SETTLE: Duration = Duration::from_millis(250);

/// Thumbnails along the timeline.
const FILMSTRIP_TILES: usize = 12;
const FILMSTRIP_TILE_SIZE: FrameSize = FrameSize::new(192, 108);

/// Emitted when the user changes the trim range from the viewer.
pub enum PlayerEvent {
    TrimChanged(TrimRange),
}

impl EventEmitter<PlayerEvent> for Player {}

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

/// The audio device, opened on first playback.
enum AudioDevice {
    Unopened,
    Ready(AudioOutput),
    Unavailable(SharedString),
}

impl AudioDevice {
    fn output(&mut self) -> Option<&mut AudioOutput> {
        if matches!(self, Self::Unopened) {
            *self = match AudioOutput::open() {
                Ok(output) => Self::Ready(output),
                Err(error) => Self::Unavailable(format!("{error:#}").into()),
            };
        }
        match self {
            Self::Ready(output) => Some(output),
            _ => None,
        }
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
    /// Playback was running when a drag started and resumes when it ends.
    resume_after_drag: bool,
    drag: Option<Drag>,
    error: Option<SharedString>,
    trim: TrimRange,
    looping: bool,
    filmstrip: Vec<Option<Arc<RenderImage>>>,
    filmstrip_workers: WorkerPool,
    audio: AudioDevice,
    volume: f32,
    muted: bool,
    volume_slider: Entity<SliderState>,
}

impl Player {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let volume = 0.8;
        let volume_slider = cx.new(|_| {
            SliderState::new()
                .min(0.)
                .max(1.)
                .step(0.01)
                .default_value(volume)
        });
        cx.subscribe(&volume_slider, |this, _, event: &SliderEvent, cx| {
            let (SliderEvent::Change(value) | SliderEvent::Release(value)) = event;
            this.volume = value.end();
            this.muted = false;
            this.apply_volume();
            cx.notify();
        })
        .detach();
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
            resume_after_drag: false,
            drag: None,
            error: None,
            trim: TrimRange::default(),
            looping: false,
            filmstrip: Vec::new(),
            filmstrip_workers: WorkerPool::new("filmstrip", 2),
            audio: AudioDevice::Unopened,
            volume,
            muted: false,
            volume_slider,
        }
    }

    /// Loads a clip and shows the first frame of its trim range.
    pub fn load(
        &mut self,
        path: PathBuf,
        info: MediaInfo,
        trim: TrimRange,
        cx: &mut Context<Self>,
    ) {
        if self
            .source
            .as_ref()
            .is_some_and(|source| source.path == path)
        {
            return;
        }
        self.unload();
        self.source = Some(Source { path, info });
        self.trim = trim;
        self.position = trim.start;
        self.request_still(trim.start, cx);
        self.request_filmstrip(cx);
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
        self.drag = None;
        self.resume_after_drag = false;
        self.trim = TrimRange::default();
        self.stale_frames.extend(self.frame.take());
        self.stale_frames.extend(self.filmstrip.drain(..).flatten());
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
            let near_end = |end: Duration| from + source.info.frame_interval() >= end;
            if self.looping {
                // Looping plays the selection, so start inside it.
                let end = self.trim.resolved_end(source.info.duration);
                if from < self.trim.start || end.is_some_and(near_end) {
                    from = self.trim.start;
                }
            } else if source.info.duration.is_some_and(near_end) {
                // Pressing play at the end starts over.
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
        let audio = if source.info.has_audio {
            self.audio.output()
        } else {
            None
        };
        let result = Playback::start(&source.path, from, size, source.frame_rate(), audio);
        self.apply_volume();
        match result {
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
        if was_playing && self.drag.is_none() {
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

    pub fn toggle_mute(&mut self, cx: &mut Context<Self>) {
        self.muted = !self.muted;
        self.apply_volume();
        cx.notify();
    }

    fn apply_volume(&mut self) {
        if let AudioDevice::Ready(output) = &self.audio {
            output.set_volume(if self.muted { 0.0 } else { self.volume });
        }
    }

    /// Replaces the trim range, e.g. after it was typed into the editor.
    pub fn set_trim(&mut self, trim: TrimRange, cx: &mut Context<Self>) {
        if self.trim != trim {
            self.trim = trim;
            cx.notify();
        }
    }

    fn change_trim(&mut self, trim: TrimRange, cx: &mut Context<Self>) {
        if self.trim != trim {
            self.trim = trim;
            cx.emit(PlayerEvent::TrimChanged(trim));
            cx.notify();
        }
    }

    /// Starts the exported range at the playhead.
    pub fn mark_in(&mut self, cx: &mut Context<Self>) {
        if self.source.is_none() {
            return;
        }
        let start = self.position();
        // An in point past the out point keeps the start and drops the end.
        let end = self.trim.end.filter(|end| *end >= start + trim::MIN_LENGTH);
        self.change_trim(TrimRange::new(start, end), cx);
    }

    /// Ends the exported range at the playhead.
    pub fn mark_out(&mut self, cx: &mut Context<Self>) {
        if self.source.is_none() {
            return;
        }
        let end = self.position();
        // An out point before the in point keeps the end and drops the start.
        let start = if end >= self.trim.start + trim::MIN_LENGTH {
            self.trim.start
        } else {
            Duration::ZERO
        };
        self.change_trim(TrimRange::new(start, Some(end)), cx);
    }

    pub fn reset_trim(&mut self, cx: &mut Context<Self>) {
        self.change_trim(TrimRange::default(), cx);
    }

    pub fn toggle_loop(&mut self, cx: &mut Context<Self>) {
        self.looping = !self.looping;
        cx.notify();
    }

    pub fn jump_to_in(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.seek(self.trim.start, window, cx);
    }

    pub fn jump_to_out(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(end) = self.trim.resolved_end(self.duration()) {
            self.seek(end, window, cx);
        }
    }

    pub(super) fn begin_drag(&mut self, drag: Drag, cx: &mut Context<Self>) {
        self.resume_after_drag = self.is_playing();
        if let Some(playback) = self.playback.take() {
            self.position = playback.position();
        }
        self.drag = Some(drag);
        cx.notify();
    }

    /// Moves whatever is being dragged to `time`, previewing that frame.
    pub(super) fn drag_to(&mut self, time: Duration, window: &mut Window, cx: &mut Context<Self>) {
        let Some(drag) = self.drag else {
            return;
        };
        let duration = self.duration().unwrap_or(time);
        let time = time.min(duration);
        match drag {
            Drag::Playhead => {}
            Drag::TrimStart => {
                let limit = self
                    .trim
                    .resolved_end(Some(duration))
                    .unwrap_or(duration)
                    .saturating_sub(trim::MIN_LENGTH);
                self.change_trim(TrimRange::new(time.min(limit), self.trim.end), cx);
            }
            Drag::TrimEnd => {
                let end = time.max(self.trim.start + trim::MIN_LENGTH).min(duration);
                // Dragging to the very end means "until the end of the video".
                let end = (end + self.frame_interval() < duration).then_some(end);
                self.change_trim(TrimRange::new(self.trim.start, end), cx);
            }
        }
        let preview = match drag {
            Drag::Playhead => time,
            Drag::TrimStart => self.trim.start,
            Drag::TrimEnd => self.trim.resolved_end(Some(duration)).unwrap_or(time),
        };
        self.seek(preview, window, cx);
    }

    pub(super) fn end_drag(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.drag.take().is_none() {
            return;
        }
        if std::mem::take(&mut self.resume_after_drag) {
            self.play(window, cx);
        }
        cx.notify();
    }

    pub(super) fn timeline_state(&self) -> TimelineState {
        TimelineState {
            duration: self.duration(),
            position: self.position(),
            trim: self.trim,
            filmstrip: self.filmstrip.clone(),
            drag: self.drag,
            aspect_ratio: self
                .source
                .as_ref()
                .and_then(|source| source.info.video_size)
                .map_or(16. / 9., FrameSize::aspect_ratio),
        }
    }

    /// Decodes evenly spaced keyframes to draw along the timeline.
    fn request_filmstrip(&mut self, cx: &mut Context<Self>) {
        let Some(source) = &self.source else {
            return;
        };
        let (Some(duration), Some(video_size)) = (source.info.duration, source.info.video_size)
        else {
            return;
        };
        let size = video_size.fit_within(FILMSTRIP_TILE_SIZE);
        self.filmstrip = vec![None; FILMSTRIP_TILES];
        let generation = self.generation;
        for index in 0..FILMSTRIP_TILES {
            let at = duration.mul_f64((index as f64 + 0.5) / FILMSTRIP_TILES as f64);
            let path = source.path.clone();
            let job = self
                .filmstrip_workers
                .run(move || ffmpeg::extract_keyframe(&path, at, size));
            cx.spawn(async move |this, cx| {
                let Ok(Ok(frame)) = job.await else {
                    return;
                };
                let image = frame_image::render_image(frame);
                this.update(cx, |this, cx| {
                    if this.generation == generation
                        && let Some(slot) = this.filmstrip.get_mut(index)
                    {
                        *slot = image;
                        cx.notify();
                    }
                })
                .ok();
            })
            .detach();
        }
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
        if self.looping {
            let end = self.trim.resolved_end(self.duration());
            if finished || end.is_some_and(|end| self.position() >= end) {
                self.seek(self.trim.start, window, cx);
                return;
            }
        }
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
            .child(div().flex_1())
            .child(
                Button::new("loop")
                    .ghost()
                    .small()
                    .icon(IconName::Repeat)
                    .selected(self.looping)
                    .tooltip(if self.looping {
                        "Stop looping the selection (R)"
                    } else {
                        "Loop the selection (R)"
                    })
                    .disabled(!enabled)
                    .on_click(cx.listener(|this, _, _, cx| this.toggle_loop(cx))),
            )
            .child(self.render_volume(cx))
    }

    fn render_volume(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let has_audio = self
            .source
            .as_ref()
            .is_none_or(|source| source.info.has_audio);
        let (icon, tooltip): (IconName, SharedString) = match &self.audio {
            AudioDevice::Unavailable(error) => (
                IconName::VolumeX,
                format!("Audio unavailable: {error}").into(),
            ),
            _ if !has_audio => (IconName::VolumeX, "This clip has no audio".into()),
            _ if self.muted || self.volume == 0.0 => (IconName::VolumeX, "Unmute (M)".into()),
            _ if self.volume < 0.5 => (IconName::Volume1, "Mute (M)".into()),
            _ => (IconName::Volume2, "Mute (M)".into()),
        };
        h_flex()
            .gap_1()
            .child(
                Button::new("mute")
                    .ghost()
                    .small()
                    .icon(icon)
                    .tooltip(tooltip)
                    .on_click(cx.listener(|this, _, _, cx| this.toggle_mute(cx))),
            )
            .child(div().w(px(96.)).child(Slider::new(&self.volume_slider)))
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
                    .child(timeline::render(self.timeline_state(), cx))
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
