//! `beckon mute` and `beckon unmute`.
//!
//! The command people reach for mid-call. It has to be short to type, obvious
//! in its effect, and impossible to leave on by accident — which is why a mute
//! always has an expiry rather than being a toggle you forget about.

use crate::core::paths::Paths;
use crate::core::state;
use chrono::{Duration, Local, Utc};

/// Mute for this long when no duration is given.
const DEFAULT: Duration = Duration::minutes(30);

pub fn mute(spec: Option<String>) -> i32 {
    let duration = match spec.as_deref() {
        None => DEFAULT,
        Some(raw) => match parse_duration(raw) {
            Some(d) => d,
            None => {
                eprintln!(
                    "cannot read `{raw}` as a duration. Try 30s, 15m, 2h, or omit it for 30m."
                );
                return 2;
            }
        },
    };

    let paths = Paths::resolve();
    let until = Utc::now() + duration;
    state::write_mute(&paths, until);

    println!(
        "Muted for {} — until {}.",
        humanize(duration),
        until.with_timezone(&Local).format("%H:%M:%S")
    );
    println!("`beckon unmute` to end it early.");
    0
}

pub fn unmute() -> i32 {
    let paths = Paths::resolve();
    match state::read_mute(&paths) {
        Some(until) if until > Utc::now() => {
            state::clear_mute(&paths);
            println!("Unmuted.");
        }
        _ => {
            // Clear anyway: an expired marker is just litter.
            state::clear_mute(&paths);
            println!("Not muted.");
        }
    }
    0
}

/// `45s`, `30m`, `2h`. A bare number is minutes, which is what people mean.
pub fn parse_duration(raw: &str) -> Option<Duration> {
    let raw = raw.trim().to_ascii_lowercase();
    if raw.is_empty() {
        return None;
    }

    let (number, unit) = match raw.strip_suffix(|c: char| c.is_ascii_alphabetic()) {
        Some(head) => (head, raw.chars().last()?),
        None => (raw.as_str(), 'm'),
    };

    let amount: i64 = number.trim().parse().ok()?;
    if amount <= 0 {
        return None;
    }

    let duration = match unit {
        's' => Duration::seconds(amount),
        'm' => Duration::minutes(amount),
        'h' => Duration::hours(amount),
        _ => return None,
    };
    // A mute you cannot remember setting is a bug report about silence.
    (duration <= Duration::hours(24)).then_some(duration)
}

fn humanize(d: Duration) -> String {
    let seconds = d.num_seconds();
    if seconds % 3600 == 0 && seconds >= 3600 {
        format!("{}h", seconds / 3600)
    } else if seconds % 60 == 0 && seconds >= 60 {
        format!("{}m", seconds / 60)
    } else {
        format!("{seconds}s")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn units_are_understood() {
        assert_eq!(parse_duration("45s"), Some(Duration::seconds(45)));
        assert_eq!(parse_duration("30m"), Some(Duration::minutes(30)));
        assert_eq!(parse_duration("2h"), Some(Duration::hours(2)));
    }

    #[test]
    fn a_bare_number_means_minutes() {
        assert_eq!(parse_duration("15"), Some(Duration::minutes(15)));
    }

    #[test]
    fn spacing_and_case_are_forgiven() {
        assert_eq!(parse_duration(" 30M "), Some(Duration::minutes(30)));
        assert_eq!(parse_duration("2H"), Some(Duration::hours(2)));
    }

    #[test]
    fn nonsense_is_refused_rather_than_guessed() {
        for bad in [
            "", "  ", "soon", "m", "-5m", "0", "0m", "5x", "1.5h", "5 m 3s",
        ] {
            assert!(parse_duration(bad).is_none(), "{bad:?} should not parse");
        }
    }

    #[test]
    fn a_mute_cannot_outlast_a_day() {
        // Silence you forget you asked for reads as a broken tool.
        assert!(parse_duration("24h").is_some());
        assert!(parse_duration("25h").is_none());
        assert!(parse_duration("2000m").is_none());
    }

    #[test]
    fn durations_read_back_the_way_they_were_written() {
        assert_eq!(humanize(Duration::hours(2)), "2h");
        assert_eq!(humanize(Duration::minutes(30)), "30m");
        assert_eq!(humanize(Duration::seconds(45)), "45s");
        assert_eq!(humanize(Duration::seconds(90)), "90s");
    }
}
