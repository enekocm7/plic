//! Benchmarks and A/V sync measurements for the preview and export paths.
//!
//! Nothing here runs with a plain `cargo test`; every test is `#[ignore]`d
//! because it needs ffmpeg and takes a while. Run them in release mode:
//!
//! ```text
//! cargo test --release bench_ -- --ignored --nocapture --test-threads=1
//! ```
//!
//! Each benchmark prints one aligned table (median and p90 over its runs)
//! and writes the same rows as JSON lines to
//! `target/bench/<label>/<name>.json`, where `<label>` is the
//! `PLIC_BENCH_LABEL` environment variable (default `current`), so a run
//! before and after a change can be compared line by line.
//!
//! The sync tests measure accuracy rather than speed. Their fixtures flash
//! the whole picture white for four frames (enough to survive a frame-rate
//! cap) and play a 1 kHz beep at
//! the same instants, so the time the pipeline assigns to each flash and
//! each beep, compared with the instant it was authored at, is the A/V
//! offset a viewer would see.

use std::{
    fmt::Write as _,
    fs,
    io::Read,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use crate::{
    audio::feeder_command,
    ffmpeg::{
        self, Animation, AudioAdjust, CancelToken, CommandExt as _, ExportMode, ExportRequest,
        Format, FrameSize, MediaInfo, OutputLimits, RawFrame, Timing, Tool, Transform,
        VideoEncoding, VideoStream,
    },
    trim::TrimRange,
    worker::WorkerPool,
};

const SAMPLE_RATE: u32 = 48_000;
/// The size the player decodes a 1080p clip at in a typical window.
const VIEW: FrameSize = FrameSize::new(1280, 720);

// ---------------------------------------------------------------------------
// Results

struct Row {
    name: String,
    unit: &'static str,
    values: Vec<f64>,
}

impl Row {
    fn sorted(&self) -> Vec<f64> {
        let mut values: Vec<f64> = self
            .values
            .iter()
            .copied()
            .filter(|v| !v.is_nan())
            .collect();
        values.sort_by(f64::total_cmp);
        values
    }

    fn median(&self) -> f64 {
        let values = self.sorted();
        match values.len() {
            0 => f64::NAN,
            n if n % 2 == 1 => values[n / 2],
            n => (values[n / 2 - 1] + values[n / 2]) / 2.0,
        }
    }

    /// Nearest-rank 90th percentile.
    fn p90(&self) -> f64 {
        let values = self.sorted();
        match values.len() {
            0 => f64::NAN,
            n => values[((n as f64 * 0.9).ceil() as usize).clamp(1, n) - 1],
        }
    }

    fn max(&self) -> f64 {
        self.sorted().last().copied().unwrap_or(f64::NAN)
    }
}

struct Report {
    name: &'static str,
    rows: Vec<Row>,
}

impl Report {
    fn new(name: &'static str) -> Self {
        Self {
            name,
            rows: Vec::new(),
        }
    }

    fn add(&mut self, name: impl Into<String>, unit: &'static str, values: Vec<f64>) {
        let name = name.into();
        let row = Row { name, unit, values };
        println!(
            "  {:<58} n={:<3} median {:>9} p90 {:>9} {}",
            row.name,
            row.values.len(),
            number(row.median()),
            number(row.p90()),
            row.unit
        );
        self.rows.push(row);
    }

    fn finish(self) {
        let width = self.rows.iter().map(|r| r.name.len()).max().unwrap_or(0);
        println!("\n== {} ==", self.name);
        println!(
            "{:<width$}  {:>3}  {:>10}  {:>10}  {:>10}  unit",
            "metric", "n", "median", "p90", "max"
        );
        let mut json = String::new();
        for row in &self.rows {
            println!(
                "{:<width$}  {:>3}  {:>10}  {:>10}  {:>10}  {}",
                row.name,
                row.values.len(),
                number(row.median()),
                number(row.p90()),
                number(row.max()),
                row.unit
            );
            let values: Vec<String> = row.values.iter().map(|v| json_number(*v)).collect();
            writeln!(
                json,
                "{{\"name\":\"{}\",\"unit\":\"{}\",\"n\":{},\"median\":{},\"p90\":{},\"max\":{},\"values\":[{}]}}",
                row.name,
                row.unit,
                row.values.len(),
                json_number(row.median()),
                json_number(row.p90()),
                json_number(row.max()),
                values.join(",")
            )
            .unwrap();
        }
        let label = std::env::var("PLIC_BENCH_LABEL").unwrap_or_else(|_| "current".into());
        let dir = bench_dir().join(label);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join(format!("{}.json", self.name)), json).unwrap();
        println!();
    }
}

fn number(value: f64) -> String {
    if value.is_nan() {
        "n/a".into()
    } else {
        format!("{value:.2}")
    }
}

fn json_number(value: f64) -> String {
    if value.is_finite() {
        format!("{value:.4}")
    } else {
        "null".into()
    }
}

fn ms(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1000.0
}

fn secs(seconds: f64) -> Duration {
    Duration::from_secs_f64(seconds.max(0.0))
}

// ---------------------------------------------------------------------------
// Fixtures

fn bench_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("target")
        .join("bench")
}

fn ffmpeg_command() -> Command {
    let mut command = ffmpeg::command(Tool::Ffmpeg);
    command.args(["-hide_banner", "-loglevel", "error", "-nostdin", "-y"]);
    command
}

/// Where the marks of a sync fixture are: `first + k * period` seconds.
#[derive(Clone, Copy)]
struct Marks {
    first: f64,
    period: f64,
}

impl Marks {
    fn index(self, time: f64) -> i64 {
        ((time - self.first) / self.period).round() as i64
    }

    /// How far `time` is from the mark nearest to it.
    fn error(self, time: f64) -> f64 {
        time - (self.first + self.index(time) as f64 * self.period)
    }
}

struct Fixture {
    path: PathBuf,
    marks: Marks,
}

struct Sync {
    name: &'static str,
    size: (u32, u32),
    rate: &'static str,
    /// Frames between flashes.
    period_frames: u32,
    /// The same interval in seconds, to the millisecond.
    period: f64,
    duration: u32,
    /// The video stream starts this late, in seconds.
    video_offset: f64,
    /// The audio streams start this late, in seconds.
    audio_offset: f64,
    audio_tracks: u32,
}

const SYNC_MARKS: Sync = Sync {
    name: "sync_marks.mp4",
    size: (1280, 720),
    rate: "60",
    period_frames: 300,
    period: 5.0,
    duration: 120,
    video_offset: 0.0,
    audio_offset: 0.0,
    audio_tracks: 2,
};
const SYNC_MARKS_2997: Sync = Sync {
    name: "sync_marks_2997.mp4",
    rate: "30000/1001",
    period_frames: 150,
    period: 5.005,
    ..SYNC_MARKS
};
const SYNC_AUDIO_LATE: Sync = Sync {
    name: "sync_offset.mp4",
    audio_offset: 0.25,
    ..SYNC_MARKS
};
const SYNC_VIDEO_LATE: Sync = Sync {
    name: "sync_offset_video.mp4",
    video_offset: 0.25,
    ..SYNC_MARKS
};
const LONG_10MIN: Sync = Sync {
    name: "long_10min.mp4",
    size: (320, 180),
    rate: "30000/1001",
    period_frames: 900,
    period: 30.03,
    duration: 600,
    video_offset: 0.0,
    audio_offset: 0.0,
    audio_tracks: 1,
};

fn fixture_path(name: &str) -> PathBuf {
    bench_dir().join("fixtures").join(name)
}

/// Builds `name` with `build` unless it exists already.
fn ensure(name: &str, build: impl FnOnce(&Path) -> bool) -> Option<PathBuf> {
    let path = fixture_path(name);
    if path.exists() {
        return Some(path);
    }
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let partial = path.with_extension("partial.mp4");
    println!("generating fixture {name}...");
    let started = Instant::now();
    if !build(&partial) {
        let _ = fs::remove_file(&partial);
        println!("  could not generate {name}; skipping");
        return None;
    }
    fs::rename(&partial, &path).unwrap();
    println!("  done in {:.1}s", started.elapsed().as_secs_f64());
    Some(path)
}

fn run(mut command: Command) -> bool {
    let output = command.no_window().output().unwrap();
    if !output.status.success() {
        println!("{}", String::from_utf8_lossy(&output.stderr));
    }
    output.status.success()
}

fn h264_1080p60() -> Option<PathBuf> {
    ensure("h264_1080p60.mp4", |out| {
        let mut command = ffmpeg_command();
        command
            .args(["-f", "lavfi", "-i", "testsrc2=s=1920x1080:r=60:d=60"])
            .args(["-f", "lavfi", "-i", "sine=f=440:r=48000:d=60"])
            .args(["-c:v", "libx264", "-preset", "veryfast", "-crf", "23"])
            .args(["-g", "120", "-keyint_min", "120", "-sc_threshold", "0"])
            .args([
                "-pix_fmt", "yuv420p", "-c:a", "aac", "-b:a", "160k", "-ac", "2",
            ])
            .arg(out);
        run(command)
    })
}

fn hevc_1440p60() -> Option<PathBuf> {
    ensure("hevc_1440p60.mp4", |out| {
        let mut command = ffmpeg_command();
        command
            .args(["-f", "lavfi", "-i", "testsrc2=s=2560x1440:r=60:d=30"])
            .args(["-f", "lavfi", "-i", "sine=f=440:r=48000:d=30"])
            .args(["-c:v", "libx265", "-preset", "ultrafast", "-crf", "26"])
            .args(["-x265-params", "keyint=120:min-keyint=120:log-level=error"])
            .args(["-tag:v", "hvc1", "-pix_fmt", "yuv420p"])
            .args(["-c:a", "aac", "-b:a", "160k", "-ac", "2"])
            .arg(out);
        run(command)
    })
}

fn hevc_2160p60() -> Option<PathBuf> {
    ensure("hevc_2160p60.mp4", |out| {
        let mut command = ffmpeg_command();
        command
            .args(["-f", "lavfi", "-i", "testsrc2=s=3840x2160:r=60:d=25"])
            .args(["-f", "lavfi", "-i", "sine=f=440:r=48000:d=25"])
            .args(["-c:v", "libx265", "-preset", "ultrafast", "-crf", "26"])
            .args(["-x265-params", "keyint=120:min-keyint=120:log-level=error"])
            .args(["-tag:v", "hvc1", "-pix_fmt", "yuv420p"])
            .args(["-c:a", "aac", "-b:a", "160k", "-ac", "2"])
            .arg(out);
        run(command)
    })
}

/// A clip whose picture flashes and whose sound beeps together every
/// `period`, starting at `video_offset`. The first frame never flashes, so
/// a player that holds it while the video has not started yet shows no
/// false mark.
fn sync_fixture(spec: &Sync) -> Fixture {
    let path = ensure(spec.name, |out| {
        let (width, height) = spec.size;
        let video_length = spec.duration as f64 - spec.video_offset;
        let video = format!(
            "color=c=black:s={width}x{height}:r={}:d={video_length},\
             drawbox=x=0:y=0:w=iw:h=ih:color=white:t=fill:enable='gte(n,{1})*lt(mod(n,{1}),4)',\
             format=yuv420p",
            spec.rate, spec.period_frames
        );
        let audio_length = spec.duration as f64 - spec.audio_offset;
        let mut command = ffmpeg_command();
        command
            .arg("-itsoffset")
            .arg(spec.video_offset.to_string())
            .args(["-f", "lavfi", "-i"])
            .arg(video);
        let pitches = [1000.0, 1500.0];
        for pitch in &pitches[..spec.audio_tracks as usize] {
            // `x` is the time since the first mark, `t` the stream's own time.
            let x = format!("(t+{}-{}+0.000001)", spec.audio_offset, spec.video_offset);
            let beep = format!(
                "if(gte({x},0)*lt(mod({x},{}),0.02),0.4*sin(2*PI*{pitch}*mod({x},{})),0)",
                spec.period, spec.period
            );
            command
                .arg("-itsoffset")
                .arg(spec.audio_offset.to_string())
                .args(["-f", "lavfi", "-i"])
                .arg(format!(
                    "aevalsrc='{beep}|{beep}':s={SAMPLE_RATE}:d={audio_length}"
                ));
        }
        command.args(["-map", "0:v"]);
        for track in 0..spec.audio_tracks {
            command.arg("-map").arg(format!("{}:a", track + 1));
        }
        command
            .args(["-c:v", "libx264", "-preset", "veryfast", "-crf", "18"])
            .args(["-g", "120", "-sc_threshold", "0", "-pix_fmt", "yuv420p"])
            .args(["-c:a", "aac", "-b:a", "128k"])
            .arg(out);
        run(command)
    })
    .unwrap_or_else(|| panic!("could not make {}", spec.name));
    Fixture {
        path,
        marks: Marks {
            first: spec.video_offset,
            period: spec.period,
        },
    }
}

fn info_of(path: &Path) -> MediaInfo {
    ffmpeg::probe(path).unwrap()
}

// ---------------------------------------------------------------------------
// Measurement helpers

/// A cheap deterministic random sequence.
struct Lcg(u64);

impl Lcg {
    fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        self.0 >> 33
    }
}

fn luma(frame: &RawFrame) -> f64 {
    let sum: u64 = frame
        .bgra
        .as_chunks::<4>()
        .0
        .iter()
        .map(|px| u64::from(px[0]) + u64::from(px[1]) + u64::from(px[2]))
        .sum();
    sum as f64 / (frame.bgra.len() / 4) as f64 / 3.0
}

fn wait_for_frame(stream: &mut VideoStream) -> Option<RawFrame> {
    loop {
        if let Some(frame) = stream.try_next() {
            return Some(frame);
        }
        if stream.is_finished() {
            return None;
        }
        thread::sleep(Duration::from_micros(200));
    }
}

/// Times of the first frame of every flash that `VideoStream` delivers
/// between `from` and `from + window`, as stamped by the player pipeline.
fn video_flashes(
    path: &Path,
    from: f64,
    window: f64,
    rate: f64,
    timing: Timing,
    hardware: bool,
) -> Vec<f64> {
    let size = FrameSize::new(160, 90);
    let mut stream = VideoStream::spawn(path, secs(from), size, rate, timing, hardware).unwrap();
    let mut flashes = Vec::new();
    let mut lit = false;
    while let Some(frame) = wait_for_frame(&mut stream) {
        let pts = frame.pts.as_secs_f64();
        if pts > from + window {
            break;
        }
        let on = luma(&frame) > 200.0;
        if on && !lit {
            flashes.push(pts);
        }
        lit = on;
    }
    flashes
}

/// Finds the first sample of each beep in a mono stream.
struct Onsets {
    threshold: f32,
    quiet: u64,
    index: u64,
    last_loud: Option<u64>,
    found: Vec<u64>,
}

impl Onsets {
    fn new(threshold: f32, quiet: f64) -> Self {
        Self {
            threshold,
            quiet: (quiet * f64::from(SAMPLE_RATE)) as u64,
            index: 0,
            last_loud: None,
            found: Vec::new(),
        }
    }

    fn push(&mut self, sample: f32) {
        if sample.abs() > self.threshold {
            if self
                .last_loud
                .is_none_or(|last| self.index - last > self.quiet)
            {
                self.found.push(self.index);
            }
            self.last_loud = Some(self.index);
        }
        self.index += 1;
    }
}

/// Times of every beep in the first `window` seconds of what the player's
/// audio decoder produces when asked to start at `from`.
fn audio_beeps(path: &Path, from: f64, window: f64, timing: Timing) -> Vec<f64> {
    let mut child = feeder_command(path, secs(from), 1.0, timing, SAMPLE_RATE, 2)
        .spawn()
        .unwrap();
    let mut stdout = child.stdout.take().unwrap();
    let mut onsets = Onsets::new(0.12, 0.5);
    let wanted = (window * f64::from(SAMPLE_RATE)) as u64;
    let mut buffer = vec![0u8; 64 * 1024];
    let mut carry = 0;
    'read: while onsets.index < wanted {
        let read = match stdout.read(&mut buffer[carry..]) {
            Ok(0) | Err(_) => break,
            Ok(read) => read,
        };
        let filled = carry + read;
        let whole = filled - filled % 8;
        for frame in buffer[..whole].as_chunks::<8>().0 {
            onsets.push(f32::from_le_bytes([frame[0], frame[1], frame[2], frame[3]]));
            if onsets.index >= wanted {
                break 'read;
            }
        }
        buffer.copy_within(whole..filled, 0);
        carry = filled - whole;
    }
    let _ = child.kill();
    let _ = child.wait();
    onsets
        .found
        .iter()
        .map(|&i| from + i as f64 / f64::from(SAMPLE_RATE))
        .collect()
}

/// Largest distance from a mark among `times`; NaN without any.
fn worst_error(marks: Marks, times: &[f64]) -> f64 {
    times
        .iter()
        .map(|&t| marks.error(t).abs())
        .fold(f64::NAN, f64::max)
}

// ---------------------------------------------------------------------------
// Speed benchmarks

fn time<T>(work: impl FnOnce() -> T) -> (f64, T) {
    let started = Instant::now();
    let value = work();
    (ms(started.elapsed()), value)
}

fn video_fixtures() -> Vec<(&'static str, PathBuf)> {
    let mut fixtures = vec![("h264_1080p60", h264_1080p60().unwrap())];
    if let Some(path) = hevc_1440p60() {
        fixtures.push(("hevc_1440p60", path));
    }
    if let Some(path) = hevc_2160p60() {
        fixtures.push(("hevc_2160p60", path));
    }
    fixtures
}

#[test]
#[ignore]
fn bench_first_frame() {
    let mut report = Report::new("first_frame");
    for (name, path) in video_fixtures() {
        // The first run only warms the file cache.
        ffmpeg::extract_frame(&path, Duration::ZERO, VIEW).unwrap();
        let runs = (0..9)
            .map(|_| time(|| ffmpeg::extract_frame(&path, Duration::ZERO, VIEW).unwrap()).0)
            .collect();
        report.add(format!("first_frame {name}"), "ms", runs);
    }
    report.finish();
}

/// Twenty positions spread over a clip, the same on every run.
fn random_positions(duration: f64) -> Vec<f64> {
    let mut random = Lcg(77);
    (0..20)
        .map(|_| (random.next() % ((duration - 1.0) * 1000.0) as u64) as f64 / 1000.0)
        .collect()
}

#[test]
#[ignore]
fn bench_seek_still() {
    let mut report = Report::new("seek_still");
    for (name, path) in video_fixtures() {
        let duration = info_of(&path).duration.unwrap().as_secs_f64();
        ffmpeg::extract_frame(&path, Duration::ZERO, VIEW).unwrap();
        let runs = random_positions(duration)
            .into_iter()
            .map(|at| time(|| ffmpeg::extract_frame(&path, secs(at), VIEW).unwrap()).0)
            .collect();
        report.add(format!("seek_still {name}"), "ms", runs);
    }
    report.finish();
}

/// What the player decodes for each step of a timeline drag.
fn scrub_step(path: &Path, at: Duration, size: FrameSize) -> RawFrame {
    ffmpeg::extract_keyframe(path, at, size).unwrap()
}

#[test]
#[ignore]
fn bench_scrub_step() {
    let mut report = Report::new("scrub_step");
    for (name, path) in video_fixtures() {
        let duration = info_of(&path).duration.unwrap().as_secs_f64();
        scrub_step(&path, Duration::ZERO, VIEW);
        let runs = random_positions(duration)
            .into_iter()
            .map(|at| time(|| scrub_step(&path, secs(at), VIEW)).0)
            .collect();
        report.add(format!("scrub_step {name}"), "ms", runs);
    }
    report.finish();
}

#[test]
#[ignore]
fn bench_playback_start() {
    let mut report = Report::new("playback_start");
    for (name, path) in video_fixtures() {
        let info = info_of(&path);
        let (rate, timing) = (crate::playback::decode_rate(&info), info.timing());
        drop(VideoStream::spawn(&path, Duration::ZERO, VIEW, rate, timing, false).unwrap());
        // The start of the clip, and a point well inside a group of pictures.
        for at in [0.0, 12.3] {
            for hwaccel in [true, false] {
                let runs = (0..7)
                    .map(|_| {
                        time(|| {
                            let mut stream =
                                VideoStream::spawn(&path, secs(at), VIEW, rate, timing, hwaccel)
                                    .unwrap();
                            wait_for_frame(&mut stream).unwrap()
                        })
                        .0
                    })
                    .collect();
                let mode = if hwaccel { "hwaccel" } else { "software" };
                report.add(
                    format!("video_first_frame {name} at {at}s {mode}"),
                    "ms",
                    runs,
                );
            }
            // Time until the audio decoder has 50 ms of sound, the point at
            // which playback can start.
            let runs = (0..7)
                .map(|_| {
                    time(|| {
                        let mut child =
                            feeder_command(&path, secs(at), 1.0, timing, SAMPLE_RATE, 2)
                                .spawn()
                                .unwrap();
                        let mut stdout = child.stdout.take().unwrap();
                        let mut buffer = vec![0u8; (SAMPLE_RATE as usize / 20) * 8];
                        stdout.read_exact(&mut buffer).unwrap();
                        let _ = child.kill();
                        let _ = child.wait();
                    })
                    .0
                })
                .collect();
            report.add(format!("audio_ready {name} at {at}s"), "ms", runs);
        }
    }
    report.finish();
}

#[test]
#[ignore]
fn bench_decode_throughput() {
    let mut report = Report::new("decode_throughput");
    for (name, path) in video_fixtures() {
        let info = info_of(&path);
        let (rate, timing) = (crate::playback::decode_rate(&info), info.timing());
        for hwaccel in [true, false] {
            let runs = (0..3)
                .map(|_| {
                    let mut stream =
                        VideoStream::spawn(&path, secs(10.0), VIEW, rate, timing, hwaccel).unwrap();
                    let first = wait_for_frame(&mut stream).unwrap();
                    let started = Instant::now();
                    let mut frames = 0u32;
                    while let Some(frame) = wait_for_frame(&mut stream) {
                        frames += 1;
                        if frame.pts.as_secs_f64() >= 20.0 {
                            break;
                        }
                    }
                    drop(first);
                    f64::from(frames) / started.elapsed().as_secs_f64()
                })
                .collect();
            let mode = if hwaccel { "hwaccel" } else { "software" };
            report.add(format!("throughput {name} {mode}"), "fps", runs);
        }
    }
    report.finish();
}

// ---------------------------------------------------------------------------
// Playback sync

fn sync_case(report: &mut Report, label: &str, fixture: &Fixture, from: f64, window: f64) {
    let info = info_of(&fixture.path);
    let rate = crate::playback::decode_rate(&info);
    let timing = info.timing();
    let flashes = video_flashes(
        &fixture.path,
        from,
        window,
        rate,
        timing,
        info.prefers_hardware_decode(),
    );
    let beeps = audio_beeps(&fixture.path, from, window, timing);

    let video_error = worst_error(fixture.marks, &flashes);
    let audio_error = worst_error(fixture.marks, &beeps);
    // The offset between the picture and sound of the same mark.
    let offset = flashes
        .iter()
        .filter_map(|&flash| {
            let index = fixture.marks.index(flash);
            beeps
                .iter()
                .find(|&&beep| fixture.marks.index(beep) == index)
                .map(|&beep| (beep - flash).abs())
        })
        .fold(f64::NAN, f64::max);
    let name = format!("{label} from {from}s");
    report.add(
        format!("{name} video max|err|"),
        "ms",
        vec![video_error * 1000.0],
    );
    report.add(
        format!("{name} audio max|err|"),
        "ms",
        vec![audio_error * 1000.0],
    );
    report.add(
        format!("{name} A/V max|offset|"),
        "ms",
        vec![offset * 1000.0],
    );
}

#[test]
#[ignore]
fn bench_playback_sync() {
    let mut report = Report::new("playback_sync");
    for spec in [
        SYNC_MARKS,
        SYNC_MARKS_2997,
        SYNC_AUDIO_LATE,
        SYNC_VIDEO_LATE,
    ] {
        let fixture = sync_fixture(&spec);
        for from in [0.0, 2.5, 7.3, 61.1] {
            sync_case(
                &mut report,
                spec.name.trim_end_matches(".mp4"),
                &fixture,
                from,
                spec.period * 2.2,
            );
        }
    }
    let long = sync_fixture(&LONG_10MIN);
    // The whole clip from the start, then late seeks.
    sync_case(&mut report, "long_10min", &long, 0.0, 600.0);
    for from in [301.0, 541.1] {
        sync_case(&mut report, "long_10min", &long, from, 66.0);
    }
    report.finish();
}

// ---------------------------------------------------------------------------
// Export sync

struct Case {
    name: &'static str,
    ranges: &'static [(f64, f64)],
    mode: ExportMode,
    tracks: &'static [usize],
    gain_db: f32,
    normalize: bool,
    max_fps: Option<u32>,
}

const REENCODE: ExportMode = ExportMode::Reencode;
const COPY: ExportMode = ExportMode::StreamCopy;
const JOIN: &[(f64, f64)] = &[(2.0, 7.0), (30.0, 36.0), (61.0, 66.0)];
/// Eight pieces, off the frame grid, to show whether errors add up per join.
const SPLIT: &[(f64, f64)] = &[
    (2.0, 6.5),
    (10.3, 14.8),
    (20.1, 24.6),
    (30.7, 35.2),
    (45.2, 49.7),
    (60.9, 65.4),
    (80.4, 84.9),
    (100.6, 105.1),
];

const CASES: &[Case] = &[
    Case {
        name: "reencode single",
        ranges: &[(3.0, 18.0)],
        mode: REENCODE,
        tracks: &[0],
        gain_db: 0.0,
        normalize: false,
        max_fps: None,
    },
    Case {
        name: "reencode join x3",
        ranges: JOIN,
        mode: REENCODE,
        tracks: &[0],
        gain_db: 0.0,
        normalize: false,
        max_fps: None,
    },
    Case {
        name: "reencode join x2 max 30fps",
        ranges: &[(2.0, 9.0), (40.0, 47.0)],
        mode: REENCODE,
        tracks: &[0],
        gain_db: 0.0,
        normalize: false,
        max_fps: Some(30),
    },
    Case {
        name: "reencode 2 tracks mix +3dB",
        ranges: &[(3.0, 18.0)],
        mode: REENCODE,
        tracks: &[0, 1],
        gain_db: 3.0,
        normalize: false,
        max_fps: None,
    },
    Case {
        name: "reencode normalize",
        ranges: &[(3.0, 23.0)],
        mode: REENCODE,
        tracks: &[0],
        gain_db: 0.0,
        normalize: true,
        max_fps: None,
    },
    Case {
        name: "reencode join x8",
        ranges: SPLIT,
        mode: REENCODE,
        tracks: &[0],
        gain_db: 0.0,
        normalize: false,
        max_fps: None,
    },
    Case {
        name: "copy single",
        ranges: &[(3.0, 18.0)],
        mode: COPY,
        tracks: &[0],
        gain_db: 0.0,
        normalize: false,
        max_fps: None,
    },
    Case {
        name: "copy join x3",
        ranges: JOIN,
        mode: COPY,
        tracks: &[0],
        gain_db: 0.0,
        normalize: false,
        max_fps: None,
    },
    Case {
        name: "copy join x8",
        ranges: SPLIT,
        mode: COPY,
        tracks: &[0],
        gain_db: 0.0,
        normalize: false,
        max_fps: None,
    },
];

fn request(source: &Path, output: &Path, info: &MediaInfo, case: &Case) -> ExportRequest {
    ExportRequest {
        source: source.to_path_buf(),
        output: output.to_path_buf(),
        ranges: case
            .ranges
            .iter()
            .map(|&(start, end)| TrimRange::new(secs(start), Some(secs(end))))
            .collect(),
        mode: case.mode,
        video: VideoEncoding::default(),
        source_duration: info.duration,
        audio_tracks: case.tracks.to_vec(),
        audio_adjust: AudioAdjust {
            gain_db: case.gain_db,
            normalize: case.normalize,
        },
        video_size: info.video_size,
        transform: Transform::default(),
        limits: OutputLimits {
            max_lines: None,
            max_fps: case.max_fps,
        },
        frame_rate: info.frame_rate,
        format: Format::Mp4,
        animation: Animation::default(),
    }
}

/// How an exported file's picture and sound line up, as a timestamp-honouring
/// player would play them.
struct Alignment {
    /// Largest picture-to-sound distance over the marks, in ms.
    offset: f64,
    /// Flashes with no beep near them.
    unpaired: usize,
    /// Audio start minus video start, in ms.
    start: f64,
    /// Audio end minus video end, in ms.
    end: f64,
}

fn align(path: &Path) -> Alignment {
    // Video: the time and brightness of every frame, as the container has them.
    let mut command = ffmpeg_command();
    let output = command
        .arg("-copyts")
        .arg("-i")
        .arg(path)
        .args(["-an", "-fps_mode", "passthrough", "-vf"])
        .arg("scale=16:9,signalstats,metadata=mode=print:file=-")
        .args(["-f", "null", "-"])
        .no_window()
        .stdin(Stdio::null())
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&output.stdout);
    let mut frames: Vec<(f64, f64)> = Vec::new();
    for line in text.lines() {
        if let Some(pts) = line.split("pts_time:").nth(1) {
            frames.push((pts.trim().parse().unwrap(), 0.0));
        } else if let Some(y) = line.strip_prefix("lavfi.signalstats.YAVG=") {
            frames.last_mut().unwrap().1 = y.trim().parse().unwrap();
        }
    }
    assert!(!frames.is_empty(), "no video frames in {}", path.display());
    let mut flashes = Vec::new();
    let mut lit = false;
    for &(pts, y) in &frames {
        let on = y > 128.0;
        if on && !lit {
            flashes.push(pts);
        }
        lit = on;
    }
    let interval = if frames.len() > 1 {
        (frames[frames.len() - 1].0 - frames[0].0) / (frames.len() - 1) as f64
    } else {
        0.0
    };
    let video_start = frames[0].0;
    let video_end = frames[frames.len() - 1].0 + interval;

    // Audio: where the container says it starts, then the samples; gaps
    // larger than 5 ms are filled, as timestamps dictate.
    let probe = ffmpeg::command(Tool::Ffprobe)
        .args(["-v", "error", "-select_streams", "a:0"])
        .args(["-show_entries", "stream=start_time", "-of", "csv=p=0"])
        .arg(path)
        .no_window()
        .output()
        .unwrap();
    let audio_start: f64 = String::from_utf8_lossy(&probe.stdout)
        .trim()
        .parse()
        .unwrap_or(0.0);
    let mut command = ffmpeg_command();
    let output = command
        .arg("-copyts")
        .arg("-i")
        .arg(path)
        .args([
            "-vn",
            "-map",
            "0:a:0",
            "-af",
            "aresample=async=1:min_hard_comp=0.005",
        ])
        .args(["-ac", "1", "-ar"])
        .arg(SAMPLE_RATE.to_string())
        .args(["-f", "f32le", "-"])
        .no_window()
        .stdin(Stdio::null())
        .output()
        .unwrap();
    let samples: Vec<f32> = output
        .stdout
        .as_chunks::<4>()
        .0
        .iter()
        .map(|b| f32::from_le_bytes(*b))
        .collect();
    let peak = samples.iter().fold(0.0f32, |m, s| m.max(s.abs()));
    let mut onsets = Onsets::new((peak * 0.25).max(0.01), 0.1);
    samples.iter().for_each(|&s| onsets.push(s));
    let beeps: Vec<f64> = onsets
        .found
        .iter()
        .map(|&i| audio_start + i as f64 / f64::from(SAMPLE_RATE))
        .collect();
    let audio_end = audio_start + samples.len() as f64 / f64::from(SAMPLE_RATE);

    let mut offset = f64::NAN;
    let mut unpaired = 0;
    for &flash in &flashes {
        let nearest = beeps
            .iter()
            .copied()
            .min_by(|a, b| (a - flash).abs().total_cmp(&(b - flash).abs()));
        match nearest.filter(|beep| (beep - flash).abs() < 1.0) {
            Some(beep) => offset = offset.max((beep - flash).abs()),
            None => unpaired += 1,
        }
    }
    Alignment {
        offset: offset * 1000.0,
        unpaired,
        start: (audio_start - video_start) * 1000.0,
        end: (audio_end - video_end) * 1000.0,
    }
}

/// Exports that re-encode, and a lossless cut of one range, keep every
/// flash within a frame (plus 5 ms) of its beep. Lossless joins are left
/// out: they can only start on keyframes, and with B-frames the picture of
/// every piece after the first lags by the reorder delay.
#[test]
#[ignore]
fn export_keeps_picture_and_sound_together() {
    let dir = bench_dir().join("exports");
    fs::create_dir_all(&dir).unwrap();
    for spec in [SYNC_MARKS_2997, SYNC_AUDIO_LATE, SYNC_VIDEO_LATE] {
        let fixture = sync_fixture(&spec);
        let info = info_of(&fixture.path);
        let rate = info.frame_rate.unwrap_or(30.0);
        for case in CASES
            .iter()
            .filter(|case| case.mode == REENCODE || case.name == "copy single")
        {
            let output = dir.join("checked.mp4");
            let _ = fs::remove_file(&output);
            let request = request(&fixture.path, &output, &info, case);
            ffmpeg::export(&request, &CancelToken::default(), |_| {}).unwrap();
            let alignment = align(&output);
            let _ = fs::remove_file(&output);
            let output_rate = case.max_fps.map_or(rate, |cap| rate.min(f64::from(cap)));
            let limit = 1000.0 / output_rate + 5.0;
            assert!(
                alignment.offset <= limit,
                "{} {}: sound is {:.1} ms from the picture, over {limit:.1} ms",
                spec.name,
                case.name,
                alignment.offset,
            );
        }
    }
}

#[test]
#[ignore]
fn bench_export_sync() {
    let mut report = Report::new("export_sync");
    let dir = bench_dir().join("exports");
    fs::create_dir_all(&dir).unwrap();
    for spec in [
        SYNC_MARKS,
        SYNC_MARKS_2997,
        SYNC_AUDIO_LATE,
        SYNC_VIDEO_LATE,
    ] {
        let fixture = sync_fixture(&spec);
        let info = info_of(&fixture.path);
        let label = spec.name.trim_end_matches(".mp4");
        for case in CASES {
            let output = dir.join("out.mp4");
            let _ = fs::remove_file(&output);
            let request = request(&fixture.path, &output, &info, case);
            let (wall, result) = time(|| ffmpeg::export(&request, &CancelToken::default(), |_| {}));
            let name = format!("{label} {}", case.name);
            if let Err(error) = result {
                println!("  {name}: FAILED {error}");
                for metric in [
                    "wall",
                    "A/V max|offset|",
                    "unpaired",
                    "start A-V",
                    "end A-V",
                ] {
                    report.add(format!("{name} {metric}"), "ms", vec![f64::NAN]);
                }
                continue;
            }
            let alignment = align(&output);
            report.add(format!("{name} wall"), "ms", vec![wall]);
            report.add(
                format!("{name} A/V max|offset|"),
                "ms",
                vec![alignment.offset],
            );
            report.add(
                format!("{name} unpaired"),
                "count",
                vec![alignment.unpaired as f64],
            );
            report.add(format!("{name} start A-V"), "ms", vec![alignment.start]);
            report.add(format!("{name} end A-V"), "ms", vec![alignment.end]);
            let _ = fs::remove_file(&output);
        }
    }
    report.finish();
}

// ---------------------------------------------------------------------------
// Browsing a folder

/// What opening a clip does besides decoding its first frame, as the player
/// does it: a filmstrip of twelve keyframes and the waveform.
fn background_extras(
    path: &Path,
    duration: Duration,
    pool: &WorkerPool,
    live: Option<(Arc<AtomicU64>, u64)>,
) {
    for index in 0..12 {
        let (path, live) = (path.to_path_buf(), live.clone());
        let at = duration.mul_f64((index as f64 + 0.5) / 12.0);
        drop(pool.run(move || {
            if let Some((live, generation)) = live
                && live.load(Ordering::Relaxed) != generation
            {
                return;
            }
            let _ = ffmpeg::extract_keyframe(&path, at, FrameSize::new(192, 108));
        }));
    }
    let path = path.to_path_buf();
    thread::spawn(move || {
        let _ = ffmpeg::audio_peaks(&path, duration, 1);
    });
}

/// The player before this change: every opened clip starts its own decode at
/// once, with the filmstrip and waveform queued behind it. Returns, for each
/// clip, the time from opening it to its first frame.
fn browse_eager(clips: &[PathBuf], interval: Duration, duration: Duration) -> Vec<f64> {
    let filmstrip = WorkerPool::new("filmstrip", 2);
    let (sender, receiver) = std::sync::mpsc::channel();
    let mut opened = Vec::new();
    for (index, clip) in clips.iter().enumerate() {
        let (decoded, sender) = (clip.clone(), sender.clone());
        opened.push(Instant::now());
        thread::spawn(move || {
            ffmpeg::extract_frame(&decoded, Duration::ZERO, VIEW).unwrap();
            sender.send((index, Instant::now())).unwrap();
        });
        background_extras(clip, duration, &filmstrip, None);
        thread::sleep(interval);
    }
    let mut latencies = vec![0.0; clips.len()];
    for _ in clips {
        let (index, done) = receiver.recv().unwrap();
        latencies[index] = ms(done - opened[index]);
    }
    latencies
}

/// The player now: stale decodes are skipped, the filmstrip and waveform
/// wait for the clip to stay open, and the neighbours' first frames are
/// prefetched into the cache. Clips that were replaced before their frame
/// arrived have no latency (NaN), as nobody saw them.
fn browse_latest(clips: &[PathBuf], interval: Duration, duration: Duration) -> Vec<f64> {
    use crate::frame_cache::{FrameCache, FrameKey};
    let live = Arc::new(AtomicU64::new(0));
    let epoch = Arc::new(AtomicU64::new(0));
    let stills = WorkerPool::new("still", 2);
    let prefetch = WorkerPool::new("prefetch", 1);
    let filmstrip = WorkerPool::new("filmstrip", 2);
    let cache = Arc::new(Mutex::new(FrameCache::new(64 << 20)));
    let mut latencies = Vec::new();
    for (index, clip) in clips.iter().enumerate() {
        let generation = index as u64 + 1;
        live.store(generation, Ordering::Relaxed);
        epoch.fetch_add(1, Ordering::Relaxed);
        let opened = Instant::now();
        let key = FrameKey::new(clip, Duration::ZERO, VIEW).unwrap();
        let cached = cache.lock().unwrap().get(&key).is_some();
        let (sender, receiver) = std::sync::mpsc::channel();
        if cached {
            sender.send(Instant::now()).unwrap();
        } else {
            let (clip, live, cache, key) = (clip.clone(), live.clone(), cache.clone(), key);
            drop(stills.run(move || {
                if live.load(Ordering::Relaxed) != generation {
                    return;
                }
                let frame = ffmpeg::extract_frame(&clip, Duration::ZERO, VIEW).unwrap();
                cache.lock().unwrap().insert(key, frame);
                let _ = sender.send(Instant::now());
            }));
        }
        // The next clip is opened after the interval; the frame counts if it
        // arrived by then, or, for the last clip, whenever it does.
        let shown = match receiver.recv_timeout(interval) {
            Ok(shown) => Some(shown),
            Err(_) if index + 1 == clips.len() => Some(receiver.recv().unwrap()),
            Err(_) => None,
        };
        // Once its frame is up, the neighbours' are prefetched.
        if shown.is_some() {
            for neighbour in [index + 1, index.wrapping_sub(1)] {
                let Some(next) = clips.get(neighbour).cloned() else {
                    continue;
                };
                let (epoch, cache) = (epoch.clone(), cache.clone());
                let queued_at = epoch.load(Ordering::Relaxed);
                drop(prefetch.run(move || {
                    if epoch.load(Ordering::Relaxed) != queued_at {
                        return;
                    }
                    let key = FrameKey::new(&next, Duration::ZERO, VIEW).unwrap();
                    if cache.lock().unwrap().contains(&key) {
                        return;
                    }
                    let frame = ffmpeg::extract_frame(&next, Duration::ZERO, VIEW).unwrap();
                    cache.lock().unwrap().insert(key, frame);
                }));
            }
        }
        latencies.push(shown.map_or(f64::NAN, |shown| ms(shown - opened)));
        thread::sleep(interval.saturating_sub(opened.elapsed()));
        // The extras only start if the clip stays open for a moment.
        if interval > Duration::from_millis(250) {
            background_extras(clip, duration, &filmstrip, Some((live.clone(), generation)));
        }
    }
    latencies
}

#[test]
#[ignore]
fn bench_browse() {
    let source = h264_1080p60().unwrap();
    let duration = Duration::from_secs(60);
    let dir = bench_dir().join("fixtures").join("browse");
    fs::create_dir_all(&dir).unwrap();
    let clips: Vec<PathBuf> = (0..8)
        .map(|index| {
            let path = dir.join(format!("clip{index}.mp4"));
            if !path.exists() {
                fs::hard_link(&source, &path)
                    .or_else(|_| fs::copy(&source, &path).map(|_| ()))
                    .unwrap();
            }
            path
        })
        .collect();
    let mut report = Report::new("browse");
    type Browse = fn(&[PathBuf], Duration, Duration) -> Vec<f64>;
    for (label, interval) in [("hold arrow key", 60), ("flip through", 600)] {
        let interval = Duration::from_millis(interval);
        for (mode, run) in [("before", browse_eager as Browse), ("after", browse_latest)] {
            let mut last = Vec::new();
            let mut each = Vec::new();
            for _ in 0..3 {
                let latencies = run(&clips, interval, duration);
                last.push(*latencies.last().unwrap());
                each.extend(latencies.into_iter().filter(|l| !l.is_nan()));
                // Let the leftovers of this run finish before the next.
                thread::sleep(Duration::from_secs(5));
            }
            report.add(
                format!("browse {label}: last clip first frame ({mode})"),
                "ms",
                last,
            );
            report.add(
                format!("browse {label}: every clip first frame ({mode})"),
                "ms",
                each,
            );
        }
    }
    report.finish();
}

#[test]
#[ignore]
fn bench_inspect() {
    let path = h264_1080p60().unwrap();
    let mut report = Report::new("inspect");
    let probe = (0..9).map(|_| time(|| info_of(&path)).0).collect();
    report.add("inspect probe", "ms", probe);
    let size = info_of(&path)
        .video_size
        .unwrap()
        .fit_within(FrameSize::new(160, 90));
    let thumbnail = (0..9)
        .map(|_| time(|| ffmpeg::extract_keyframe(&path, Duration::from_secs(1), size).unwrap()).0)
        .collect();
    report.add("inspect thumbnail", "ms", thumbnail);
    report.finish();
}
