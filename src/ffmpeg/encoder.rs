//! Picking a video encoder and its settings.
//!
//! Each codec has a chain of encoders: the hardware ones (AMF, NVENC, Quick
//! Sync) first, then a software encoder that always exists in a full ffmpeg
//! build. The first one that can encode a test clip on this machine is used
//! for every export in that codec.

use std::{
    process::{Command, Stdio},
    sync::OnceLock,
    thread,
};

use super::CommandExt as _;

/// The video codec of an export.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Codec {
    #[default]
    H264,
    Hevc,
    Av1,
}

impl Codec {
    pub const ALL: [Self; 3] = [Self::H264, Self::Hevc, Self::Av1];

    pub fn label(self) -> &'static str {
        match self {
            Self::H264 => "H.264",
            Self::Hevc => "HEVC",
            Self::Av1 => "AV1",
        }
    }

    pub fn description(self) -> &'static str {
        match self {
            Self::H264 => "Plays everywhere",
            Self::Hevc => "Smaller files, most devices",
            Self::Av1 => "Smallest files, newer devices",
        }
    }

    fn index(self) -> usize {
        match self {
            Self::H264 => 0,
            Self::Hevc => 1,
            Self::Av1 => 2,
        }
    }

    fn encoders(self) -> &'static [Encoder] {
        match self {
            Self::H264 => &H264_ENCODERS,
            Self::Hevc => &HEVC_ENCODERS,
            Self::Av1 => &AV1_ENCODERS,
        }
    }
}

static H264_ENCODERS: [Encoder; 4] = [
    Encoder::new("h264_amf", Family::Amf),
    Encoder::new("h264_nvenc", Family::Nvenc),
    Encoder::new("h264_qsv", Family::Qsv),
    Encoder::new("libx264", Family::X26x),
];

static HEVC_ENCODERS: [Encoder; 4] = [
    Encoder::new("hevc_amf", Family::Amf),
    Encoder::new("hevc_nvenc", Family::Nvenc),
    Encoder::new("hevc_qsv", Family::Qsv),
    Encoder::new("libx265", Family::X26x),
];

static AV1_ENCODERS: [Encoder; 5] = [
    Encoder::new("av1_amf", Family::Amf),
    Encoder::new("av1_nvenc", Family::Nvenc),
    Encoder::new("av1_qsv", Family::Qsv),
    Encoder::new("libsvtav1", Family::SvtAv1),
    Encoder::new("libaom-av1", Family::Aom),
];

/// How hard the encoder compresses, as named presets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Quality {
    Small,
    #[default]
    Balanced,
    High,
}

impl Quality {
    pub const ALL: [Self; 3] = [Self::Small, Self::Balanced, Self::High];

    pub fn label(self) -> &'static str {
        match self {
            Self::Small => "Small",
            Self::Balanced => "Balanced",
            Self::High => "High",
        }
    }

    /// The constant-quality level for `codec`: CRF, CQ or QP depending on
    /// the encoder, which share roughly the same scale within a codec.
    fn level(self, codec: Codec) -> u8 {
        match (codec, self) {
            (Codec::H264 | Codec::Hevc, Self::Small) => 26,
            (Codec::H264 | Codec::Hevc, Self::Balanced) => 20,
            (Codec::H264 | Codec::Hevc, Self::High) => 16,
            (Codec::Av1, Self::Small) => 38,
            (Codec::Av1, Self::Balanced) => 30,
            (Codec::Av1, Self::High) => 24,
        }
    }
}

/// How the video of an export is encoded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct VideoEncoding {
    pub codec: Codec,
    pub quality: Quality,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Family {
    Amf,
    Nvenc,
    Qsv,
    X26x,
    SvtAv1,
    Aom,
}

#[derive(Debug)]
struct Encoder {
    name: &'static str,
    family: Family,
}

impl Encoder {
    const fn new(name: &'static str, family: Family) -> Self {
        Self { name, family }
    }

    fn args(&self, codec: Codec, quality: Quality) -> Vec<String> {
        let level = quality.level(codec).to_string();
        let mut args: Vec<String> = vec!["-c:v".into(), self.name.into()];
        let extra: Vec<&str> = match self.family {
            Family::Amf => vec![
                "-quality", "balanced", "-rc", "cqp", "-qp_i", &level, "-qp_p", &level,
            ],
            Family::Nvenc => vec!["-preset", "p4", "-rc", "vbr", "-cq", &level, "-b:v", "0"],
            Family::Qsv => vec!["-preset", "fast", "-global_quality", &level],
            Family::X26x => vec!["-preset", "fast", "-crf", &level],
            Family::SvtAv1 => vec!["-preset", "8", "-crf", &level],
            Family::Aom => vec![
                "-cpu-used",
                "6",
                "-row-mt",
                "1",
                "-crf",
                &level,
                "-b:v",
                "0",
            ],
        };
        args.extend(extra.into_iter().map(String::from));
        // Apple players only take HEVC in MP4 with this tag.
        if codec == Codec::Hevc {
            args.extend(["-tag:v".into(), "hvc1".into()]);
        }
        args
    }

    /// Whether this encoder can encode a short test clip on this machine.
    fn works(&self, codec: Codec) -> bool {
        Command::new("ffmpeg")
            .args(["-hide_banner", "-loglevel", "error", "-f", "lavfi", "-i"])
            .arg("color=c=black:s=640x360:r=30:d=0.5,format=yuv420p")
            .args(self.args(codec, Quality::Balanced))
            .args(["-f", "null", "-"])
            .no_window()
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|status| status.success())
    }
}

static CHOSEN: [OnceLock<&'static Encoder>; 3] =
    [OnceLock::new(), OnceLock::new(), OnceLock::new()];

/// The first encoder for `codec` that works here, probing on first use.
fn encoder(codec: Codec) -> &'static Encoder {
    CHOSEN[codec.index()].get_or_init(|| {
        let encoders = codec.encoders();
        encoders
            .iter()
            .find(|encoder| encoder.works(codec))
            .unwrap_or(&encoders[encoders.len() - 1])
    })
}

/// The ffmpeg arguments that encode the video as `encoding` asks.
pub fn video_args(encoding: VideoEncoding) -> Vec<String> {
    encoder(encoding.codec).args(encoding.codec, encoding.quality)
}

/// Probes `codec`'s encoders on a background thread so the next export in
/// that codec does not pay for it.
pub fn warm_up(codec: Codec) {
    thread::spawn(move || encoder(codec));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quality_levels_map_onto_each_encoder() {
        let nvenc = &Codec::Hevc.encoders()[1];
        assert_eq!(
            nvenc.args(Codec::Hevc, Quality::High),
            [
                "-c:v",
                "hevc_nvenc",
                "-preset",
                "p4",
                "-rc",
                "vbr",
                "-cq",
                "16",
                "-b:v",
                "0",
                "-tag:v",
                "hvc1"
            ]
        );
        let svt = &Codec::Av1.encoders()[3];
        assert_eq!(
            svt.args(Codec::Av1, Quality::Small),
            ["-c:v", "libsvtav1", "-preset", "8", "-crf", "38"]
        );
        // The default keeps the quality plic always exported at.
        assert_eq!(Quality::default().level(Codec::H264), 20);
    }

    #[test]
    fn every_codec_ends_in_a_software_encoder() {
        for codec in Codec::ALL {
            let last = codec.encoders().last().unwrap();
            assert!(matches!(
                last.family,
                Family::X26x | Family::SvtAv1 | Family::Aom
            ));
        }
    }
}
