//! A UTC ISO-8601 timestamp for `ItemServed.served_at`, with no date/time
//! dependency: `thor-core` deliberately dropped `chrono` (none of its ported
//! files use it - see `core/Cargo.toml`), so this crate does not reintroduce
//! it for one timestamp. `civil_from_days`/`days_from_civil` are Howard
//! Hinnant's well-known days-since-epoch <-> (y, m, d) algorithms;
//! `iso8601_from_unix`/`unix_from_iso8601` are pure and unit-tested against
//! known values, `now_unix`/`now_iso8601` are the only impure callers.

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

/// (year, month, day), proleptic Gregorian -> days since the Unix epoch: the
/// exact inverse of `civil_from_days` above, same source algorithm
/// (`days_from_civil`). Needed by `unix_from_iso8601` - the evaluation debt
/// (`usefulness::newest_verdict_unix`) has to turn a stored `marked_at` back
/// into an instant it can compare against "now", and every timestamp this
/// codebase ever writes is already in this exact `YYYY-MM-DDTHH:MM:SSZ`
/// shape (`ItemMarkedUseful`/`ItemMarkedNoise`'s own `marked_at`, `ItemServed`'s
/// `served_at`), so no other format needs to be understood.
fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400; // [0, 399]
    let mp = (m as i64 + 9) % 12; // [0, 11]
    let doy = (153 * mp + 2) / 5 + d as i64 - 1; // [0, 365]
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy; // [0, 146096]
    era * 146097 + doe - 719468
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

/// Parse a `YYYY-MM-DDTHH:MM:SSZ` UTC timestamp - exactly the shape
/// `iso8601_from_unix` produces, and the only shape this codebase ever
/// writes into a `marked_at`/`served_at` field - back into Unix seconds.
/// `None` on anything that does not match that exact shape, rather than
/// guessing at a looser one: a malformed or foreign timestamp must read as
/// "unknown" to its one caller (`usefulness::newest_verdict_unix`), never as
/// a wrong instant silently accepted, since that reader's whole job is
/// deciding whether an instant is recent enough to cancel a debt.
pub fn unix_from_iso8601(s: &str) -> Option<i64> {
    let s = s.strip_suffix('Z')?;
    let (date, time) = s.split_once('T')?;
    let mut d = date.split('-');
    let (y, mo, da) = (d.next()?, d.next()?, d.next()?);
    if d.next().is_some() || y.len() != 4 || mo.len() != 2 || da.len() != 2 {
        return None;
    }
    let mut t = time.split(':');
    let (h, mi, se) = (t.next()?, t.next()?, t.next()?);
    if t.next().is_some() || h.len() != 2 || mi.len() != 2 || se.len() != 2 {
        return None;
    }
    let y: i64 = y.parse().ok()?;
    let mo: u32 = mo.parse().ok()?;
    let da: u32 = da.parse().ok()?;
    let h: i64 = h.parse().ok()?;
    let mi: i64 = mi.parse().ok()?;
    let se: i64 = se.parse().ok()?;
    if !(1..=12).contains(&mo) || !(1..=31).contains(&da) || h > 23 || mi > 59 || se > 59 {
        return None;
    }
    Some(days_from_civil(y, mo, da) * 86400 + h * 3600 + mi * 60 + se)
}

/// The current instant as Unix seconds - the one impure clock read this
/// module makes, and the shared root both `now_iso8601` below and the
/// evaluation debt's own "how long since" comparison
/// (`usefulness::eval_debt_owed`) build on, so a hook run and the string it
/// might format from `now_iso8601` in the same breath can never read two
/// different instants. Falls back to the Unix epoch on a clock that reports
/// before it (never observed in practice; kept so this can never panic on a
/// system clock oddity - a read/serve-path helper must not be the thing that
/// turns a fail-open path loud).
pub fn now_unix() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

/// The current instant, as `served_at` is recorded.
pub fn now_iso8601() -> String {
    iso8601_from_unix(now_unix())
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

    #[test]
    fn now_unix_and_now_iso8601_agree() {
        // `now_iso8601` is defined as `iso8601_from_unix(now_unix())`, so a
        // fresh `now_unix()` bracketed around one `now_iso8601()` call, then
        // parsed back with `unix_from_iso8601`, must land inside that
        // bracket - proving the two report the same instant rather than two
        // independent clock reads that merely look similar.
        let before = now_unix();
        let parsed_back = unix_from_iso8601(&now_iso8601()).expect("now_iso8601's own shape must parse");
        let after = now_unix();
        assert!(before <= parsed_back && parsed_back <= after, "before={before} parsed_back={parsed_back} after={after}");
    }

    #[test]
    fn parsing_known_timestamps_round_trips_formatting() {
        for secs in [0, 1_700_000_000, 1_893_456_000, 1_754_140_800, 157_680_000] {
            let stamp = iso8601_from_unix(secs);
            assert_eq!(unix_from_iso8601(&stamp), Some(secs), "round trip through {stamp}");
        }
    }

    #[test]
    fn parsing_rejects_anything_not_in_the_exact_written_shape() {
        assert_eq!(unix_from_iso8601(""), None);
        assert_eq!(unix_from_iso8601("2026-09-08T00:00:00"), None, "missing the trailing Z");
        assert_eq!(unix_from_iso8601("2026-09-08 00:00:00Z"), None, "space instead of T");
        assert_eq!(unix_from_iso8601("not-a-timestamp-at-all"), None);
        assert_eq!(unix_from_iso8601("2026-09-08T00:00:00.000Z"), None, "fractional seconds are a different shape");
        assert_eq!(unix_from_iso8601("2026-13-01T00:00:00Z"), None, "month 13 does not exist");
        assert_eq!(unix_from_iso8601("2026-09-08T24:00:00Z"), None, "hour 24 does not exist");
    }
}
