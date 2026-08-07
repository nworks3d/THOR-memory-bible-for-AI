//! A UTC ISO-8601 timestamp for `ItemServed.served_at`, with no date/time
//! dependency: `thor-core` deliberately dropped `chrono` (none of its ported
//! files use it - see `core/Cargo.toml`), so this crate does not reintroduce
//! it for one timestamp. `civil_from_days` is Howard Hinnant's well-known
//! days-since-epoch -> (y, m, d) algorithm; `iso8601_from_unix` is pure and
//! unit-tested against known values, `now_iso8601` is the only impure caller.

/// Days since the Unix epoch -> (year, month, day), proleptic Gregorian.
/// http://howardhinnant.github.io/date_algorithms.html - `civil_from_days`.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = z - era * 146097; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let m = (if mp < 10 { mp + 3 } else { mp - 9 }) as u32; // [1, 12]
    (y + if m <= 2 { 1 } else { 0 }, m, d)
}

/// Format a Unix timestamp (seconds since epoch, UTC) as
/// `YYYY-MM-DDTHH:MM:SSZ`. Pure, so it is unit-testable without a clock.
pub fn iso8601_from_unix(total_secs: i64) -> String {
    let days = total_secs.div_euclid(86400);
    let secs_of_day = total_secs.rem_euclid(86400);
    let (y, m, d) = civil_from_days(days);
    let h = secs_of_day / 3600;
    let min = (secs_of_day % 3600) / 60;
    let s = secs_of_day % 60;
    format!("{y:04}-{m:02}-{d:02}T{h:02}:{min:02}:{s:02}Z")
}

/// The current instant, as `served_at` is recorded. Falls back to the Unix
/// epoch on a clock that reports before it (never observed in practice; kept
/// so this can never panic on a system clock oddity - a read/serve-path
/// helper must not be the thing that turns a fail-open path loud).
pub fn now_iso8601() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    iso8601_from_unix(secs)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_timestamps_format_correctly() {
        assert_eq!(iso8601_from_unix(0), "1970-01-01T00:00:00Z");
        assert_eq!(iso8601_from_unix(1_700_000_000), "2023-11-14T22:13:20Z");
        assert_eq!(iso8601_from_unix(1_893_456_000), "2030-01-01T00:00:00Z");
        assert_eq!(iso8601_from_unix(1_754_140_800), "2025-08-02T13:20:00Z");
        assert_eq!(iso8601_from_unix(157_680_000), "1974-12-31T00:00:00Z");
    }

    #[test]
    fn now_produces_a_well_formed_stamp() {
        let s = now_iso8601();
        assert_eq!(s.len(), 20, "{s}");
        assert!(s.ends_with('Z'));
        assert!(s.starts_with("20"), "sanity: this code did not run before the year 2000: {s}");
    }
}
