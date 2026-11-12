//! The part of a clip that will be exported.

use std::time::Duration;

/// A trim range. `end == None` means "until the end of the video", which lets
/// a clip be exported without knowing its exact length.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TrimRange {
    pub start: Duration,
    pub end: Option<Duration>,
}

/// The shortest range the editor will produce, so handles cannot cross.
pub const MIN_LENGTH: Duration = Duration::from_millis(100);

impl TrimRange {
    pub fn new(start: Duration, end: Option<Duration>) -> Self {
        Self { start, end }
    }

    /// The end point, resolving an open end against the source length.
    pub fn resolved_end(&self, source_duration: Option<Duration>) -> Option<Duration> {
        self.end.or(source_duration)
    }

    /// Length of the range, if it can be known.
    pub fn length(&self, source_duration: Option<Duration>) -> Option<Duration> {
        self.resolved_end(source_duration)
            .map(|end| end.saturating_sub(self.start))
    }

    /// Whether the range covers the whole source.
    pub fn is_full(&self, source_duration: Option<Duration>) -> bool {
        self.start.is_zero()
            && match (self.end, source_duration) {
                (None, _) => true,
                (Some(end), Some(total)) => end >= total,
                (Some(_), None) => false,
            }
    }

    /// Checks the range against the source length and explains what is wrong.
    pub fn validate(&self, source_duration: Option<Duration>) -> Result<(), TrimError> {
        if let Some(total) = source_duration
            && self.start >= total
        {
            return Err(TrimError::StartPastEnd);
        }
        if let Some(end) = self.end
            && end <= self.start
        {
            return Err(TrimError::EndBeforeStart);
        }
        Ok(())
    }
}

/// The ranges marked on one clip. There is always at least one; the
/// active one is the range the handles, the time fields and the in/out
/// shortcuts edit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Segments {
    ranges: Vec<TrimRange>,
    active: usize,
}

impl Default for Segments {
    fn default() -> Self {
        Self {
            ranges: vec![TrimRange::default()],
            active: 0,
        }
    }
}

impl Segments {
    pub fn ranges(&self) -> &[TrimRange] {
        &self.ranges
    }

    pub fn len(&self) -> usize {
        self.ranges.len()
    }

    pub fn active_index(&self) -> usize {
        self.active
    }

    pub fn active(&self) -> TrimRange {
        self.ranges[self.active]
    }

    pub fn set_active(&mut self, range: TrimRange) {
        self.ranges[self.active] = range;
    }

    /// Makes another segment the one being edited.
    pub fn select(&mut self, index: usize) -> bool {
        let valid = index < self.ranges.len() && index != self.active;
        if valid {
            self.active = index;
        }
        valid
    }

    /// Starts a new segment at `at`, running until the next segment starts
    /// or the end of the video, and makes it the active one. Segments may
    /// overlap; nothing is added if one already starts at `at`.
    pub fn add(&mut self, at: Duration, source_duration: Option<Duration>) -> bool {
        if source_duration.is_some_and(|total| at + MIN_LENGTH > total)
            || self.ranges.iter().any(|range| range.start == at)
        {
            return false;
        }
        let end = self
            .ranges
            .iter()
            .map(|range| range.start)
            .filter(|start| *start >= at + MIN_LENGTH)
            .min();
        self.ranges.push(TrimRange::new(at, end));
        self.active = self.ranges.len() - 1;
        true
    }

    /// Drops a segment, keeping at least one.
    pub fn remove(&mut self, index: usize) -> bool {
        if self.ranges.len() < 2 || index >= self.ranges.len() {
            return false;
        }
        self.ranges.remove(index);
        if self.active > index || self.active == self.ranges.len() {
            self.active -= 1;
        }
        true
    }

    /// The segments in the order they appear in the video, which is the
    /// order they are exported and joined in.
    pub fn in_order(&self) -> Vec<TrimRange> {
        let mut ranges = self.ranges.clone();
        ranges.sort_by_key(|range| range.start);
        ranges
    }

    /// Combined length of every segment, if it can be known.
    pub fn total_length(&self, source_duration: Option<Duration>) -> Option<Duration> {
        self.ranges
            .iter()
            .map(|range| range.length(source_duration))
            .sum()
    }

    /// Checks every segment, naming the first one that is wrong.
    pub fn validate(&self, source_duration: Option<Duration>) -> Result<(), String> {
        for (index, range) in self.ranges.iter().enumerate() {
            range.validate(source_duration).map_err(|error| {
                if self.ranges.len() == 1 {
                    error.to_string()
                } else {
                    format!("Segment {}: {error}", index + 1)
                }
            })?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrimError {
    StartPastEnd,
    EndBeforeStart,
}

impl std::fmt::Display for TrimError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::StartPastEnd => "Start is past the end of the video",
            Self::EndBeforeStart => "End must be later than start",
        })
    }
}

impl std::error::Error for TrimError {}

#[cfg(test)]
mod tests {
    use super::*;

    const fn s(seconds: u64) -> Duration {
        Duration::from_secs(seconds)
    }

    #[test]
    fn open_end_resolves_against_source_length() {
        let range = TrimRange::new(s(10), None);
        assert_eq!(range.length(Some(s(60))), Some(s(50)));
        assert_eq!(range.length(None), None);
        assert_eq!(TrimRange::new(s(10), Some(s(25))).length(None), Some(s(15)));
    }

    #[test]
    fn detects_full_ranges() {
        assert!(TrimRange::default().is_full(None));
        assert!(TrimRange::new(s(0), Some(s(60))).is_full(Some(s(60))));
        assert!(!TrimRange::new(s(1), None).is_full(Some(s(60))));
        assert!(!TrimRange::new(s(0), Some(s(59))).is_full(Some(s(60))));
    }

    #[test]
    fn segments_add_select_and_remove() {
        let mut segments = Segments::default();
        segments.set_active(TrimRange::new(s(10), Some(s(20))));
        // A new segment runs until the next one starts.
        assert!(segments.add(s(2), Some(s(60))));
        assert_eq!(segments.active(), TrimRange::new(s(2), Some(s(10))));
        assert!(segments.add(s(30), Some(s(60))));
        assert_eq!(segments.active(), TrimRange::new(s(30), None));
        // Not twice at the same point, nor past the end.
        assert!(!segments.add(s(30), Some(s(60))));
        assert!(!segments.add(s(60), Some(s(60))));

        assert_eq!(
            segments.in_order(),
            vec![
                TrimRange::new(s(2), Some(s(10))),
                TrimRange::new(s(10), Some(s(20))),
                TrimRange::new(s(30), None),
            ]
        );
        assert_eq!(segments.total_length(Some(s(60))), Some(s(48)));

        assert!(segments.select(0));
        assert!(segments.remove(2));
        assert_eq!(segments.active_index(), 0);
        assert!(segments.remove(0));
        assert_eq!(segments.active(), TrimRange::new(s(2), Some(s(10))));
        // The last segment stays.
        assert!(!segments.remove(0));
    }

    #[test]
    fn validation_rejects_inverted_or_out_of_bounds_ranges() {
        assert_eq!(
            TrimRange::new(s(5), Some(s(5))).validate(None),
            Err(TrimError::EndBeforeStart)
        );
        assert_eq!(
            TrimRange::new(s(70), None).validate(Some(s(60))),
            Err(TrimError::StartPastEnd)
        );
        assert_eq!(
            TrimRange::new(s(5), Some(s(6))).validate(Some(s(60))),
            Ok(())
        );
    }
}
