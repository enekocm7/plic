//! Bookmarks dropped on a clip while watching it.

use std::time::Duration;

/// Markers closer than this are the same marker.
const TOLERANCE: Duration = Duration::from_millis(250);

/// A sorted set of positions in a clip.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Markers {
    times: Vec<Duration>,
}

impl Markers {
    pub fn times(&self) -> &[Duration] {
        &self.times
    }

    /// Adds a marker at `at`, or removes the one already there.
    pub fn toggle(&mut self, at: Duration) {
        match self.times.iter().position(|time| near(*time, at)) {
            Some(index) => {
                self.times.remove(index);
            }
            None => {
                let index = self.times.partition_point(|time| *time < at);
                self.times.insert(index, at);
            }
        }
    }

    /// The first marker after `at`.
    pub fn next_after(&self, at: Duration) -> Option<Duration> {
        self.times
            .iter()
            .copied()
            .find(|time| *time > at && !near(*time, at))
    }

    /// The last marker before `at`.
    pub fn previous_before(&self, at: Duration) -> Option<Duration> {
        self.times
            .iter()
            .rev()
            .copied()
            .find(|time| *time < at && !near(*time, at))
    }
}

fn near(a: Duration, b: Duration) -> bool {
    a.abs_diff(b) < TOLERANCE
}

#[cfg(test)]
mod tests {
    use super::*;

    fn secs(s: u64) -> Duration {
        Duration::from_secs(s)
    }

    #[test]
    fn toggling_adds_in_order_and_removes() {
        let mut markers = Markers::default();
        markers.toggle(secs(10));
        markers.toggle(secs(2));
        assert_eq!(markers.times(), [secs(2), secs(10)]);
        markers.toggle(secs(10) + Duration::from_millis(100));
        assert_eq!(markers.times(), [secs(2)]);
    }

    #[test]
    fn finds_neighbours_skipping_the_marker_under_the_playhead() {
        let mut markers = Markers::default();
        for s in [2, 5, 9] {
            markers.toggle(secs(s));
        }
        assert_eq!(markers.next_after(secs(5)), Some(secs(9)));
        assert_eq!(markers.previous_before(secs(5)), Some(secs(2)));
        assert_eq!(markers.previous_before(secs(1)), None);
        assert_eq!(markers.next_after(secs(9)), None);
    }
}
