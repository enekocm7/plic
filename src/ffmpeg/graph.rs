//! Building ffmpeg filter graphs for exports.
//!
//! A re-encoded export always runs through one `-filter_complex` graph that
//! ends in `[v]` and, with sound, `[a]`: one input per range, the chosen
//! audio tracks of each mixed into one, the ranges joined, then the video
//! and audio chains applied. A stream copy keeps the video as it is and only
//! needs a graph when the audio has to be mixed or filtered.

/// Which audio tracks go into the export and what happens to them.
#[derive(Debug, Clone, Copy)]
pub struct AudioPlan<'a> {
    /// Audio stream indices (`0:a:N`) to keep, mixed together when there
    /// are several. Empty drops the sound.
    pub tracks: &'a [usize],
    /// Filters applied to the mixed sound, such as a gain change.
    pub chain: Option<&'a str>,
}

impl AudioPlan<'_> {
    pub fn is_silent(&self) -> bool {
        self.tracks.is_empty()
    }

    /// Whether a stream copy can map the sound straight through.
    pub fn is_passthrough(&self) -> bool {
        self.tracks.len() == 1 && self.chain.is_none()
    }

    /// The label carrying input `input`'s sound, preceded by the filter
    /// that mixes its tracks when there are several.
    fn input_label(&self, input: usize, graph: &mut Vec<String>) -> String {
        match self.tracks {
            [track] => format!("[{input}:a:{track}]"),
            tracks => {
                let sources: String = tracks
                    .iter()
                    .map(|track| format!("[{input}:a:{track}]"))
                    .collect();
                let label = format!("[mix{input}]");
                graph.push(format!(
                    "{sources}amix=inputs={}:duration=longest:normalize=0{label}",
                    tracks.len()
                ));
                label
            }
        }
    }
}

/// The graph for a re-encoded export of `inputs` seeked inputs, joined in
/// order. Its outputs are `[v]` and, unless the plan is silent, `[a]`.
pub fn reencode(inputs: usize, video_chain: Option<&str>, audio: AudioPlan) -> String {
    let mut graph = Vec::new();
    let with_audio = !audio.is_silent();
    let audio_labels: Vec<String> = if with_audio {
        (0..inputs)
            .map(|input| audio.input_label(input, &mut graph))
            .collect()
    } else {
        Vec::new()
    };

    let (video, sound) = if inputs == 1 {
        ("[0:v:0]".to_string(), audio_labels.first().cloned())
    } else {
        let mut join = String::new();
        for input in 0..inputs {
            join.push_str(&format!("[{input}:v:0]"));
            if let Some(label) = audio_labels.get(input) {
                join.push_str(label);
            }
        }
        join.push_str(&format!(
            "concat=n={inputs}:v=1:a={}[joined]",
            u8::from(with_audio)
        ));
        if with_audio {
            join.push_str("[joineda]");
        }
        graph.push(join);
        (
            "[joined]".to_string(),
            with_audio.then(|| "[joineda]".to_string()),
        )
    };

    graph.push(format!("{video}{}[v]", video_chain.unwrap_or("null")));
    if let Some(sound) = sound {
        graph.push(format!("{sound}{}[a]", audio.chain.unwrap_or("anull")));
    }
    graph.join(";")
}

/// The audio graph for a stream copy from input 0, ending in `[a]`, or
/// `None` when the sound is dropped or mapped straight through.
pub fn copy_audio(audio: AudioPlan) -> Option<String> {
    if audio.is_silent() || audio.is_passthrough() {
        return None;
    }
    let mut graph = Vec::new();
    let label = audio.input_label(0, &mut graph);
    graph.push(format!("{label}{}[a]", audio.chain.unwrap_or("anull")));
    Some(graph.join(";"))
}

#[cfg(test)]
mod tests {
    use super::*;

    const fn plan(tracks: &[usize]) -> AudioPlan<'_> {
        AudioPlan {
            tracks,
            chain: None,
        }
    }

    #[test]
    fn single_range_with_one_track() {
        assert_eq!(
            reencode(1, None, plan(&[0])),
            "[0:v:0]null[v];[0:a:0]anull[a]"
        );
        assert_eq!(reencode(1, Some("hflip"), plan(&[])), "[0:v:0]hflip[v]");
    }

    #[test]
    fn mixes_tracks_before_joining() {
        assert_eq!(
            reencode(2, Some("hflip"), plan(&[0, 2])),
            "[0:a:0][0:a:2]amix=inputs=2:duration=longest:normalize=0[mix0];\
             [1:a:0][1:a:2]amix=inputs=2:duration=longest:normalize=0[mix1];\
             [0:v:0][mix0][1:v:0][mix1]concat=n=2:v=1:a=1[joined][joineda];\
             [joined]hflip[v];[joineda]anull[a]"
        );
        assert_eq!(
            reencode(2, None, plan(&[])),
            "[0:v:0][1:v:0]concat=n=2:v=1:a=0[joined];[joined]null[v]"
        );
    }

    #[test]
    fn stream_copy_only_filters_audio_when_it_must() {
        assert_eq!(copy_audio(plan(&[1])), None);
        assert_eq!(copy_audio(plan(&[])), None);
        assert_eq!(
            copy_audio(plan(&[0, 1])),
            Some(
                "[0:a:0][0:a:1]amix=inputs=2:duration=longest:normalize=0[mix0];[mix0]anull[a]"
                    .into()
            )
        );
        assert_eq!(
            copy_audio(AudioPlan {
                tracks: &[0],
                chain: Some("volume=3dB"),
            }),
            Some("[0:a:0]volume=3dB[a]".into())
        );
    }
}
