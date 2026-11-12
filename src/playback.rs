//! Real-time playback: keeps decoded video frames in step with the audio.

use std::{
    path::Path,
    time::{Duration, Instant},
};

use anyhow::Result;

use crate::{
    audio::{AudioOutput, AudioSession},
    ffmpeg::{FrameSize, MediaInfo, RawFrame, VideoStream},
};

/// Playback never decodes faster than this; higher rates only cost CPU.
pub const MAX_FRAME_RATE: f64 = 60.0;

/// The playback speeds offered, slowest first.
pub const SPEEDS: [f64; 4] = [0.5, 1.0, 1.5, 2.0];
const MIN_SPEED: f64 = 0.25;
const MAX_SPEED: f64 = 4.0;

/// How long playback waits for audio before starting without it.
const AUDIO_START_TIMEOUT: Duration = Duration::from_millis(750);

/// Video this far behind the clock means the decoder cannot keep up...
const BEHIND_LIMIT: Duration = Duration::from_millis(150);
/// ...if it stays behind this long; a single slow moment is not enough.
const BEHIND_GRACE: Duration = Duration::from_millis(500);

/// How hard playback works to keep up with a clip that decodes too slowly
/// for real time. Each step is tried in turn when the video falls behind.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord)]
pub enum Effort {
    /// The decoder [`MediaInfo::prefers_hardware_decode`] picks.
    #[default]
    Normal,
    /// Decodes on the GPU whatever the clip.
    Hardware,
    /// On the GPU, and only every other frame.
    HalfRate,
}

impl Effort {
    /// The next step for `info`, or `None` when there is none left.
    pub fn next(self, info: &MediaInfo) -> Option<Self> {
        match self {
            // Already on the GPU, so the next real step is fewer frames.
            Self::Normal if info.prefers_hardware_decode() => Some(Self::HalfRate),
            Self::Normal => Some(Self::Hardware),
            Self::Hardware => Some(Self::HalfRate),
            Self::HalfRate => None,
        }
    }

    fn hardware(self, info: &MediaInfo) -> bool {
        self > Self::Normal || info.prefers_hardware_decode()
    }
}

/// The rate frames are decoded at before any speed-up: the clip's own, up
/// to the most that is worth showing.
pub fn decode_rate(info: &MediaInfo) -> f64 {
    info.frame_rate.unwrap_or(30.0).min(MAX_FRAME_RATE)
}

/// A monotonic clock that can be paused, used when there is no audio.
#[derive(Debug, Clone, Copy)]
struct WallClock {
    base: Duration,
    running_since: Option<Instant>,
    /// Media seconds per real second.
    speed: f64,
}

impl WallClock {
    fn new(base: Duration, speed: f64) -> Self {
        Self {
            base,
            running_since: None,
            speed,
        }
    }

    fn now(&self) -> Duration {
        self.base
            + self
                .running_since
                .map_or(Duration::ZERO, |since| since.elapsed().mul_f64(self.speed))
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
    speed: f64,
    /// Since when the video has been [`BEHIND_LIMIT`] behind the clock.
    behind_since: Option<Instant>,
}

impl Playback {
    pub fn start(
        path: &Path,
        from: Duration,
        size: FrameSize,
        info: &MediaInfo,
        speed: f64,
        effort: Effort,
        audio: Option<&mut AudioOutput>,
    ) -> Result<Self> {
        let speed = speed.clamp(MIN_SPEED, MAX_SPEED);
        let timing = info.timing();
        // Faster playback shows more media frames per second; decode fewer
        // so the display rate stays within bounds.
        let mut frame_rate = decode_rate(info).clamp(1.0, MAX_FRAME_RATE / speed.max(1.0));
        if effort == Effort::HalfRate {
            frame_rate = (frame_rate / 2.0).max(1.0);
        }
        let video =
            VideoStream::spawn(path, from, size, frame_rate, timing, effort.hardware(info))?;
        // A clip whose audio cannot be decoded still plays, silently.
        let audio = audio.and_then(|output| output.start(path, from, speed, timing).ok());
        Ok(Self {
            video,
            pending: None,
            audio,
            clock: WallClock::new(from, speed),
            created: Instant::now(),
            started: false,
            playing: true,
            speed,
            behind_since: None,
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
        self.behind_since = None;
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
        self.track_lag(now, due.as_ref());
        due
    }

    /// Notes whether the decoder is keeping up: it is not when the newest
    /// frame it has delivered is already well past due.
    fn track_lag(&mut self, now: Duration, shown: Option<&RawFrame>) {
        let starved = self.pending.is_none() && !self.video.is_finished();
        let late = shown.is_some_and(|frame| now.saturating_sub(frame.pts) > BEHIND_LIMIT);
        if !self.started || !self.playing {
            self.behind_since = None;
        } else if starved && late {
            self.behind_since.get_or_insert_with(Instant::now);
        } else if shown.is_some() || self.pending.is_some() {
            self.behind_since = None;
        }
    }

    /// True once the video has lagged the sound for a while: the decoder
    /// cannot keep up, so playback should restart with more [`Effort`].
    pub fn is_behind(&self) -> bool {
        self.behind_since
            .is_some_and(|since| since.elapsed() > BEHIND_GRACE)
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
            self.clock = WallClock::new(audio.position(), self.speed);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wall_clock_runs_at_its_speed() {
        let mut clock = WallClock::new(Duration::from_secs(10), 2.0);
        clock.running_since = Instant::now().checked_sub(Duration::from_secs(3));
        let now = clock.now();
        assert!(
            now >= Duration::from_secs(16) && now < Duration::from_secs(17),
            "{now:?}"
        );
        clock.stop();
        assert!(clock.now() >= Duration::from_secs(16));
    }

    #[test]
    fn effort_steps_up_to_the_gpu_then_fewer_frames() {
        let h264 = MediaInfo {
            video_codec: Some("h264".into()),
            ..MediaInfo::default()
        };
        assert_eq!(Effort::Normal.next(&h264), Some(Effort::Hardware));
        assert_eq!(Effort::Hardware.next(&h264), Some(Effort::HalfRate));
        assert_eq!(Effort::HalfRate.next(&h264), None);
        assert!(!Effort::Normal.hardware(&h264));
        assert!(Effort::Hardware.hardware(&h264));

        // AV1 is on the GPU from the start.
        let av1 = MediaInfo {
            video_codec: Some("av1".into()),
            ..MediaInfo::default()
        };
        assert!(Effort::Normal.hardware(&av1));
        assert_eq!(Effort::Normal.next(&av1), Some(Effort::HalfRate));
    }
}
