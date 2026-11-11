//! Real-time playback: keeps decoded video frames in step with a clock.

use std::{
    path::Path,
    time::{Duration, Instant},
};

use anyhow::Result;

use crate::ffmpeg::{FrameSize, RawFrame, VideoStream};

/// Playback never decodes faster than this; higher rates only cost CPU.
pub const MAX_FRAME_RATE: f64 = 60.0;

/// A monotonic media clock that can be paused.
///
/// It does not start on its own: the first frame (or first audio sample)
/// starts it, so decoder start-up latency is never counted as playback.
#[derive(Debug, Clone, Copy)]
struct Clock {
    base: Duration,
    running_since: Option<Instant>,
}

impl Clock {
    fn new(base: Duration) -> Self {
        Self {
            base,
            running_since: None,
        }
    }

    fn now(&self) -> Duration {
        self.base
            + self
                .running_since
                .map_or(Duration::ZERO, |since| since.elapsed())
    }

    fn start(&mut self) {
        if self.running_since.is_none() {
            self.running_since = Some(Instant::now());
        }
    }

    fn stop(&mut self) {
        self.base = self.now();
        self.running_since = None;
    }

    fn is_running(&self) -> bool {
        self.running_since.is_some()
    }
}

/// Plays a clip from a given position until dropped.
pub struct Playback {
    video: VideoStream,
    /// The next frame, taken from the stream but not yet due.
    pending: Option<RawFrame>,
    clock: Clock,
    /// Whether the clock should run once frames are flowing.
    playing: bool,
    started: bool,
}

impl Playback {
    pub fn start(path: &Path, from: Duration, size: FrameSize, frame_rate: f64) -> Result<Self> {
        let frame_rate = frame_rate.clamp(1.0, MAX_FRAME_RATE);
        Ok(Self {
            video: VideoStream::spawn(path, from, size, frame_rate)?,
            pending: None,
            clock: Clock::new(from),
            playing: true,
            started: false,
        })
    }

    /// The media time currently being presented.
    pub fn position(&self) -> Duration {
        self.clock.now()
    }

    pub fn frame_size(&self) -> FrameSize {
        self.video.size()
    }

    pub fn is_playing(&self) -> bool {
        self.playing
    }

    pub fn pause(&mut self) {
        self.playing = false;
        self.clock.stop();
    }

    pub fn resume(&mut self) {
        self.playing = true;
        if self.started {
            self.clock.start();
        }
    }

    /// The newest frame that is due, skipping any the display missed.
    ///
    /// Call once per display frame. Returns `None` when the frame on screen is
    /// still current.
    pub fn poll_frame(&mut self) -> Option<RawFrame> {
        if self.pending.is_none() {
            self.pending = self.video.try_next();
        }
        if !self.started && self.pending.is_some() {
            // First frame decoded: from here on, time moves.
            self.started = true;
            if self.playing {
                self.clock.start();
            }
        }

        let now = self.clock.now();
        let mut due = None;
        while self.pending.as_ref().is_some_and(|frame| frame.pts <= now) {
            due = self.pending.take();
            self.pending = self.video.try_next();
        }
        due
    }

    /// True once every frame up to the end of the clip has been shown.
    pub fn is_finished(&self) -> bool {
        self.pending.is_none() && self.video.is_finished()
    }

    /// Whether the decoder has produced its first frame yet.
    pub fn has_started(&self) -> bool {
        self.started
    }
}
