//! How the clip list is searched, sorted and filtered.

use std::{cmp::Ordering, time::Duration, time::SystemTime};

/// What the clip list is ordered by.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SortKey {
    #[default]
    Name,
    Date,
    Duration,
    Size,
}

impl SortKey {
    pub fn next(self) -> Self {
        match self {
            Self::Name => Self::Date,
            Self::Date => Self::Duration,
            Self::Duration => Self::Size,
            Self::Size => Self::Name,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Name => "Name",
            Self::Date => "Date",
            Self::Duration => "Length",
            Self::Size => "Size",
        }
    }
}

/// Which clips the list shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum StatusFilter {
    #[default]
    All,
    /// Clips still to be edited.
    New,
    /// Originals already exported.
    Edited,
}

impl StatusFilter {
    pub fn next(self) -> Self {
        match self {
            Self::All => Self::New,
            Self::New => Self::Edited,
            Self::Edited => Self::All,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::All => "All",
            Self::New => "New",
            Self::Edited => "Edited",
        }
    }
}

/// What the sort needs to know about a clip.
pub struct SortItem<'a> {
    pub name: &'a str,
    pub modified: Option<SystemTime>,
    pub duration: Option<Duration>,
    pub size: u64,
}

/// Indices of the items that match `query`, in display order.
pub fn visible(items: &[SortItem], query: &str, key: SortKey, descending: bool) -> Vec<usize> {
    let query = query.trim().to_lowercase();
    let mut indices: Vec<usize> = (0..items.len())
        .filter(|&index| query.is_empty() || items[index].name.to_lowercase().contains(&query))
        .collect();
    indices.sort_by(|&a, &b| {
        let (a, b) = (&items[a], &items[b]);
        let order = match key {
            SortKey::Name => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
            SortKey::Date => a.modified.cmp(&b.modified),
            // Clips whose length is not known yet go last either way.
            SortKey::Duration => match (a.duration, b.duration) {
                (Some(a), Some(b)) => a.cmp(&b),
                (Some(_), None) => return Ordering::Less,
                (None, Some(_)) => return Ordering::Greater,
                (None, None) => Ordering::Equal,
            },
            SortKey::Size => a.size.cmp(&b.size),
        };
        if descending { order.reverse() } else { order }
    });
    indices
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(name: &str, size: u64, duration: Option<u64>) -> SortItem<'_> {
        SortItem {
            name,
            modified: None,
            duration: duration.map(Duration::from_secs),
            size,
        }
    }

    #[test]
    fn searches_names_ignoring_case() {
        let items = [item("Alpha.mp4", 1, None), item("beta.mp4", 2, None)];
        assert_eq!(visible(&items, "ALP", SortKey::Name, false), [0]);
        assert_eq!(visible(&items, "  ", SortKey::Name, false), [0, 1]);
    }

    #[test]
    fn sorts_either_way() {
        let items = [
            item("b", 30, Some(5)),
            item("a", 10, None),
            item("c", 20, Some(1)),
        ];
        assert_eq!(visible(&items, "", SortKey::Name, false), [1, 0, 2]);
        assert_eq!(visible(&items, "", SortKey::Size, true), [0, 2, 1]);
        // Unknown lengths stay last in both directions.
        assert_eq!(visible(&items, "", SortKey::Duration, false), [2, 0, 1]);
        assert_eq!(visible(&items, "", SortKey::Duration, true), [0, 2, 1]);
    }
}
