//! Audio peaks for drawing a clip's waveform along the timeline.

use std::{io::Read as _, path::Path, process::Stdio, time::Duration};

use anyhow::{Context, Result, bail};

use super::CommandExt as _;

/// Plenty for a timeline a few thousand pixels wide.
pub const PEAK_COUNT: usize = 1000;
/// Decoding at a low rate is fast and keeps every transient that shows.
const SAMPLE_RATE: u32 = 8000;
/// Quieter than this is drawn as silence rather than blown up to full height.
const NOISE_FLOOR: f32 = 0.02;

/// The loudest sample in each of `PEAK_COUNT` equal slices of the clip,
/// scaled so the loudest slice is 1. Every audio track is mixed in, so a
/// voice on a separate microphone track shows too.
pub fn peaks(path: &Path, duration: Duration, tracks: usize) -> Result<Vec<f32>> {
    let mut command = crate::ffmpeg::command(crate::ffmpeg::Tool::Ffmpeg);
    command
        .args(["-hide_banner", "-loglevel", "error", "-nostdin", "-i"])
        .arg(path);
    if tracks > 1 {
        let inputs: String = (0..tracks).map(|track| format!("[0:a:{track}]")).collect();
        command
            .arg("-filter_complex")
            .arg(format!("{inputs}amix=inputs={tracks}:normalize=0[a]"));
        command.args(["-map", "[a]"]);
    } else {
        command.args(["-map", "0:a:0"]);
    }
    let mut child = command
        .args(["-ac", "1", "-ar"])
        .arg(SAMPLE_RATE.to_string())
        .args(["-f", "s16le", "pipe:1"])
        .no_window()
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .context("could not start ffmpeg for the waveform")?;
    let mut stdout = child.stdout.take().context("ffmpeg has no stdout")?;

    let expected = (duration.as_secs_f64() * SAMPLE_RATE as f64).ceil() as u64;
    let mut peaks = Peaks::new(expected);
    let mut buffer = vec![0u8; 64 * 1024];
    // An odd byte left over from the previous read.
    let mut carry: Option<u8> = None;
    loop {
        let read = stdout.read(&mut buffer).context("could not read audio")?;
        if read == 0 {
            break;
        }
        let mut bytes = &buffer[..read];
        if let Some(low) = carry.take() {
            peaks.push(i16::from_le_bytes([low, bytes[0]]));
            bytes = &bytes[1..];
        }
        let (samples, rest) = bytes.as_chunks::<2>();
        for &sample in samples {
            peaks.push(i16::from_le_bytes(sample));
        }
        carry = rest.first().copied();
    }
    if !child.wait().context("ffmpeg did not finish")?.success() {
        bail!("could not decode the audio");
    }
    Ok(peaks.finish())
}

/// Folds a stream of samples into per-slice peaks.
struct Peaks {
    expected: u64,
    seen: u64,
    peaks: Vec<f32>,
}

impl Peaks {
    fn new(expected: u64) -> Self {
        Self {
            expected: expected.max(1),
            seen: 0,
            peaks: vec![0.0; PEAK_COUNT],
        }
    }

    fn push(&mut self, sample: i16) {
        let slice = (self.seen * PEAK_COUNT as u64 / self.expected) as usize;
        // Audio can run slightly past the probed duration.
        let slice = slice.min(PEAK_COUNT - 1);
        let level = (sample as f32 / i16::MAX as f32).abs().min(1.0);
        self.peaks[slice] = self.peaks[slice].max(level);
        self.seen += 1;
    }

    fn finish(mut self) -> Vec<f32> {
        let loudest = self.peaks.iter().copied().fold(0.0, f32::max);
        let scale = 1.0 / loudest.max(NOISE_FLOOR);
        for peak in &mut self.peaks {
            *peak = (*peak * scale).min(1.0);
        }
        self.peaks
    }
}

/// The loudest of `peaks` in each of `columns` equal slices, for drawing.
pub fn resample(peaks: &[f32], columns: usize) -> Vec<f32> {
    if peaks.is_empty() || columns == 0 {
        return Vec::new();
    }
    (0..columns)
        .map(|column| {
            let start = column * peaks.len() / columns;
            let end = ((column + 1) * peaks.len() / columns).max(start + 1);
            peaks[start..end.min(peaks.len())]
                .iter()
                .copied()
                .fold(0.0, f32::max)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spreads_samples_over_slices_and_normalizes() {
        let mut peaks = Peaks::new(PEAK_COUNT as u64 * 2);
        for index in 0..PEAK_COUNT * 2 {
            // Loud in the first half, quiet in the second.
            peaks.push(if index < PEAK_COUNT { 16_000 } else { -4_000 });
        }
        // A sample past the expected end lands in the last slice.
        peaks.push(0);
        let peaks = peaks.finish();
        assert_eq!(peaks.len(), PEAK_COUNT);
        assert!((peaks[0] - 1.0).abs() < 0.001);
        assert!((peaks[PEAK_COUNT - 1] - 0.25).abs() < 0.001);
    }

    #[test]
    fn silence_stays_flat() {
        let mut peaks = Peaks::new(100);
        for _ in 0..100 {
            peaks.push(10);
        }
        assert!(peaks.finish().iter().all(|&peak| peak < 0.05));
    }

    #[test]
    fn resamples_to_column_peaks() {
        let peaks = [0.1, 0.9, 0.2, 0.3];
        assert_eq!(resample(&peaks, 2), vec![0.9, 0.3]);
        assert_eq!(resample(&peaks, 8).len(), 8);
        assert!(resample(&[], 4).is_empty());
    }

    #[test]
    #[ignore = "needs ffmpeg"]
    fn reads_peaks_from_a_clip() {
        let dir = std::env::temp_dir().join("plic-waveform-test");
        std::fs::create_dir_all(&dir).unwrap();
        let clip = dir.join("tone.mp4");
        let status = crate::ffmpeg::command(crate::ffmpeg::Tool::Ffmpeg)
            .args(["-y", "-loglevel", "error", "-f", "lavfi", "-i"])
            .arg("sine=frequency=440:duration=2")
            .args(["-af", "volume='if(lt(t,1),1,0)':eval=frame", "-c:a", "aac"])
            .arg(&clip)
            .status()
            .unwrap();
        assert!(status.success());
        let peaks = peaks(&clip, Duration::from_secs(2), 1).unwrap();
        assert!(peaks[PEAK_COUNT / 4] > 0.5, "{}", peaks[PEAK_COUNT / 4]);
        assert!(
            peaks[PEAK_COUNT * 3 / 4] < 0.05,
            "{}",
            peaks[PEAK_COUNT * 3 / 4]
        );
    }
}
