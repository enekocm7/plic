//! Unfinished work on each clip, kept between runs: the segments marked,
//! the export name and the destination folder. Stored in the app data
//! directory rather than next to the recordings, and dropped once a clip
//! has been exported.

use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    sync::{
        Mutex, OnceLock,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use serde::{Deserialize, Serialize};

use crate::trim::{Segments, TrimRange};

/// One segment as saved, in milliseconds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct SavedSegment {
    start_ms: u64,
    end_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    name: String,
}

/// What is remembered about one clip.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Draft {
    /// Size of the file when the draft was saved. A recording replaced by
    /// another of a different size does not inherit the old trims.
    size: u64,
    segments: Vec<SavedSegment>,
    active: usize,
    /// The export name, if it differs from the file's own name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub folder: Option<PathBuf>,
}

impl Draft {
    /// A draft of `segments`, `name` and `folder` for a file `size` bytes
    /// long, or `None` if there is nothing worth remembering.
    pub fn new(
        size: u64,
        segments: &Segments,
        name: Option<String>,
        folder: Option<PathBuf>,
    ) -> Option<Self> {
        if *segments == Segments::default() && name.is_none() && folder.is_none() {
            return None;
        }
        let millis = |duration: Duration| duration.as_millis() as u64;
        Some(Self {
            size,
            segments: segments
                .ranges()
                .iter()
                .zip(segments.names())
                .map(|(range, name)| SavedSegment {
                    start_ms: millis(range.start),
                    end_ms: range.end.map(millis),
                    name: name.clone(),
                })
                .collect(),
            active: segments.active_index(),
            name,
            folder,
        })
    }

    /// The saved segments, if they still make sense.
    pub fn segments(&self) -> Option<Segments> {
        let parts = self
            .segments
            .iter()
            .map(|segment| {
                (
                    TrimRange::new(
                        Duration::from_millis(segment.start_ms),
                        segment.end_ms.map(Duration::from_millis),
                    ),
                    segment.name.clone(),
                )
            })
            .collect();
        Segments::from_parts(parts, self.active)
    }

    pub fn matches_size(&self, size: u64) -> bool {
        self.size == size
    }
}

#[derive(Default)]
struct Store {
    drafts: BTreeMap<PathBuf, Draft>,
}

fn store() -> &'static Mutex<Store> {
    static STORE: OnceLock<Mutex<Store>> = OnceLock::new();
    STORE.get_or_init(Default::default)
}

/// Set when the drafts changed since they were last written.
static DIRTY: AtomicBool = AtomicBool::new(false);

fn file() -> Option<PathBuf> {
    dirs::data_dir().map(|dir| dir.join("plic").join("drafts.json"))
}

/// Reads the saved drafts, if there are any.
pub fn load() {
    let loaded = file()
        .and_then(|path| fs::read_to_string(path).ok())
        .and_then(|text| serde_json::from_str::<BTreeMap<PathBuf, Draft>>(&text).ok());
    if let (Some(drafts), Ok(mut store)) = (loaded, store().lock()) {
        store.drafts = drafts;
    }
}

/// The draft saved for the clip at `path`.
pub fn get(path: &Path) -> Option<Draft> {
    store().lock().ok()?.drafts.get(path).cloned()
}

/// Remembers `draft` for `path`, or forgets it if there is none. Call
/// [`save`] to write the change.
pub fn put(path: &Path, draft: Option<Draft>) {
    let Ok(mut store) = store().lock() else {
        return;
    };
    let changed = match draft {
        Some(draft) => {
            store.drafts.get(path) != Some(&draft) && {
                store.drafts.insert(path.to_path_buf(), draft);
                true
            }
        }
        None => store.drafts.remove(path).is_some(),
    };
    if changed {
        DIRTY.store(true, Ordering::Relaxed);
    }
}

/// Forgets the draft of a clip, e.g. once it has been exported.
pub fn forget(path: &Path) {
    put(path, None);
}

/// Whether there are changes that [`save`] would write.
pub fn is_dirty() -> bool {
    DIRTY.load(Ordering::Relaxed)
}

/// Writes the drafts if they changed. Drafts of files that no longer exist
/// are dropped on the way.
pub fn save() {
    if !DIRTY.swap(false, Ordering::Relaxed) {
        return;
    }
    let Some(path) = file() else {
        return;
    };
    let text = {
        let Ok(mut store) = store().lock() else {
            return;
        };
        store.drafts.retain(|clip, _| clip.exists());
        serde_json::to_string_pretty(&store.drafts)
    };
    let Ok(text) = text else {
        return;
    };
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).ok();
    }
    fs::write(path, text).ok();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn untouched_clips_have_no_draft() {
        assert_eq!(Draft::new(10, &Segments::default(), None, None), None);
    }

    #[test]
    fn segments_survive_a_round_trip() {
        let mut segments = Segments::default();
        segments.set_active(TrimRange::new(
            Duration::from_millis(1_500),
            Some(Duration::from_secs(4)),
        ));
        segments.add(Duration::from_secs(10), None);
        segments.set_active_name("ending".into());
        let draft = Draft::new(42, &segments, Some("best".into()), None).unwrap();
        let text = serde_json::to_string(&draft).unwrap();
        let loaded: Draft = serde_json::from_str(&text).unwrap();
        assert_eq!(loaded, draft);
        assert_eq!(loaded.segments(), Some(segments));
        assert!(loaded.matches_size(42));
        assert_eq!(loaded.name.as_deref(), Some("best"));
    }

    #[test]
    fn forgets_drafts() {
        let path = Path::new("/nowhere/clip.mp4");
        let mut segments = Segments::default();
        segments.set_active(TrimRange::new(Duration::from_secs(1), None));
        put(path, Draft::new(1, &segments, None, None));
        assert!(get(path).is_some());
        forget(path);
        assert!(get(path).is_none());
    }
}
