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
