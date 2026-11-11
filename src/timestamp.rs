//! Parsing and formatting of the human-readable timestamps used for trimming.
//!
//! Accepted forms are `SS`, `M:SS` and `H:MM:SS`, each with an optional
//! fractional part (`1:02.5`, `0:07.25`). Precision is kept to the
//! millisecond, which is finer than any frame interval we care about.

use std::time::Duration;

/// Parses a timestamp such as `90`, `2:30`, `1:02:03` or `4:21.75`.
///
/// Returns `None` for malformed input, out-of-range minute/second
/// components, or values that would overflow.
pub fn parse(value: &str) -> Option<Duration> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }

    let (whole, fraction) = match value.split_once('.') {
        Some((whole, fraction)) => (whole, Some(fraction)),
        None => (value, None),
    };

    let parts: Vec<&str> = whole.split(':').collect();
    if parts.len() > 3 || parts.iter().any(|part| !is_digits(part)) {
        return None;
    }
    let numbers: Vec<u64> = parts
        .iter()
        .map(|part| part.parse().ok())
        .collect::<Option<_>>()?;

    let (hours, minutes, seconds) = match numbers.as_slice() {
        [seconds] => (0, 0, *seconds),
        [minutes, seconds] if *seconds < 60 => (0, *minutes, *seconds),
        [hours, minutes, seconds] if *minutes < 60 && *seconds < 60 => (*hours, *minutes, *seconds),
        _ => return None,
    };

    let millis = match fraction {
        None => 0,
        Some(fraction) if !fraction.is_empty() && is_digits(fraction) => {
            // Keep the first three digits; anything finer is noise.
            let digits: String = fraction.chars().chain("000".chars()).take(3).collect();
            digits.parse::<u64>().ok()?
        }
        Some(_) => return None,
    };

    let total_seconds = hours
        .checked_mul(3600)?
        .checked_add(minutes.checked_mul(60)?)?
        .checked_add(seconds)?;
    Some(Duration::from_secs(total_seconds) + Duration::from_millis(millis))
}

/// Formats a duration as `M:SS` or `H:MM:SS`, adding hundredths only when the
/// value is not a whole second (`1:02.50` becomes `1:02.5`).
pub fn format(duration: Duration) -> String {
    let centis = (duration.as_millis() + 5) / 10;
    let total_seconds = centis / 100;
    let hundredths = centis % 100;

    let mut text = format_seconds(total_seconds);
    if hundredths != 0 {
        let fraction = format!("{hundredths:02}");
        text.push('.');
        text.push_str(fraction.trim_end_matches('0'));
    }
    text
}

/// Formats a duration for display next to a running playhead, always showing
/// tenths so the readout does not change width while playing.
pub fn format_precise(duration: Duration) -> String {
    let tenths = duration.as_millis() / 100;
    format!("{}.{}", format_seconds(tenths as u64 / 10), tenths % 10)
}

/// Formats a duration as `M:SS` / `H:MM:SS`, truncating sub-second precision.
pub fn format_coarse(duration: Duration) -> String {
    format_seconds(duration.as_secs())
}

fn format_seconds(total_seconds: impl Into<u128>) -> String {
    let total_seconds: u128 = total_seconds.into();
    let hours = total_seconds / 3600;
    let minutes = (total_seconds % 3600) / 60;
    let seconds = total_seconds % 60;
    if hours > 0 {
        format!("{hours}:{minutes:02}:{seconds:02}")
    } else {
        format!("{minutes}:{seconds:02}")
    }
}

fn is_digits(text: &str) -> bool {
    !text.is_empty() && text.bytes().all(|byte| byte.is_ascii_digit())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn secs(value: f64) -> Duration {
        Duration::from_secs_f64(value)
    }

    #[test]
    fn parses_every_supported_shape() {
        assert_eq!(parse("45"), Some(secs(45.0)));
        assert_eq!(parse("2:30"), Some(secs(150.0)));
        assert_eq!(parse("1:02:03"), Some(secs(3723.0)));
        assert_eq!(parse(" 0:07.25 "), Some(secs(7.25)));
        assert_eq!(parse("4:21.5"), Some(secs(261.5)));
        assert_eq!(parse("0:00.1234"), Some(Duration::from_millis(123)));
    }

    #[test]
    fn rejects_malformed_input() {
        for input in [
            "", ":", "1:", ":30", "1:60", "1:00:60", "1:60:00", "a:bc", "1:2:3:4", "1.", "1.x",
            "-1",
        ] {
            assert_eq!(parse(input), None, "{input:?} should not parse");
        }
    }

    #[test]
    fn formats_whole_and_fractional_values() {
        assert_eq!(format(secs(0.0)), "0:00");
        assert_eq!(format(secs(150.0)), "2:30");
        assert_eq!(format(secs(3723.0)), "1:02:03");
        assert_eq!(format(secs(62.5)), "1:02.5");
        assert_eq!(format(secs(7.25)), "0:07.25");
        assert_eq!(format(Duration::from_millis(59_996)), "1:00");
    }

    #[test]
    fn format_round_trips_through_parse() {
        for millis in [0, 10, 990, 61_250, 3_599_990, 7_200_000] {
            let duration = Duration::from_millis(millis);
            assert_eq!(parse(&format(duration)), Some(duration), "{millis} ms");
        }
    }

    #[test]
    fn precise_format_keeps_a_stable_width() {
        assert_eq!(format_precise(secs(0.0)), "0:00.0");
        assert_eq!(format_precise(secs(61.27)), "1:01.2");
        assert_eq!(format_coarse(secs(61.9)), "1:01");
    }
}
