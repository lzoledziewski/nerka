//! Time values of signals: seconds since the Unix epoch (UTC) as `f64`,
//! which keeps sub-microsecond precision for today's dates.
//!
//! A time of day without a date (`10:00:01.5`) is seconds into day 0
//! (1970-01-01); a signal's events add a day when such a time goes back by
//! more than 12 hours (midnight), see [`TIME_OF_DAY_END`].
//!
//! **Zones.** Values are always UTC. What is shown and typed is UTC, or
//! this computer's local time after [`set_local`] (the GUI's setting): then
//! [`format_time`] and DLT's `time` column show local time, and a
//! date-time without a zone ([`parse_time`]: a capture, `@time >= "…"`) is
//! read as local time, so a plain-text log's times show as written either
//! way. A time of day is never converted. The offset at each instant comes
//! from the OS (its zone rules, past years' DST included).

use std::sync::atomic::{AtomicBool, Ordering};

/// Values below this are a time of day (day 0), not a date.
pub const TIME_OF_DAY_END: f64 = 86_400.0;

/// Values below this are a time of day or a few days after day 0
/// (midnights), shown without a date and never converted.
const DATES_FROM: f64 = 1000.0 * 86_400.0;

static LOCAL: AtomicBool = AtomicBool::new(false);

/// Show and read times in this computer's local time (`true`) or UTC.
pub fn set_local(on: bool) {
    LOCAL.store(on, Ordering::Relaxed);
}

/// Whether times are shown in local time ([`set_local`]).
pub fn is_local() -> bool {
    LOCAL.load(Ordering::Relaxed)
}

/// Seconds local time is ahead of UTC at the instant `t` (0 where the OS
/// doesn't say). Offsets change only on quarter hours: cached per quarter
/// hour and thread.
pub fn local_offset(t: f64) -> f64 {
    thread_local! {
        static LAST: std::cell::Cell<(i64, f64)> = const { std::cell::Cell::new((i64::MIN, 0.0)) };
    }
    let quarter = (t / 900.0).floor() as i64;
    LAST.with(|c| {
        let (q, off) = c.get();
        if q == quarter {
            return off;
        }
        let off = os::offset(quarter.saturating_mul(900)).unwrap_or(0) as f64;
        c.set((quarter, off));
        off
    })
}

/// The instant of the local wall time `t` (seconds as if it were UTC). In
/// the hour a DST change repeats, one of the two.
pub fn local_to_utc(t: f64) -> f64 {
    t - local_offset(t - local_offset(t))
}

/// `t` as it is shown: moved to local time if [`is_local`] (dates only).
pub fn shown(t: f64) -> f64 {
    if is_local() && t >= DATES_FROM { t + local_offset(t) } else { t }
}

/// Read a time: `YYYY-MM-DD[T ]HH:MM:SS[.frac][Z|±HH[:MM]]` (also with `/`
/// in the date), `HH:MM:SS[.frac]` (a time of day), or a plain number of
/// seconds. A date-time without a zone is UTC, or local time if
/// [`is_local`].
pub fn parse_time(s: &str) -> Option<f64> {
    parse_in(s, &|t| if is_local() { local_to_utc(t) } else { t })
}

/// [`parse_time`], with `zoneless` turning a date-time without a zone
/// (seconds as if UTC) into the instant.
fn parse_in(s: &str, zoneless: &dyn Fn(f64) -> f64) -> Option<f64> {
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
            None => return Some(zoneless(days_from_civil(y, m, d) as f64 * 86_400.0)),
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
    Some(match (day, zone) {
        (Some(d), Some(zone)) => d as f64 * 86_400.0 + t - zone,
        (Some(d), None) => zoneless(d as f64 * 86_400.0 + t),
        (None, _) => t,
    })
}

/// `"10:00:01+02:00"` → (`"10:00:01"`, 7200 s); no zone → `None`.
fn split_zone(s: &str) -> Option<(&str, Option<f64>)> {
    if let Some(c) = s.strip_suffix(['Z', 'z']) {
        return Some((c, Some(0.0)));
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
            Some((&s[..i], Some(sign * (h * 3600 + m * 60) as f64)))
        }
        None => Some((s, None)),
    }
}

fn num(s: &str) -> Option<i64> {
    (!s.is_empty() && s.bytes().all(|c| c.is_ascii_digit())).then(|| s.parse().ok()).flatten()
}

/// Days since 1970-01-01 (Howard Hinnant's algorithm).
pub(crate) fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

pub(crate) fn civil_from_days(z: i64) -> (i64, i64, i64) {
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

/// `2026-10-17 10:00:01.250000` (UTC, or local time if [`is_local`]); a
/// time of day (day 0, or a few days after it, from midnights) as
/// `10:00:01.250000` with `+1d` and so on. `digits` is the number of
/// decimals (0–9).
pub fn format_time(t: f64, digits: usize) -> String {
    format_shown(shown(t), digits)
}

/// [`format_time`] of a value already [`shown`].
fn format_shown(t: f64, digits: usize) -> String {
    let digits = digits.min(9);
    let scale = 10f64.powi(digits as i32);
    let total = (t * scale).round();
    let whole = (total / scale).floor();
    let frac = (total - whole * scale) as u64;
    let whole = whole as i64;
    let (day, sod) = (whole.div_euclid(86_400), whole.rem_euclid(86_400));
    let clock = format!("{:02}:{:02}:{:02}", sod / 3600, sod % 3600 / 60, sod % 60);
    let clock = if digits > 0 { format!("{clock}.{frac:0digits$}") } else { clock };
    if (0..1000).contains(&day) && t < DATES_FROM {
        return if day == 0 { clock } else { format!("{clock} +{day}d") };
    }
    let (y, m, d) = civil_from_days(day);
    format!("{y:04}-{m:02}-{d:02} {clock}")
}

/// This computer's UTC offset at an instant, from the OS.
#[cfg(all(unix, target_pointer_width = "64"))]
mod os {
    use std::ffi::{c_char, c_int};

    /// `struct tm` with the `tm_gmtoff` and `tm_zone` that glibc, musl and
    /// the BSDs (macOS) have.
    #[repr(C)]
    struct Tm {
        sec: c_int,
        min: c_int,
        hour: c_int,
        mday: c_int,
        mon: c_int,
        year: c_int,
        wday: c_int,
        yday: c_int,
        isdst: c_int,
        /// C's `long`: 64 bits on the 64-bit Unix this module is for.
        gmtoff: i64,
        zone: *const c_char,
    }

    unsafe extern "C" {
        fn tzset();
        fn localtime_r(t: *const i64, tm: *mut Tm) -> *mut Tm;
    }

    /// Seconds local time is ahead of UTC at `t` (seconds since the epoch).
    pub fn offset(t: i64) -> Option<i64> {
        static INIT: std::sync::Once = std::sync::Once::new();
        // SAFETY: tzset reads TZ and the zone files; once, before any use.
        INIT.call_once(|| unsafe { tzset() });
        let mut tm = std::mem::MaybeUninit::<Tm>::zeroed();
        // SAFETY: both pointers are valid for the call; localtime_r is
        // thread-safe and fills `tm` when it returns non-null.
        let ok = !unsafe { localtime_r(&t, tm.as_mut_ptr()) }.is_null();
        // SAFETY: zeroed is a valid Tm (a null `zone`); filled when `ok`.
        ok.then(|| unsafe { tm.assume_init() }.gmtoff)
    }
}

/// This computer's UTC offset at an instant, from the OS.
#[cfg(windows)]
mod os {
    use std::sync::OnceLock;

    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    struct SystemTime {
        year: u16,
        month: u16,
        day_of_week: u16,
        day: u16,
        hour: u16,
        minute: u16,
        second: u16,
        ms: u16,
    }

    /// `DYNAMIC_TIME_ZONE_INFORMATION`: the zone, with its rules per year.
    #[repr(C)]
    struct Zone {
        bias: i32,
        standard_name: [u16; 32],
        standard_date: SystemTime,
        standard_bias: i32,
        daylight_name: [u16; 32],
        daylight_date: SystemTime,
        daylight_bias: i32,
        key_name: [u16; 128],
        dynamic_daylight_time_disabled: u8,
    }

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetDynamicTimeZoneInformation(zone: *mut Zone) -> u32;
        fn SystemTimeToTzSpecificLocalTimeEx(zone: *const Zone, utc: *const SystemTime, local: *mut SystemTime) -> i32;
    }

    /// Seconds local time is ahead of UTC at `t` (seconds since the epoch).
    pub fn offset(t: i64) -> Option<i64> {
        static ZONE: OnceLock<Option<Zone>> = OnceLock::new();
        let zone = ZONE
            .get_or_init(|| {
                // SAFETY: all zeros is a valid Zone (numbers and arrays);
                // the call fills it.
                let mut z: Zone = unsafe { std::mem::zeroed() };
                // SAFETY: `z` is valid for the call.
                (unsafe { GetDynamicTimeZoneInformation(&mut z) } != u32::MAX).then_some(z)
            })
            .as_ref()?;
        let (days, sod) = (t.div_euclid(86_400), t.rem_euclid(86_400));
        let (y, m, d) = super::civil_from_days(days);
        if !(1601..=30_827).contains(&y) {
            return None;
        }
        let utc = SystemTime {
            year: y as u16,
            month: m as u16,
            day: d as u16,
            hour: (sod / 3600) as u16,
            minute: (sod / 60 % 60) as u16,
            second: (sod % 60) as u16,
            ..SystemTime::default()
        };
        let mut l = SystemTime::default();
        // SAFETY: the pointers are valid for the call.
        if unsafe { SystemTimeToTzSpecificLocalTimeEx(zone, &utc, &mut l) } == 0 {
            return None;
        }
        let local = super::days_from_civil(l.year.into(), l.month.into(), l.day.into()) * 86_400
            + i64::from(l.hour) * 3600
            + i64::from(l.minute) * 60
            + i64::from(l.second);
        Some(local - t)
    }
}

/// No local zone known: times stay UTC.
#[cfg(not(any(windows, all(unix, target_pointer_width = "64"))))]
mod os {
    pub fn offset(_t: i64) -> Option<i64> {
        None
    }
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

    #[test]
    fn zoneless_date_times_go_through_the_zone() {
        // A zone two hours ahead: local wall time minus two hours.
        let cest = |t: f64| t - 7200.0;
        let day = 20_378.0 * 86_400.0; // 2025-10-17
        assert_eq!(parse_in("2025-10-17 10:00:01", &cest), Some(day + 36_001.0 - 7200.0));
        assert_eq!(parse_in("2025-10-17", &cest), Some(day - 7200.0));
        // An explicit zone wins; a time of day is never converted.
        assert_eq!(parse_in("2025-10-17T10:00:01Z", &cest), Some(day + 36_001.0));
        assert_eq!(parse_in("10:00:01", &cest), Some(36_001.0));
    }

    #[test]
    fn local_offsets_from_the_os_are_whole_quarters() {
        // Whatever zone this machine is in (CI: usually UTC). The global
        // setting stays off in tests: it would change other tests' output.
        for t in [0.0, 951_782_400.0, 1_729_771_200.5, 1_761_350_400.0] {
            let off = local_offset(t);
            assert!(off.abs() <= 14.0 * 3600.0 && off % 900.0 == 0.0, "{t}: {off}");
            // Away from a DST change, local time maps back to the instant.
            assert_eq!(local_to_utc(t + off), t, "{t}");
        }
    }
}
