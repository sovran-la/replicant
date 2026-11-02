//! Timestamps without a date crate.
//!
//! The journal writes UTC (`2026-10-31T19:04:11Z`). PLAN.md's example lines
//! show local time with an offset; std has no time zone database, and the
//! journal is read by machines in different zones anyway, so UTC is the one
//! unambiguous choice. `replicant log` prints what is stored.

/// Days since 1970-01-01 → (year, month, day). Howard Hinnant's algorithm, as
/// the agents crate's certs.rs uses it.
pub fn civil_from_days(days: i64) -> (i32, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    ((yoe + era * 400 + i64::from(m <= 2)) as i32, m, d)
}

/// (year, month, day) → days since 1970-01-01.
pub fn days_from_civil(y: i32, m: u32, d: u32) -> i64 {
    let y = i64::from(if m <= 2 { y - 1 } else { y });
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let m = i64::from(m);
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + i64::from(d) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// `2026-10-31T19:04:11Z`.
pub fn iso(secs: u64) -> String {
    let s = secs as i64;
    let (y, mo, d) = civil_from_days(s.div_euclid(86_400));
    let t = s.rem_euclid(86_400);
    format!(
        "{y:04}-{mo:02}-{d:02}T{:02}:{:02}:{:02}Z",
        t / 3600,
        t % 3600 / 60,
        t % 60
    )
}

/// Parse what [`iso`] writes, or a bare `YYYY-MM-DD` (midnight UTC).
pub fn parse_iso(s: &str) -> Option<u64> {
    let (date, time) = match s.split_once('T') {
        Some((d, t)) => (d, Some(t.strip_suffix('Z')?)),
        None => (s, None),
    };
    let mut dp = date.splitn(3, '-').map(|p| p.parse::<u32>().ok());
    let (y, m, d) = (dp.next()??, dp.next()??, dp.next()??);
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return None;
    }
    let mut secs = days_from_civil(y as i32, m, d) * 86_400;
    if let Some(t) = time {
        let mut tp = t.splitn(3, ':').map(|p| p.parse::<i64>().ok());
        let (h, mi, se) = (tp.next()??, tp.next()??, tp.next()??);
        if h > 23 || mi > 59 || se > 60 {
            return None;
        }
        secs += h * 3600 + mi * 60 + se;
    }
    u64::try_from(secs).ok()
}

/// `20261031T190411Z`: [`iso`] without separators, for a directory name.
pub fn compact(secs: u64) -> String {
    iso(secs).replace(['-', ':'], "")
}

/// Parse what [`compact`] writes (and nothing looser).
pub fn parse_compact(s: &str) -> Option<u64> {
    let b = s.as_bytes();
    if b.len() != 16 || b[8] != b'T' || b[15] != b'Z' {
        return None;
    }
    let digits = |r: std::ops::Range<usize>| s[r].bytes().all(|c| c.is_ascii_digit());
    if !digits(0..8) || !digits(9..15) {
        return None;
    }
    parse_iso(&format!(
        "{}-{}-{}T{}:{}:{}Z",
        &s[0..4],
        &s[4..6],
        &s[6..8],
        &s[9..11],
        &s[11..13],
        &s[13..15]
    ))
}

/// `--since`: an ISO timestamp, a date, or a relative age (`45s`, `90m`, `2h`,
/// `3d`) counted back from `now`.
pub fn parse_since(s: &str, now: u64) -> Option<u64> {
    if let Some(t) = parse_iso(s) {
        return Some(t);
    }
    let (n, unit) = s.split_at(s.len().checked_sub(1)?);
    let n: u64 = n.parse().ok()?;
    let mult = match unit {
        "s" => 1,
        "m" => 60,
        "h" => 3600,
        "d" => 86_400,
        _ => return None,
    };
    Some(now.saturating_sub(n.saturating_mul(mult)))
}

/// `40s`, `2m`, `5h`, `9d`: how long ago, coarsely.
pub fn ago(secs: u64) -> String {
    match secs {
        0..60 => format!("{secs}s"),
        60..3600 => format!("{}m", secs / 60),
        3600..86_400 => format!("{}h", secs / 3600),
        _ => format!("{}d", secs / 86_400),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compact_round_trips_and_refuses_anything_else() {
        assert_eq!(compact(0), "19700101T000000Z");
        for t in [0, 1_793_592_042, 4_102_444_799] {
            assert_eq!(parse_compact(&compact(t)), Some(t));
        }
        for bad in [
            "",
            "19700101T000000",
            "1970-01-01T00:00:00Z",
            "19701301T000000Z",
            "1970010aT000000Z",
            "19700101T000000Z-2",
            "../../etc",
        ] {
            assert_eq!(parse_compact(bad), None, "{bad}");
        }
    }

    #[test]
    fn iso_round_trip_and_known_values() {
        assert_eq!(iso(0), "1970-01-01T00:00:00Z");
        assert_eq!(iso(1_793_473_451), "2026-10-31T19:04:11Z");
        for t in [0, 951_782_400, 1_793_473_451, 4_102_444_800] {
            assert_eq!(parse_iso(&iso(t)), Some(t));
        }
        assert_eq!(
            parse_iso("2000-02-29T00:00:00Z"),
            Some(951_782_400),
            "leap day"
        );
    }

    #[test]
    fn parse_iso_dates_and_garbage() {
        assert_eq!(parse_iso("2026-10-31"), Some(1_793_404_800));
        for bad in [
            "",
            "2026-13-01",
            "2026-10-32",
            "2026-10-31T25:00:00Z",
            "2026-10-31T10:00:00",
            "x",
        ] {
            assert_eq!(parse_iso(bad), None, "{bad}");
        }
    }

    #[test]
    fn since_relative_and_absolute() {
        let now = 1_000_000;
        assert_eq!(parse_since("45s", now), Some(now - 45));
        assert_eq!(parse_since("90m", now), Some(now - 5400));
        assert_eq!(parse_since("2h", now), Some(now - 7200));
        assert_eq!(parse_since("3d", now), Some(now - 259_200));
        assert_eq!(parse_since("1970-01-02", now), Some(86_400));
        assert_eq!(parse_since("99999d", now), Some(0), "saturates");
        assert_eq!(parse_since("3w", now), None);
        assert_eq!(parse_since("", now), None);
    }

    #[test]
    fn ago_buckets() {
        assert_eq!(ago(40), "40s");
        assert_eq!(ago(120), "2m");
        assert_eq!(ago(5 * 3600), "5h");
        assert_eq!(ago(9 * 86_400), "9d");
    }
}
