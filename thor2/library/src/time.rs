//! A UTC timestamp with no date dependency.
//!
//! A SECOND COPY of `serve::time`, on purpose. This crate deliberately depends
//! on nothing from the code lane (see Cargo.toml), and reaching into `serve`
//! for one timestamp would undo that for the sake of thirty lines. Duplication
//! beats the wrong coupling here; if a third copy ever appears, that is the
//! moment to lift it into a shared crate.
//!
//! `civil_from_days` is Howard Hinnant's days-since-epoch -> (y, m, d)
//! algorithm. `iso8601_from_unix` is pure, so it is testable without a clock.

/// Days since the Unix epoch -> (year, month, day), proleptic Gregorian.
/// http://howardhinnant.github.io/date_algorithms.html - `civil_from_days`.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = (if mp < 10 { mp + 3 } else { mp - 9 }) as u32;
    (y + if m <= 2 { 1 } else { 0 }, m, d)
}

/// A Unix timestamp (seconds, UTC) as `YYYY-MM-DD`. The library keeps dates,
/// not instants: a reading list, a diary and a ledger are all read by day.
pub fn date_from_unix(total_secs: i64) -> String {
    let (y, m, d) = civil_from_days(total_secs.div_euclid(86400));
    format!("{y:04}-{m:02}-{d:02}")
}

/// Today, UTC. Falls back to the epoch on a clock reporting before it, so a
/// system-clock oddity can never panic a write.
pub fn today() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    date_from_unix(secs)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_dates_round_trip() {
        assert_eq!(date_from_unix(0), "1970-01-01");
        assert_eq!(date_from_unix(1_000_000_000), "2001-09-09");
        // A leap day, the case this algorithm exists to get right.
        assert_eq!(date_from_unix(1_709_164_800), "2024-02-29");
    }

    #[test]
    fn a_clock_before_the_epoch_never_panics() {
        assert_eq!(date_from_unix(-1), "1969-12-31");
    }
}
