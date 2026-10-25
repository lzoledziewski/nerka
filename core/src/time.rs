//! Time values of signals: seconds since the Unix epoch (UTC) as `f64`,
//! which keeps sub-microsecond precision for today's dates.
//!
//! A time of day without a date (`10:00:01.5`) is seconds into day 0
//! (1970-01-01); a signal's events add a day when such a time goes back by
//! more than 12 hours (midnight), see [`TIME_OF_DAY_END`].

/// Values below this are a time of day (day 0), not a date.
pub const TIME_OF_DAY_END: f64 = 86_400.0;

/// Read a time: `YYYY-MM-DD[T ]HH:MM:SS[.frac][Z|±HH[:MM]]` (also with `/`
/// in the date), `HH:MM:SS[.frac]` (a time of day), or a plain number of
/// seconds. A date-time without a zone is UTC.
pub fn parse_time(s: &str) -> Option<f64> {
    let s = s.trim();
    if let Ok(n) = s.parse::<f64>() {
        return n.is_finite().then_some(n);
    }
    let b = s.as_bytes();
    let (day, rest) = if b.len() >= 10 && b[4] == b[7] && (b[4] == b'-' || b[4] == b'/') {
        let (y, m, d) = (num(&s[..4])?, num(&s[5..7])?, num(&s[8..10])?);
        if !(1..=12).contains(&m) || !(1..=31).contains(&d) {
            return None;
        }
        let rest = &s[10..];
        match rest.as_bytes().first() {
            None => return Some(days_from_civil(y, m, d) as f64 * 86_400.0),
            Some(b'T' | b't' | b' ') => (Some(days_from_civil(y, m, d)), &rest[1..]),
            _ => return None,
        }
    } else {
        (None, s)
    };
    let (clock, zone) = split_zone(rest)?;
    let mut it = clock.splitn(3, ':');
    let (h, m, sec) = (num(it.next()?)?, num(it.next()?)?, it.next()?);
    let sec_whole = sec.split('.').next()?;
    if sec_whole.len() != 2 || h > 23 || m > 59 {
        return None;
    }
    let secs: f64 = sec.parse().ok().filter(|v: &f64| (0.0..61.0).contains(v))?;
    let t = (h * 3600 + m * 60) as f64 + secs;
    Some(match day {
        Some(d) => d as f64 * 86_400.0 + t - zone,
        None => t,
    })
}

/// `"10:00:01+02:00"` → (`"10:00:01"`, 7200 s).
fn split_zone(s: &str) -> Option<(&str, f64)> {
    if let Some(c) = s.strip_suffix(['Z', 'z']) {
        return Some((c, 0.0));
    }
    match s.rfind(['+', '-']) {
        Some(i) => {
            let z = &s[i + 1..];
            let (h, m) = match z.split_once(':') {
                Some((h, m)) => (num(h)?, num(m)?),
                None if z.len() == 4 => (num(&z[..2])?, num(&z[2..])?),
                None => (num(z)?, 0),
            };
            let sign = if s.as_bytes()[i] == b'-' { -1.0 } else { 1.0 };
            Some((&s[..i], sign * (h * 3600 + m * 60) as f64))
        }
        None => Some((s, 0.0)),
    }
}

fn num(s: &str) -> Option<i64> {
    (!s.is_empty() && s.bytes().all(|c| c.is_ascii_digit())).then(|| s.parse().ok()).flatten()
}

/// Days since 1970-01-01 (Howard Hinnant's algorithm).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}

/// `2026-10-17 10:00:01.250000` (UTC); a time of day (day 0, or a few days
/// after it, from midnights) as `10:00:01.250000` with `+1d` and so on.
/// `digits` is the number of decimals (0–9).
pub fn format_time(t: f64, digits: usize) -> String {
    let digits = digits.min(9);
    let scale = 10f64.powi(digits as i32);
    let total = (t * scale).round();
    let whole = (total / scale).floor();
    let frac = (total - whole * scale) as u64;
    let whole = whole as i64;
    let (day, sod) = (whole.div_euclid(86_400), whole.rem_euclid(86_400));
    let clock = format!("{:02}:{:02}:{:02}", sod / 3600, sod % 3600 / 60, sod % 60);
    let clock = if digits > 0 { format!("{clock}.{frac:0digits$}") } else { clock };
    if (0..1000).contains(&day) && t < 1000.0 * 86_400.0 {
        return if day == 0 { clock } else { format!("{clock} +{day}d") };
    }
    let (y, m, d) = civil_from_days(day);
    format!("{y:04}-{m:02}-{d:02} {clock}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_common_forms() {
        let day = 20_378.0 * 86_400.0; // 2025-10-17
        assert_eq!(parse_time("2025-10-17"), Some(day));
        assert_eq!(parse_time("2025-10-17 10:00:01.5"), Some(day + 36_001.5));
        assert_eq!(parse_time("2025/10/17T10:00:01Z"), Some(day + 36_001.0));
        assert_eq!(parse_time("2025-10-17T12:00:01+02:00"), Some(day + 36_001.0));
        assert_eq!(parse_time("2025-10-17T08:00:01-0200"), Some(day + 36_001.0));
        assert_eq!(parse_time("10:00:01.250"), Some(36_001.25));
        assert_eq!(parse_time(" 1729771200.5 "), Some(1_729_771_200.5));
        for bad in ["", "10:00", "25:00:00", "10:61:00", "10:00:1", "2025-13-01", "2025-10-17X10:00:00", "now"] {
            assert_eq!(parse_time(bad), None, "{bad}");
        }
    }

    #[test]
    fn formats_dates_and_times_of_day() {
        assert_eq!(format_time(20_378.0 * 86_400.0 + 36_001.25, 3), "2025-10-17 10:00:01.250");
        assert_eq!(format_time(36_001.25, 6), "10:00:01.250000");
        assert_eq!(format_time(86_400.0 + 59.9996, 3), "00:01:00.000 +1d");
        assert_eq!(format_time(0.0, 0), "00:00:00");
        for t in [0.0, 951_782_400.0, 1_729_771_200.123] {
            assert!((parse_time(&format_time(t, 3)).unwrap() - t).abs() < 1e-6, "{t}");
        }
    }
}
