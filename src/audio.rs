//! Audio output: one device stream for the app, fed by an ffmpeg process per
//! playback session.
//!
//! The device callback counts the frames it actually plays. That count is
//! the master clock during playback, so video follows what is heard even if
//! the decoder briefly falls behind.

use std::{
    io::Read,
    path::Path,
    process::{Child, Command, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering},
    },
    thread,
    time::Duration,
};

use anyhow::{Context, Result, anyhow};
use cpal::{
    FromSample, OutputCallbackInfo, SampleFormat, SizedSample, StreamConfig,
    traits::{DeviceTrait, HostTrait, StreamTrait},
};
use rtrb::{Consumer, Producer, RingBuffer};

use crate::ffmpeg::{CommandExt as _, Timing, seconds_arg};

/// Decoded audio buffered ahead of the device: about a second, enough to
/// ride out a busy moment without delaying seeks.
const BUFFER_SECONDS: f64 = 1.0;

/// State shared with the real-time callback. Everything here is lock-free
/// except `session`, which the callback only ever `try_lock`s.
struct Shared {
    session: Mutex<Option<CallbackSession>>,
    playing: AtomicBool,
    volume: AtomicU32,
    /// Frames played for the current session.
    played_frames: AtomicU64,
    /// Output latency reported by the device, in microseconds.
    latency_us: AtomicU64,
}

struct CallbackSession {
    samples: Consumer<f32>,
    id: u64,
}

impl Shared {
    fn volume(&self) -> f32 {
        f32::from_bits(self.volume.load(Ordering::Relaxed))
    }
}

/// The app's connection to the default output device.
pub struct AudioOutput {
    shared: Arc<Shared>,
    sample_rate: u32,
    channels: u16,
    next_session: u64,
    // Dropping the stream stops output; it must outlive every session.
    _stream: cpal::Stream,
}

impl AudioOutput {
    /// Opens the default output device.
    pub fn open() -> Result<Self> {
        let device = cpal::default_host()
            .default_output_device()
            .ok_or_else(|| anyhow!("no audio output device"))?;
        let supported = device
            .default_output_config()
            .context("could not query the audio device")?;
        let config: StreamConfig = supported.config();
        let shared = Arc::new(Shared {
            session: Mutex::new(None),
            playing: AtomicBool::new(false),
            volume: AtomicU32::new(1.0f32.to_bits()),
            played_frames: AtomicU64::new(0),
            latency_us: AtomicU64::new(0),
        });

        let stream = match supported.sample_format() {
            SampleFormat::F32 => build_stream::<f32>(&device, &config, &shared),
            SampleFormat::I16 => build_stream::<i16>(&device, &config, &shared),
            SampleFormat::U16 => build_stream::<u16>(&device, &config, &shared),
            SampleFormat::I32 => build_stream::<i32>(&device, &config, &shared),
            format => Err(anyhow!("unsupported audio sample format {format}")),
        }?;
        stream.play().context("could not start audio output")?;

        Ok(Self {
            shared,
            sample_rate: config.sample_rate,
            channels: config.channels,
            next_session: 0,
            _stream: stream,
        })
    }

    pub fn set_volume(&self, volume: f32) {
        self.shared
            .volume
            .store(volume.clamp(0.0, 1.0).to_bits(), Ordering::Relaxed);
    }

    /// Starts decoding the audio of `path` from `from`. The new session
    /// replaces any previous one and stays silent until [`AudioSession::set_playing`].
    ///
    /// At a `speed` other than 1 the sound is time-stretched, keeping its
    /// pitch.
    pub fn start(
        &mut self,
        path: &Path,
        from: Duration,
        speed: f64,
        timing: Timing,
    ) -> Result<AudioSession> {
        let capacity = (self.sample_rate as f64 * self.channels as f64 * BUFFER_SECONDS) as usize;
        let (producer, consumer) = RingBuffer::new(capacity);
        let feeder = Feeder::spawn(
            path,
            from,
            speed,
            timing,
            self.sample_rate,
            self.channels,
            producer,
        )?;

        self.next_session += 1;
        let id = self.next_session;
        self.shared.playing.store(false, Ordering::Relaxed);
        {
            // Swap and reset under the lock: the callback counts frames while
            // holding it, so none can be credited to the wrong session.
            let mut session = self
                .shared
                .session
                .lock()
                .unwrap_or_else(|poison| poison.into_inner());
            *session = Some(CallbackSession {
                samples: consumer,
                id,
            });
            self.shared.played_frames.store(0, Ordering::Relaxed);
        }

        Ok(AudioSession {
            shared: Arc::clone(&self.shared),
            feeder,
            id,
            from,
            speed,
            sample_rate: self.sample_rate,
        })
    }
}

/// One playback session's audio, started at a given position.
pub struct AudioSession {
    shared: Arc<Shared>,
    feeder: Feeder,
    id: u64,
    from: Duration,
    /// Media seconds per second of sound played.
    speed: f64,
    sample_rate: u32,
}

impl AudioSession {
    pub fn set_playing(&self, playing: bool) {
        self.shared.playing.store(playing, Ordering::Relaxed);
    }

    /// The media time the listener is hearing right now.
    pub fn position(&self) -> Duration {
        let frames = self.shared.played_frames.load(Ordering::Relaxed);
        let played = Duration::from_secs_f64(frames as f64 / self.sample_rate as f64);
        let latency = Duration::from_micros(self.shared.latency_us.load(Ordering::Relaxed));
        self.from + played.saturating_sub(latency).mul_f64(self.speed)
    }

    /// True once enough audio is decoded to start without an audible gap.
    pub fn is_ready(&self) -> bool {
        self.feeder.buffered_frames() >= u64::from(self.sample_rate / 20) || self.feeder.is_done()
    }

    /// True once the decoder has finished and everything has been played.
    pub fn is_exhausted(&self) -> bool {
        self.feeder.is_done() && self.remaining_samples() == 0
    }

    fn remaining_samples(&self) -> usize {
        match self.shared.session.try_lock() {
            Ok(guard) => guard
                .as_ref()
                .filter(|session| session.id == self.id)
                .map_or(0, |session| session.samples.slots()),
            // The callback holds the lock; assume samples remain.
            Err(_) => 1,
        }
    }
}

impl Drop for AudioSession {
    fn drop(&mut self) {
        self.shared.playing.store(false, Ordering::Relaxed);
        if let Ok(mut session) = self.shared.session.lock()
            && session
                .as_ref()
                .is_some_and(|session| session.id == self.id)
        {
            *session = None;
        }
    }
}

fn build_stream<T>(
    device: &cpal::Device,
    config: &StreamConfig,
    shared: &Arc<Shared>,
) -> Result<cpal::Stream>
where
    T: SizedSample + FromSample<f32>,
{
    let shared = Arc::clone(shared);
    let channels = usize::from(config.channels.max(1));
    let stream = device.build_output_stream(
        config,
        move |output: &mut [T], info: &OutputCallbackInfo| {
            fill_output(output, channels, &shared);
            let timestamp = info.timestamp();
            if let Some(latency) = timestamp.playback.duration_since(&timestamp.callback) {
                shared
                    .latency_us
                    .store(latency.as_micros() as u64, Ordering::Relaxed);
            }
        },
        |error| eprintln!("audio output error: {error}"),
        None,
    )?;
    Ok(stream)
}

/// The real-time callback: copies decoded samples to the device, or silence.
fn fill_output<T: SizedSample + FromSample<f32>>(
    output: &mut [T],
    channels: usize,
    shared: &Shared,
) {
    let silence = T::from_sample(0.0f32);
    let mut written = 0;
    if shared.playing.load(Ordering::Relaxed)
        && let Ok(mut session) = shared.session.try_lock()
        && let Some(session) = session.as_mut()
    {
        let volume = shared.volume();
        // Whole frames only, so channels never get out of step.
        let available = session.samples.slots().min(output.len());
        let available = available - available % channels;
        if let Ok(chunk) = session.samples.read_chunk(available) {
            let (first, second) = chunk.as_slices();
            for (slot, sample) in output.iter_mut().zip(first.iter().chain(second)) {
                *slot = T::from_sample(sample * volume);
            }
            chunk.commit_all();
            written = available;
        }
        shared
            .played_frames
            .fetch_add((written / channels) as u64, Ordering::Relaxed);
    }
    output[written..].fill(silence);
}

/// The ffmpeg command that decodes `path` from `from` to raw `f32le`
/// samples on stdout, ready to spawn.
///
/// The first sample is exactly the one at `from`: ffmpeg keeps the
/// container's own timestamps (`-copyts`), and `aresample` fills the start
/// with silence when the audio begins after `from`, or trims it when it
/// begins before. Without that, a recording whose audio starts a little
/// after its video would play early by the difference.
pub(crate) fn feeder_command(
    path: &Path,
    from: Duration,
    speed: f64,
    timing: Timing,
    sample_rate: u32,
    channels: u16,
) -> Command {
    let first_sample = (timing.container_time(from) * f64::from(sample_rate)).round() as i64;
    let mut filters = format!("aresample=async=1:first_pts={first_sample}");
    if (speed - 1.0).abs() > f64::EPSILON {
        filters.push_str(&format!(",atempo={speed:.3}"));
    }
    let mut command = crate::ffmpeg::command(crate::ffmpeg::Tool::Ffmpeg);
    command
        .args(["-hide_banner", "-loglevel", "error", "-nostdin"])
        .arg("-ss")
        .arg(seconds_arg(from))
        .arg("-copyts")
        .arg("-i")
        .arg(path)
        .args(["-vn", "-sn", "-af"])
        .arg(filters)
        .arg("-ac")
        .arg(channels.to_string())
        .arg("-ar")
        .arg(sample_rate.to_string())
        .args(["-f", "f32le", "pipe:1"])
        .no_window()
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    command
}

/// Runs ffmpeg and pushes its decoded samples into the ring buffer.
struct Feeder {
    child: Option<Child>,
    state: Arc<FeederState>,
}

#[derive(Default)]
struct FeederState {
    done: AtomicBool,
    stop: AtomicBool,
    pushed_frames: AtomicU64,
}

impl Feeder {
    fn spawn(
        path: &Path,
        from: Duration,
        speed: f64,
        timing: Timing,
        sample_rate: u32,
        channels: u16,
        producer: Producer<f32>,
    ) -> Result<Self> {
        let mut child = feeder_command(path, from, speed, timing, sample_rate, channels)
            .spawn()
            .context("could not start ffmpeg for audio")?;
        let stdout = child.stdout.take().context("ffmpeg has no stdout")?;

        let state = Arc::new(FeederState::default());
        let thread_state = Arc::clone(&state);
        thread::Builder::new()
            .name("audio decode".into())
            .spawn(move || feed(stdout, producer, channels, &thread_state))
            .context("could not start the audio thread")?;
        Ok(Self {
            child: Some(child),
            state,
        })
    }

    fn is_done(&self) -> bool {
        self.state.done.load(Ordering::Relaxed)
    }

    fn buffered_frames(&self) -> u64 {
        self.state.pushed_frames.load(Ordering::Relaxed)
    }
}

impl Drop for Feeder {
    fn drop(&mut self) {
        self.state.stop.store(true, Ordering::Relaxed);
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            thread::spawn(move || {
                let _ = child.wait();
            });
        }
    }
}

fn feed(mut stdout: impl Read, mut producer: Producer<f32>, channels: u16, state: &FeederState) {
    let mut bytes = vec![0u8; 16 * 1024];
    let mut samples = Vec::with_capacity(bytes.len() / 4);
    // ffmpeg may split a sample across reads; carry the remainder over.
    let mut carry = 0;
    'decode: loop {
        let read = match stdout.read(&mut bytes[carry..]) {
            Ok(0) | Err(_) => break,
            Ok(read) => read,
        };
        let filled = carry + read;
        let whole = filled - filled % 4;
        samples.clear();
        let (frames, _) = bytes[..whole].as_chunks::<4>();
        samples.extend(frames.iter().map(|sample| f32::from_le_bytes(*sample)));
        bytes.copy_within(whole..filled, 0);
        carry = filled - whole;

        let mut pending = samples.as_slice();
        while !pending.is_empty() {
            if state.stop.load(Ordering::Relaxed) || producer.is_abandoned() {
                break 'decode;
            }
            let (pushed, rest) = producer.push_partial_slice(pending);
            state.pushed_frames.fetch_add(
                (pushed.len() / usize::from(channels.max(1))) as u64,
                Ordering::Relaxed,
            );
            pending = rest;
            if !pending.is_empty() {
                // The buffer is full: playback is paused or far ahead.
                thread::sleep(Duration::from_millis(10));
            }
        }
    }
    state.done.store(true, Ordering::Relaxed);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn arguments(command: &Command) -> Vec<String> {
        command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn decodes_from_the_container_timestamp_of_the_start() {
        let timing = Timing {
            origin: 0.5,
            ..Timing::default()
        };
        let command = feeder_command(
            Path::new("clip.mp4"),
            Duration::from_millis(2_500),
            1.0,
            timing,
            48_000,
            2,
        );
        let arguments = arguments(&command);
        let position = |flag: &str| arguments.iter().position(|arg| arg == flag).unwrap();
        assert_eq!(arguments[position("-ss") + 1], "2.500");
        assert!(arguments.contains(&"-copyts".to_string()));
        // 3.0 seconds on the container clock, in samples.
        assert_eq!(
            arguments[position("-af") + 1],
            "aresample=async=1:first_pts=144000"
        );

        let fast = feeder_command(
            Path::new("clip.mp4"),
            Duration::ZERO,
            1.5,
            timing,
            44_100,
            2,
        );
        assert_eq!(
            arguments_of(&fast, "-af"),
            "aresample=async=1:first_pts=22050,atempo=1.500"
        );
    }

    fn arguments_of(command: &Command, flag: &str) -> String {
        let arguments = arguments(command);
        let at = arguments.iter().position(|arg| arg == flag).unwrap();
        arguments[at + 1].clone()
    }
}
