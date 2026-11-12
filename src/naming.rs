//! Export name templates such as `{folder}-{date}-{n}`.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// What a template can refer to.
pub struct TemplateContext<'a> {
    /// The clip's file name without its extension: `{name}`.
    pub name: &'a str,
    /// Name of the clips folder: `{folder}`.
    pub folder: &'a str,
    /// When the clip was recorded: `{date}`.
    pub recorded: Option<SystemTime>,
}

/// Fills in a template. `{n}` becomes the lowest number for which `taken`
/// does not report the result as used.
pub fn expand(template: &str, context: &TemplateContext, taken: impl Fn(&str) -> bool) -> String {
    let date = context.recorded.map(format_date).unwrap_or_default();
    let base = template
        .replace("{name}", context.name)
        .replace("{folder}", context.folder)
        .replace("{date}", &date);
    if !base.contains("{n}") {
        return base;
    }
    (1..)
        .map(|n| base.replace("{n}", &n.to_string()))
        .find(|candidate| !taken(candidate))
        .expect("an unbounded range always ends in a free name")
}

/// `2026-11-12`, in UTC.
fn format_date(time: SystemTime) -> String {
    let days = time
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::ZERO)
        .as_secs()
        / 86_400;
    let (year, month, day) = civil_from_days(days as i64);
    format!("{year:04}-{month:02}-{day:02}")
}

/// Days since 1970-01-01 to a calendar date (Howard Hinnant's algorithm).
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let day_of_era = z.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let shifted_month = (5 * day_of_year + 2) / 153;
    let day = (day_of_year - (153 * shifted_month + 2) / 5 + 1) as u32;
    let month = if shifted_month < 10 {
        shifted_month + 3
    } else {
        shifted_month - 9
    } as u32;
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn context(recorded: Option<SystemTime>) -> TemplateContext<'static> {
        TemplateContext {
            name: "clip",
            folder: "Videos",
            recorded,
        }
    }

    #[test]
    fn fills_in_variables() {
        // 2026-11-12 00:00:00 UTC
        let recorded = UNIX_EPOCH + Duration::from_secs(1_794_441_600);
        assert_eq!(
            expand("{folder}-{date}-{name}", &context(Some(recorded)), |_| {
                false
            }),
            "Videos-2026-11-12-clip"
        );
    }

    #[test]
    fn increments_past_used_names() {
        let used = ["run-1", "run-2"];
        let name = expand("run-{n}", &context(None), |candidate| {
            used.contains(&candidate)
        });
        assert_eq!(name, "run-3");
    }

    #[test]
    fn dates_around_leap_days() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(19_782), (2024, 2, 29));
    }
}
