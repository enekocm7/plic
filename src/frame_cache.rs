//! A small cache of decoded frames, so the first picture of a clip that was
//! just opened, or is likely to be opened next, appears without waiting for
//! ffmpeg to start.

use std::{
    collections::VecDeque,
    path::{Path, PathBuf},
    time::{Duration, SystemTime},
};

use crate::ffmpeg::{FrameSize, RawFrame};

/// What identifies a cached frame: the clip as it is on disk, the moment and
/// the size it was decoded at.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct FrameKey {
    path: PathBuf,
    at: Duration,
    size: FrameSize,
    /// Length and modification time, so a file that was replaced under the
    /// same name is decoded again.
    stamp: (u64, Option<SystemTime>),
}

impl FrameKey {
    /// `None` when the file cannot be read.
    pub fn new(path: &Path, at: Duration, size: FrameSize) -> Option<Self> {
        let metadata = std::fs::metadata(path).ok()?;
        Some(Self {
            path: path.to_path_buf(),
            at,
            size,
            stamp: (metadata.len(), metadata.modified().ok()),
        })
    }
}

/// Frames kept up to a memory budget, the most recently used last.
pub struct FrameCache {
    entries: VecDeque<(FrameKey, RawFrame)>,
    bytes: usize,
    budget: usize,
}

impl FrameCache {
    pub fn new(budget: usize) -> Self {
        Self {
            entries: VecDeque::new(),
            bytes: 0,
            budget,
        }
    }

    pub fn contains(&self, key: &FrameKey) -> bool {
        self.entries.iter().any(|(other, _)| other == key)
    }

    /// A copy of the frame under `key`, which becomes the newest.
    pub fn get(&mut self, key: &FrameKey) -> Option<RawFrame> {
        let index = self.entries.iter().position(|(other, _)| other == key)?;
        let entry = self.entries.remove(index)?;
        let frame = entry.1.clone();
        self.entries.push_back(entry);
        Some(frame)
    }

    /// Stores `frame`, dropping the oldest ones to stay within the budget. A
    /// frame bigger than the whole budget is not kept.
    pub fn insert(&mut self, key: FrameKey, frame: RawFrame) {
        if let Some(index) = self.entries.iter().position(|(other, _)| *other == key)
            && let Some((_, old)) = self.entries.remove(index)
        {
            self.bytes -= old.bgra.len();
        }
        if frame.bgra.len() > self.budget {
            return;
        }
        self.bytes += frame.bgra.len();
        self.entries.push_back((key, frame));
        while self.bytes > self.budget {
            let Some((_, old)) = self.entries.pop_front() else {
                break;
            };
            self.bytes -= old.bgra.len();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(name: &str) -> FrameKey {
        FrameKey {
            path: PathBuf::from(name),
            at: Duration::ZERO,
            size: FrameSize::new(2, 2),
            stamp: (0, None),
        }
    }

    fn frame(bytes: usize) -> RawFrame {
        RawFrame {
            size: FrameSize::new(2, 2),
            pts: Duration::ZERO,
            bgra: vec![0; bytes],
        }
    }

    #[test]
    fn evicts_the_least_recently_used_frame_over_budget() {
        let mut cache = FrameCache::new(30);
        cache.insert(key("a"), frame(10));
        cache.insert(key("b"), frame(10));
        cache.insert(key("c"), frame(10));
        // Using `a` makes `b` the oldest.
        assert!(cache.get(&key("a")).is_some());
        cache.insert(key("d"), frame(10));
        assert!(!cache.contains(&key("b")));
        assert!(
            cache.contains(&key("a")) && cache.contains(&key("c")) && cache.contains(&key("d"))
        );
    }

    #[test]
    fn replaces_a_frame_and_skips_ones_over_the_whole_budget() {
        let mut cache = FrameCache::new(20);
        cache.insert(key("a"), frame(15));
        cache.insert(key("a"), frame(5));
        cache.insert(key("b"), frame(15));
        assert!(cache.contains(&key("a")) && cache.contains(&key("b")));
        cache.insert(key("huge"), frame(21));
        assert!(!cache.contains(&key("huge")));
        assert!(cache.get(&key("missing")).is_none());
    }

    #[test]
    fn a_replaced_file_is_a_different_key() {
        let dir = std::env::temp_dir().join(format!("plic-cache-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("clip.mp4");
        std::fs::write(&path, b"one").unwrap();
        let size = FrameSize::new(2, 2);
        let before = FrameKey::new(&path, Duration::ZERO, size).unwrap();
        std::fs::write(&path, b"another").unwrap();
        let after = FrameKey::new(&path, Duration::ZERO, size).unwrap();
        std::fs::remove_dir_all(&dir).ok();
        assert_ne!(before, after);
        assert!(FrameKey::new(&dir.join("gone.mp4"), Duration::ZERO, size).is_none());
    }
}
