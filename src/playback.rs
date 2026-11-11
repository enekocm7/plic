//! Real-time playback: keeps decoded video frames in step with the audio.

use std::{
    path::Path,
    time::{Duration, Instant},
};

use anyhow::Result;

use crate::{
    audio::{AudioOutput, AudioSession},
    ffmpeg::{FrameSize, RawFrame, VideoStream},
};

/// Playback never decodes faster than this; higher rates only cost CPU.
pub const MAX_FRAME_RATE: f64 = 60.0;

/// How long playback waits for audio before starting without it.
const AUDIO_START_TIMEOUT: Duration = Duration::from_millis(750);

/// A monotonic clock that can be paused, used when there is no audio.
#[derive(Debug, Clone, Copy)]
struct WallClock {
    base: Duration,
    running_since: Option<Instant>,
}

impl WallClock {
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
}

/// Plays a clip from a given position until dropped.
///
/// Nothing moves until the first video frame and enough audio are decoded,
/// so decoder start-up is never mistaken for dropped frames. From then on
/// the audio device is the master clock; video frames are shown when their
/// time comes and skipped if the display falls behind.
pub struct Playback {
    video: VideoStream,
    /// The next frame, taken from the stream but not yet due.
    pending: Option<RawFrame>,
    audio: Option<AudioSession>,
    /// Used when there is no audio, and after the audio track runs out.
    clock: WallClock,
    created: Instant,
    started: bool,
    playing: bool,
}

impl Playback {
    pub fn start(
        path: &Path,
        from: Duration,
        size: FrameSize,
        frame_rate: f64,
        audio: Option<&mut AudioOutput>,
    ) -> Result<Self> {
        let frame_rate = frame_rate.clamp(1.0, MAX_FRAME_RATE);
        let video = VideoStream::spawn(path, from, size, frame_rate)?;
        // A clip whose audio cannot be decoded still plays, silently.
        let audio = audio.and_then(|output| output.start(path, from).ok());
        Ok(Self {
            video,
            pending: None,
            audio,
            clock: WallClock::new(from),
            created: Instant::now(),
            started: false,
            playing: true,
        })
    }

    /// The media time currently being presented.
    pub fn position(&self) -> Duration {
        match &self.audio {
            Some(audio) if self.started => audio.position(),
            _ => self.clock.now(),
        }
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
        if let Some(audio) = &self.audio {
            audio.set_playing(false);
        }
    }

    pub fn resume(&mut self) {
        self.playing = true;
        if self.started {
            self.run_clocks();
        }
    }

    fn run_clocks(&mut self) {
        match &self.audio {
            Some(audio) => audio.set_playing(true),
            None => self.clock.start(),
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
        if !self.started && self.ready_to_start() {
            self.started = true;
            if self.playing {
                self.run_clocks();
            }
        }
        self.fall_back_to_wall_clock_when_audio_ends();

        let now = self.position();
        let mut due = None;
        while self.pending.as_ref().is_some_and(|frame| frame.pts <= now) {
            due = self.pending.take();
            self.pending = self.video.try_next();
        }
        due
    }

    fn ready_to_start(&self) -> bool {
        let video_ready = self.pending.is_some() || self.video.is_finished();
        let audio_ready = self.audio.as_ref().is_none_or(AudioSession::is_ready)
            || self.created.elapsed() > AUDIO_START_TIMEOUT;
        video_ready && audio_ready
    }

    /// Audio tracks are often a little shorter than the video. Once the
    /// audio is used up, keep time with the wall clock instead of freezing.
    fn fall_back_to_wall_clock_when_audio_ends(&mut self) {
        let Some(audio) = &self.audio else {
            return;
        };
        if self.started && audio.is_exhausted() {
            self.clock = WallClock::new(audio.position());
            self.audio = None;
            if self.playing {
                self.clock.start();
            }
        }
    }

    /// True once every frame up to the end of the clip has been shown.
    pub fn is_finished(&self) -> bool {
        self.pending.is_none() && self.video.is_finished()
    }
}
