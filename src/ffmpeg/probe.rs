//! Reading clip metadata with `ffprobe`.

use std::{
    path::Path,
    process::{Command, Stdio},
    time::Duration,
};

use anyhow::{Context, Result, bail};
use serde::Deserialize;

use super::{CommandExt as _, FrameSize};

/// What the player and editor need to know about a clip.
#[derive(Debug, Clone, PartialEq)]
pub struct MediaInfo {
    pub duration: Option<Duration>,
    /// Display size, with rotation metadata already applied.
    pub video_size: Option<FrameSize>,
    pub frame_rate: Option<f64>,
    pub has_audio: bool,
    pub video_codec: Option<String>,
}

impl MediaInfo {
    /// The frame interval, defaulting to 30 fps for streams that do not say.
    pub fn frame_interval(&self) -> Duration {
        Duration::from_secs_f64(1.0 / self.frame_rate.unwrap_or(30.0))
    }
}

pub fn probe(path: &Path) -> Result<MediaInfo> {
    let output = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-print_format",
            "json",
            "-show_format",
            "-show_streams",
        ])
        .arg(path)
        .no_window()
        .stdin(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .context("could not start ffprobe; check that ffmpeg is installed and on PATH")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("ffprobe could not read the file: {}", stderr.trim());
    }
    parse(&output.stdout)
}

/// The time of the last video keyframe at or before `at`: where a stream
/// copy that is asked to start at `at` really starts.
pub fn keyframe_at_or_before(path: &Path, at: Duration) -> Result<Duration> {
    if at.is_zero() {
        return Ok(Duration::ZERO);
    }
    // Seeking lands on the keyframe before the target, so the first packet
    // read after it is the one we want.
    let output = Command::new("ffprobe")
        .args(["-v", "error", "-select_streams", "v:0", "-read_intervals"])
        .arg(format!("{}%+#1", super::seconds_arg(at)))
        .args([
            "-show_entries",
            "packet=pts_time,flags:stream=start_time",
            "-print_format",
            "json",
        ])
        .arg(path)
        .no_window()
        .stdin(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .context("could not start ffprobe; check that ffmpeg is installed and on PATH")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("ffprobe could not read the file: {}", stderr.trim());
    }
    parse_keyframe(&output.stdout, at)
}

#[derive(Deserialize)]
struct KeyframeOutput {
    #[serde(default)]
    packets: Vec<Packet>,
    #[serde(default)]
    streams: Vec<StreamStart>,
}

#[derive(Deserialize)]
struct Packet {
    pts_time: Option<String>,
    flags: Option<String>,
}

#[derive(Deserialize)]
struct StreamStart {
    start_time: Option<String>,
}

fn parse_keyframe(json: &[u8], at: Duration) -> Result<Duration> {
    let output: KeyframeOutput =
        serde_json::from_slice(json).context("unexpected ffprobe output")?;
    let packet = output
        .packets
        .iter()
        .find(|packet| {
            packet
                .flags
                .as_deref()
                .is_some_and(|flags| flags.starts_with('K'))
        })
        .context("no keyframe found before the start")?;
    let pts: f64 = packet
        .pts_time
        .as_deref()
        .and_then(|text| text.parse().ok())
        .context("the keyframe has no timestamp")?;
    // Packet times are absolute; the clip's timeline starts at the stream's
    // start time.
    let start: f64 = output
        .streams
        .first()
        .and_then(|stream| stream.start_time.as_deref())
        .and_then(|text| text.parse().ok())
        .unwrap_or(0.0);
    let keyframe = Duration::from_secs_f64((pts - start).max(0.0));
    // Never report a later point than asked for, whatever rounding did.
    Ok(keyframe.min(at))
}

#[derive(Deserialize)]
struct ProbeOutput {
    #[serde(default)]
    streams: Vec<Stream>,
    format: Option<Format>,
}

#[derive(Deserialize)]
struct Format {
    duration: Option<String>,
}

#[derive(Deserialize)]
struct Stream {
    codec_type: Option<String>,
    codec_name: Option<String>,
    width: Option<u32>,
    height: Option<u32>,
    avg_frame_rate: Option<String>,
    r_frame_rate: Option<String>,
    duration: Option<String>,
    #[serde(default)]
    side_data_list: Vec<SideData>,
    #[serde(default)]
    tags: Tags,
    #[serde(default)]
    disposition: Disposition,
}

#[derive(Deserialize, Default)]
struct SideData {
    rotation: Option<f64>,
}

#[derive(Deserialize, Default)]
struct Tags {
    rotate: Option<String>,
}

#[derive(Deserialize, Default)]
struct Disposition {
    #[serde(default)]
    attached_pic: u8,
}

fn parse(json: &[u8]) -> Result<MediaInfo> {
    let probe: ProbeOutput = serde_json::from_slice(json).context("unexpected ffprobe output")?;

    // Cover art is reported as a video stream; skip it.
    let video = probe.streams.iter().find(|stream| {
        stream.codec_type.as_deref() == Some("video") && stream.disposition.attached_pic == 0
    });
    let has_audio = probe
        .streams
        .iter()
        .any(|stream| stream.codec_type.as_deref() == Some("audio"));

    let duration = probe
        .format
        .as_ref()
        .and_then(|format| format.duration.as_deref())
        .or_else(|| video.and_then(|stream| stream.duration.as_deref()))
        .and_then(parse_seconds);

    let video_size = video.and_then(|stream| {
        let (width, height) = (stream.width?, stream.height?);
        let quarter_turn =
            rotation(stream).is_some_and(|degrees| (degrees.abs() / 90.0).round() as i64 % 2 == 1);
        Some(if quarter_turn {
            FrameSize::new(height, width)
        } else {
            FrameSize::new(width, height)
        })
    });

    let frame_rate = video.and_then(|stream| {
        [&stream.avg_frame_rate, &stream.r_frame_rate]
            .into_iter()
            .flatten()
            .find_map(|rate| parse_rate(rate))
    });

    Ok(MediaInfo {
        duration,
        video_size,
        frame_rate,
        has_audio,
        video_codec: video.and_then(|stream| stream.codec_name.clone()),
    })
}

fn rotation(stream: &Stream) -> Option<f64> {
    stream
        .side_data_list
        .iter()
        .find_map(|data| data.rotation)
        .or_else(|| stream.tags.rotate.as_deref()?.parse().ok())
}

fn parse_seconds(text: &str) -> Option<Duration> {
    let seconds: f64 = text.parse().ok()?;
    (seconds.is_finite() && seconds > 0.0).then(|| Duration::from_secs_f64(seconds))
}

/// Parses rates such as `60/1` or `30000/1001`; `0/0` means "unknown".
fn parse_rate(text: &str) -> Option<f64> {
    let (numerator, denominator) = text.split_once('/')?;
    let numerator: f64 = numerator.parse().ok()?;
    let denominator: f64 = denominator.parse().ok()?;
    let rate = numerator / denominator;
    (rate.is_finite() && rate > 0.0 && rate < 1000.0).then_some(rate)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_a_typical_recording() -> Result<()> {
        let json = br#"{
            "streams": [
                {"codec_type": "video", "codec_name": "h264", "width": 1920, "height": 1080,
                 "avg_frame_rate": "60/1", "r_frame_rate": "60/1"},
                {"codec_type": "audio", "codec_name": "aac"}
            ],
            "format": {"duration": "61.250000"}
        }"#;
        let info = parse(json)?;
        assert_eq!(info.duration, Some(Duration::from_millis(61_250)));
        assert_eq!(info.video_size, Some(FrameSize::new(1920, 1080)));
        assert_eq!(info.frame_rate, Some(60.0));
        assert!(info.has_audio);
        assert_eq!(info.video_codec.as_deref(), Some("h264"));
        Ok(())
    }

    #[test]
    fn finds_the_keyframe_relative_to_the_stream_start() -> Result<()> {
        let json = br#"{
            "packets": [{"pts_time": "2.500000", "flags": "K__"}],
            "streams": [{"start_time": "0.500000"}]
        }"#;
        let at = Duration::from_millis(3_300);
        assert_eq!(parse_keyframe(json, at)?, Duration::from_secs(2));

        let no_keyframe = br#"{"packets": [{"pts_time": "2.5", "flags": "___"}], "streams": []}"#;
        assert!(parse_keyframe(no_keyframe, at).is_err());
        Ok(())
    }

    #[test]
    fn applies_rotation_and_skips_cover_art() -> Result<()> {
        let json = br#"{
            "streams": [
                {"codec_type": "video", "width": 600, "height": 600, "disposition": {"attached_pic": 1}},
                {"codec_type": "video", "width": 1920, "height": 1080, "avg_frame_rate": "0/0",
                 "r_frame_rate": "30000/1001", "side_data_list": [{"rotation": -90}]}
            ],
            "format": {}
        }"#;
        let info = parse(json)?;
        assert_eq!(info.video_size, Some(FrameSize::new(1080, 1920)));
        assert!((info.frame_rate.unwrap() - 29.97).abs() < 0.01);
        assert!(!info.has_audio);
        assert_eq!(info.duration, None);
        Ok(())
    }
}
