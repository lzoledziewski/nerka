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

use std::borrow::Cow;
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

/// Read a time, as logs write them:
///
/// - a date: `2025-10-17`, `2025/10/17`, `2025.10.17`, `17.10.2025`,
///   `20251017`, or with the month's name (`17 Oct 2025`, `Oct 17, 2025`,
///   `17-Oct-2025`, Apache's `17/Oct/2025`, a weekday first: `Fri, 17 Oct
///   2025`; `Fri Oct 17 10:00:01 2025` with the year last);
/// - then (after `T`, a space, or `:` as Apache writes it) or alone, a
///   time of day: `10:00:01`, `9:05:01`, `10:00`, `100001` (after a
///   compact date), seconds with a fraction after `.` or `,`
///   (`10:00:01,250`), `AM` / `PM`, and a zone: `Z`, `UTC`, `GMT`,
///   `+02:00`, `-0200`, `+02`;
/// - or a plain number: seconds since the Unix epoch, or milliseconds,
///   microseconds, nanoseconds when it is too large for seconds (an epoch
///   stamp in ms is ~1.7e12).
///
/// A date-time without a zone is UTC, or local time if [`is_local`]; a time
/// of day alone stays a time of day. Not read (ambiguous): a date without a
/// year (`Oct 17 10:00:01`), and slashed dates with the year last
/// (`10/17/2025` or `17/10/2025`).
pub fn parse_time(s: &str) -> Option<f64> {
    parse_in(s, &|t| if is_local() { local_to_utc(t) } else { t })
}

/// [`parse_time`], with `zoneless` turning a date-time without a zone
/// (seconds as if UTC) into the instant.
fn parse_in(s: &str, zoneless: &dyn Fn(f64) -> f64) -> Option<f64> {
    let s = s.trim();
    // Times are ASCII, and the readers below cut by byte position (fuzz,
    // 2026-11-22: `2026-10-1€…` panicked once every text line's start was
    // read).
    if !s.is_ascii() {
        return None;
    }
    if let Ok(n) = s.parse::<f64>() {
        return n.is_finite().then(|| epoch_seconds(n));
    }
    let Some((day, rest, year_after)) = date(s) else {
        // A time of day.
        let (t, _) = clock(s, false)?;
        return Some(t);
    };
    let compact = s.as_bytes().get(..8).is_some_and(|b| b.iter().all(u8::is_ascii_digit));
    let rest = rest.trim_start_matches(['T', 't', ' ', ':']);
    if rest.is_empty() {
        return Some(zoneless(day as f64 * 86_400.0));
    }
    // `Fri Oct 17 10:00:01 2025`: the year after the time.
    let (rest, day) = match year_after {
        Some((m, d)) => {
            let (clock_part, year) = rest.trim_end().rsplit_once(' ')?;
            (clock_part, days_from_civil(num(year).filter(|_| year.len() == 4)?, m, d))
        }
        None => (rest, day),
    };
    let (t, zone) = clock(rest, compact)?;
    Some(match zone {
        Some(zone) => day as f64 * 86_400.0 + t - zone,
        None => zoneless(day as f64 * 86_400.0 + t),
    })
}

/// The zone of a file's times written without one (a profile's `zone`).
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub enum Zone {
    /// As the process shows times ([`is_local`]: the GUI's Local time /
    /// UTC switch, the CLI's `--local-time`).
    #[default]
    Shown,
    Utc,
    /// This computer's zone, whatever the switch says.
    Local,
    /// A fixed offset ahead of UTC, in seconds (`+02:00`).
    Fixed(f64),
}

impl Zone {
    /// `utc`, `local`, `shown`, or an offset: `+02:00`, `-0530`, `+2`.
    pub fn parse(s: &str) -> Option<Zone> {
        let s = s.trim();
        Some(match s.to_ascii_lowercase().as_str() {
            "utc" | "z" | "gmt" => Zone::Utc,
            "local" => Zone::Local,
            "shown" => Zone::Shown,
            _ => {
                let sign = match s.as_bytes().first()? {
                    b'+' => 1.0,
                    b'-' => -1.0,
                    _ => return None,
                };
                let z = &s[1..];
                let (h, m) = match z.split_once(':') {
                    Some((h, m)) => (num(h)?, num(m)?),
                    None if z.len() == 4 => (num(&z[..2])?, num(&z[2..])?),
                    None => (num(z)?, 0),
                };
                if h > 14 || m > 59 {
                    return None;
                }
                Zone::Fixed(sign * (h * 3600 + m * 60) as f64)
            }
        })
    }

    /// As [`Self::parse`] reads it back.
    pub fn spec(self) -> String {
        match self {
            Zone::Shown => "shown".into(),
            Zone::Utc => "utc".into(),
            Zone::Local => "local".into(),
            Zone::Fixed(off) => {
                let (sign, a) = if off < 0.0 { ('-', -off) } else { ('+', off) };
                let a = a as i64;
                format!("{sign}{:02}:{:02}", a / 3600, a % 3600 / 60)
            }
        }
    }

    /// The instant of a wall time `t` in this zone (seconds as if UTC).
    pub fn to_utc(self, t: f64) -> f64 {
        match self {
            Zone::Shown if is_local() => local_to_utc(t),
            Zone::Shown | Zone::Utc => t,
            Zone::Local => local_to_utc(t),
            Zone::Fixed(off) => t - off,
        }
    }
}

/// What a plain number in a file's times counts (a profile's `numbers`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Numbers {
    /// Seconds since 1970, or ms / µs / ns when too large for seconds.
    #[default]
    Auto,
    S,
    Ms,
    Us,
    Ns,
}

impl Numbers {
    pub fn parse(s: &str) -> Option<Numbers> {
        Some(match s.trim().to_ascii_lowercase().as_str() {
            "auto" => Numbers::Auto,
            "s" => Numbers::S,
            "ms" => Numbers::Ms,
            "us" | "µs" => Numbers::Us,
            "ns" => Numbers::Ns,
            _ => return None,
        })
    }

    pub fn spec(self) -> &'static str {
        match self {
            Numbers::Auto => "auto",
            Numbers::S => "s",
            Numbers::Ms => "ms",
            Numbers::Us => "us",
            Numbers::Ns => "ns",
        }
    }

    fn seconds(self, n: f64) -> f64 {
        match self {
            Numbers::Auto => epoch_seconds(n),
            Numbers::S => n,
            Numbers::Ms => n / 1e3,
            Numbers::Us => n / 1e6,
            Numbers::Ns => n / 1e9,
        }
    }
}

/// [`parse_time`] for a file's times: a date-time without a zone in
/// `zone`, a plain number counted as `numbers` says.
pub fn parse_time_as(s: &str, zone: Zone, numbers: Numbers) -> Option<f64> {
    let t = s.trim();
    if let Ok(n) = t.parse::<f64>() {
        return n.is_finite().then(|| numbers.seconds(n));
    }
    parse_in(t, &|t| zone.to_utc(t))
}

/// Bytes of a line's start that [`leading_time`] looks at.
const LEADING_BYTES: usize = 64;

/// The time a line starts with, as most text logs write it: a date-time
/// (or a time of day) before the message, perhaps in brackets
/// (`2025-10-17 10:00:01.250 ERROR …`, `[2025-10-17T10:00:01Z] …`, `Oct 17
/// 10:00:01 host …`). The longest run of its first five words that
/// [`parse_time_as`] reads, trailing `,:;|-` aside. A plain number isn't
/// taken (`12 files copied`): a profile's own rule can read numbers. A
/// logfmt line's first key (`time=2025-10-17T10:00:01.25Z level=INFO …`,
/// Go's slog; also `ts=`, `t=`, `timestamp=`, quoted or not) is its time.
pub fn leading_time(line: &str, zone: Zone) -> Option<f64> {
    let mut end = line.len().min(LEADING_BYTES);
    while !line.is_char_boundary(end) {
        end -= 1;
    }
    let head = line[..end].trim_start();
    if let Some(value) = ["time=", "ts=", "t=", "timestamp="].iter().find_map(|k| head.strip_prefix(k)) {
        let value = match value.strip_prefix('"') {
            Some(q) => &q[..q.find('"')?],
            None => value.split_whitespace().next()?,
        };
        return (value.parse::<f64>().is_err()).then(|| parse_time_as(value, zone, Numbers::Auto)).flatten();
    }
    // In brackets: what they hold.
    if let Some(rest) = head.strip_prefix(['[', '(']) {
        let close = rest.find([']', ')'])?;
        let inner = rest[..close].trim();
        return (inner.parse::<f64>().is_err()).then(|| parse_time_as(inner, zone, Numbers::Auto)).flatten();
    }
    let mut best = None;
    let mut words = 0;
    let mut in_word = false;
    for (i, c) in head.char_indices().chain(std::iter::once((head.len(), ' '))) {
        let space = c.is_whitespace();
        if space && in_word {
            words += 1;
            let cand = head[..i].trim_end_matches([',', ':', ';', '|', '-', ']']);
            if cand.parse::<f64>().is_err()
                && let Some(t) = parse_time_as(cand, zone, Numbers::Auto)
            {
                best = Some(t);
            }
            if words == 5 {
                break;
            }
        }
        in_word = !space;
    }
    best
}

/// A signed amount of time: `+3h`, `-0.250s`, `90`, `-1.5m`, `500ms`
/// ([`crate::lifecycle::parse_duration`] with a sign).
pub fn parse_offset(s: &str) -> Option<f64> {
    let s = s.trim();
    let (sign, rest) = match s.as_bytes().first()? {
        b'-' => (-1.0, &s[1..]),
        b'+' => (1.0, &s[1..]),
        _ => (1.0, s),
    };
    crate::lifecycle::parse_duration(rest).map(|d| sign * d)
}

/// An offset as [`parse_offset`] reads it back: `+3h`, `-0.25s`, `+0s`.
pub fn format_offset(d: f64) -> String {
    let (sign, a) = if d < 0.0 { ('-', -d) } else { ('+', d) };
    let round = |x: f64| (x * 1e9).round() / 1e9;
    if a >= 3600.0 && (a / 3600.0).fract() == 0.0 {
        format!("{sign}{}h", a / 3600.0)
    } else if a >= 60.0 && (a / 60.0).fract() == 0.0 {
        format!("{sign}{}m", a / 60.0)
    } else {
        format!("{sign}{}s", round(a))
    }
}

/// A plain number as seconds: one too large for seconds (after the year
/// 5000) is milliseconds, microseconds or nanoseconds since the epoch.
fn epoch_seconds(n: f64) -> f64 {
    match n.abs() {
        a if a >= 1e17 => n / 1e9,
        a if a >= 1e14 => n / 1e6,
        a if a >= 1e11 => n / 1e3,
        _ => n,
    }
}

/// A weekday's name (`Fri`, `friday`) before a date.
fn weekday(s: &str) -> bool {
    const NAMES: [&str; 7] = ["mon", "tue", "wed", "thu", "fri", "sat", "sun"];
    s.len() >= 3 && s.bytes().all(|c| c.is_ascii_alphabetic()) && NAMES.iter().any(|n| n.eq_ignore_ascii_case(&s[..3]))
}

/// The year it is now (UTC), for dates written without one.
fn this_year() -> i64 {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
    civil_from_days((now / 86_400) as i64).0
}

/// A month's name (its first three letters, any case) as 1–12.
fn month(s: &str) -> Option<i64> {
    const NAMES: [&str; 12] = ["jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec"];
    let s = s.get(..3)?;
    NAMES.iter().position(|n| n.eq_ignore_ascii_case(s)).map(|i| i as i64 + 1)
}

/// The date at the start of `s`: its day (since the epoch) and what follows
/// it; for `Oct 17 10:00:01 2025` (the year after the time) the month and
/// day instead, the caller reading the year.
#[allow(clippy::type_complexity)]
fn date(s: &str) -> Option<(i64, &str, Option<(i64, i64)>)> {
    let ymd =
        |y: i64, m: i64, d: i64| ((1..=12).contains(&m) && (1..=31).contains(&d)).then(|| days_from_civil(y, m, d));
    let b = s.as_bytes();
    // 2025-10-17, 2025/10/17, 2025.10.17
    if b.len() >= 10
        && b[4] == b[7]
        && matches!(b[4], b'-' | b'/' | b'.')
        && let (Some(y), Some(m), Some(d)) = (num(&s[..4]), num(&s[5..7]), num(&s[8..10]))
    {
        return Some((ymd(y, m, d)?, &s[10..], None));
    }
    // 17.10.2025
    if let [d, m, rest] = s.splitn(3, '.').collect::<Vec<_>>()[..]
        && d.len() <= 2
        && m.len() <= 2
        && rest.len() >= 4
        && let (Some(d), Some(m), Some(y)) = (num(d), num(m), num(&rest[..4]))
    {
        return Some((ymd(y, m, d)?, &rest[4..], None));
    }
    // 20251017, then T100001 or nothing
    if b.len() >= 8 && b[..8].iter().all(u8::is_ascii_digit) && b.get(8).is_none_or(|c| matches!(c, b'T' | b't' | b' '))
    {
        return Some((ymd(num(&s[..4])?, num(&s[4..6])?, num(&s[6..8])?)?, &s[8..], None));
    }
    // Names of months, a weekday first or not: `Fri, 17 Oct 2025`,
    // `Oct 17, 2025`, `17-Oct-2025`, `17/Oct/2025`, `Fri Oct 17 10:00:01 2025`.
    let words: Vec<&str> = s.split([' ', '-', '/', ',']).filter(|w| !w.is_empty()).collect();
    let skip = usize::from(words.first().is_some_and(|w| weekday(w)));
    let w = words.get(skip..)?;
    let after = |n: usize| -> &str {
        // What follows the first `n` words (after the weekday) in `s`.
        let mut rest = s;
        for word in words.iter().take(skip + n) {
            let at = rest.find(word).map_or(rest.len(), |i| i + word.len());
            rest = &rest[at..];
        }
        rest.trim_start_matches([',', ' ', '-', '/'])
    };
    let year = |w: &str| num(w.get(..4)?).filter(|_| w.len() == 4 || w.as_bytes()[4] == b':');
    match w {
        [d, m, y, ..] if num(d).is_some() && month(m).is_some() && year(y).is_some() => {
            Some((ymd(year(y)?, month(m)?, num(d)?)?, after(2).get(4..)?, None))
        }
        [m, d, y, ..] if month(m).is_some() && num(d).is_some() && year(y).is_some() => {
            Some((ymd(year(y)?, month(m)?, num(d)?)?, after(2).get(4..)?, None))
        }
        [m, d, _, y] if month(m).is_some() && num(d).is_some() && year(y).is_some() => {
            let (m, d) = (month(m)?, num(d)?);
            Some((0, after(2), Some((m, d))))
        }
        // No year (BSD syslog: `Oct 17 10:00:01`): this year (owner, 2026-11-02).
        [m, d, ..] if month(m).is_some() && num(d).is_some() => {
            Some((ymd(this_year(), month(m)?, num(d)?)?, after(2), None))
        }
        [d, m, ..] if num(d).is_some() && month(m).is_some() => {
            Some((ymd(this_year(), month(m)?, num(d)?)?, after(2), None))
        }
        _ => None,
    }
}

/// A time of day at the start of `s` (the rest is its zone): seconds into
/// the day, and the zone's offset if it has one. `compact`: `HHMMSS` too.
fn clock(s: &str, compact: bool) -> Option<(f64, Option<f64>)> {
    let s = s.trim();
    // A zone written as a word, the 12-hour clock: only a value that ends
    // in a letter has them (a value per event: keep the common case short).
    let (mut s, mut word_zone, mut pm) = (s, None, None);
    if s.as_bytes().last().is_some_and(u8::is_ascii_alphabetic) {
        if let Some((c, z)) = s.rsplit_once(' ')
            && (z.eq_ignore_ascii_case("UTC") || z.eq_ignore_ascii_case("GMT"))
        {
            (s, word_zone) = (c.trim_end(), Some(0.0));
        }
        match s.len().checked_sub(2).and_then(|i| s.get(i..)) {
            Some(t) if t.eq_ignore_ascii_case("PM") => (s, pm) = (s[..s.len() - 2].trim_end(), Some(true)),
            Some(t) if t.eq_ignore_ascii_case("AM") => (s, pm) = (s[..s.len() - 2].trim_end(), Some(false)),
            _ => {}
        }
    }
    let (clock, zone) = split_zone(s.trim_end())?;
    let zone = zone.or(word_zone);
    let clock = clock.trim_end();
    let mut parts = clock.split(':');
    let (h, m, sec) = match (parts.next(), parts.next(), parts.next(), parts.next()) {
        (Some(h), Some(m), sec, None) => (h, m, sec),
        (Some(c), None, None, None) if compact && c.len() >= 6 => (&c[..2], &c[2..4], Some(&c[4..])),
        _ => return None,
    };
    let (mut h, m) = (num(h).filter(|_| h.len() <= 2)?, num(m).filter(|_| m.len() == 2)?);
    // Seconds: two digits, a fraction after `.` or `,` (log4j).
    if sec.is_some_and(|s| s.split(['.', ',']).next().map(str::len) != Some(2)) || m > 59 {
        return None;
    }
    let sec = match sec {
        None => Cow::Borrowed("0"),
        Some(s) if s.contains(',') => Cow::Owned(s.replace(',', ".")),
        Some(s) => Cow::Borrowed(s),
    };
    match pm {
        Some(pm) if (1..=12).contains(&h) => h = h % 12 + if pm { 12 } else { 0 },
        Some(_) => return None,
        None if h > 23 => return None,
        None => {}
    }
    let secs: f64 = sec.parse().ok().filter(|v: &f64| (0.0..61.0).contains(v))?;
    Some(((h * 3600 + m * 60) as f64 + secs, zone))
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

/// [`format_time`] in UTC whatever [`is_local`] says (files for scripts).
pub fn format_utc(t: f64, digits: usize) -> String {
    format_shown(t, digits)
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
        for bad in ["", "25:00:00", "10:61:00", "10:00:1", "10:00:0", "2025-13-01", "2025-10-17X10:00:00", "now"] {
            assert_eq!(parse_time(bad), None, "{bad}");
        }
    }

    /// The forms logs write besides ISO: each the same instant.
    #[test]
    fn parses_the_forms_logs_write() {
        let at = 20_378.0 * 86_400.0 + 36_001.25; // 2025-10-17 10:00:01.25 UTC
        for s in [
            "2025-10-17 10:00:01,250",
            "2025.10.17 10:00:01.250",
            "17.10.2025 10:00:01.25",
            "20251017T100001.25",
            "20251017 100001,25",
            "17 Oct 2025 10:00:01.25",
            "Fri, 17 Oct 2025 12:00:01.25 +0200",
            "17-Oct-2025 10:00:01.25 UTC",
            "17/Oct/2025:12:00:01.25 +0200",
            "Oct 17, 2025 10:00:01.25 AM",
            "october 17 2025 10:00:01.25 GMT",
            "Fri Oct 17 10:00:01.25 2025",
            "2025-10-17T05:00:01.25-05",
        ] {
            assert_eq!(parse_time(s), Some(at), "{s}");
        }
        assert_eq!(parse_time("Oct 17, 2025 10:00:01 PM"), Some(at - 0.25 + 12.0 * 3600.0));
        assert_eq!(parse_time("12:30 AM"), Some(30.0 * 60.0));
        // Times of day.
        assert_eq!(parse_time("9:05:01"), Some(32_701.0));
        assert_eq!(parse_time("10:00"), Some(36_000.0));
        assert_eq!(parse_time("10:00:01,5"), Some(36_001.5));
        // Epoch stamps in ms, µs and ns.
        // (A nanosecond stamp is as exact as an f64 of it: ~0.2 µs here.)
        for s in ["1760695201250", "1760695201250000", "1760695201250000000"] {
            assert!((parse_time(s).unwrap() - 1_760_695_201.25).abs() < 1e-6, "{s}");
        }
        // No year: this year's (BSD syslog).
        let this = days_from_civil(this_year(), 10, 17) as f64 * 86_400.0 + 36_001.0;
        assert_eq!(parse_time("Oct 17 10:00:01"), Some(this));
        assert_eq!(parse_time("Fri Oct 17 12:00:01 +0200"), Some(this));
        assert_eq!(parse_time("17 Oct 10:00:01"), Some(this));
        // Ambiguous or broken: not read.
        for bad in [
            "Oct 17 25:00:00",
            "ERROR Oct 17 10:00:01",
            "10/17/2025 10:00:01",
            "17/10/2025",
            "32 Oct 2025",
            "17 Foo 2025",
            "13:00 PM",
            "ERROR 10:00:01",
        ] {
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
    fn leading_times_as_logs_write_them() {
        let at = 20_378.0 * 86_400.0 + 36_001.25; // 2025-10-17 10:00:01.25 UTC
        for line in [
            "2025-10-17 10:00:01.250 ERROR failed",
            "2025-10-17T10:00:01.25Z [main] INFO x",
            "  2025-10-17 10:00:01,250 | WARN | y",
            "[2025-10-17 10:00:01.250] [info] z",
            "(2025-10-17T12:00:01.25+02:00) z",
            "2025-10-17 12:00:01.25 +0200 host app: z",
            "Fri Oct 17 10:00:01.25 2025 a b c",
            "17/Oct/2025:10:00:01.25 +0000 GET /",
            "2025-10-17 10:00:01.25: message",
        ] {
            assert_eq!(leading_time(line, Zone::Utc), Some(at), "{line}");
        }
        assert_eq!(leading_time("10:00:01.25 INFO x", Zone::Utc), Some(36_001.25));
        // logfmt (Go's slog): the time key first.
        for line in [
            "time=2025-10-17T10:00:01.25Z level=INFO msg=x",
            "time=2025-10-17T03:00:01.250-07:00 level=INFO source=routes.go:1742 msg=\"server config\"",
            "ts=\"2025-10-17 10:00:01.25\" level=warn",
            "t=2025-10-17T10:00:01.25Z",
        ] {
            assert_eq!(leading_time(line, Zone::Utc), Some(at), "{line}");
        }
        for no in
            ["time=12 level=x", "timeout=2025-10-17 x", "time=\"2025-10-17", "level=INFO time=2025-10-17T10:00:01Z"]
        {
            assert_eq!(leading_time(no, Zone::Utc), None, "{no}");
        }
        // A zone for times written without one; a written zone wins.
        assert_eq!(leading_time("2025-10-17 12:00:01.25 x", Zone::Fixed(7200.0)), Some(at));
        assert_eq!(leading_time("2025-10-17T10:00:01.25Z x", Zone::Fixed(7200.0)), Some(at));
        for no in ["12 files copied", "[   12.345678] usb", "ERROR 2025-10-17 10:00:01", "", "   ", "[INFO] 10:00"] {
            assert_eq!(leading_time(no, Zone::Utc), None, "{no}");
        }
        // Not ASCII where a date or a zone is cut: no time, no panic.
        for s in ["2026-10-1€ 00:00:00", "2026-10-17 0€:00", "17.10.20€5 10:00", "10:00:00+0€", "20251017T10€001"]
        {
            assert_eq!(parse_time(s), None, "{s}");
        }
        assert_eq!(leading_time("2026-10-1€ 00:00:00", Zone::Utc), None);
        // The date before the cut word still is one.
        assert_eq!(leading_time("2025-10-17 1€:00 x", Zone::Utc), Some(20_378.0 * 86_400.0));
        assert_eq!(leading_time("2025-10-17 10:00:01.25 żółw", Zone::Utc), Some(at));
        // Only the line's start is read.
        let long = format!("{} 2025-10-17", "x".repeat(100));
        assert_eq!(leading_time(&long, Zone::Utc), None);
        assert_eq!(leading_time("żółw 10:00", Zone::Utc), None);
    }

    #[test]
    fn zones_numbers_and_offsets_read_back() {
        for (s, z) in [
            ("utc", Zone::Utc),
            ("local", Zone::Local),
            ("shown", Zone::Shown),
            ("+02:00", Zone::Fixed(7200.0)),
            ("-05:30", Zone::Fixed(-19_800.0)),
        ] {
            assert_eq!(Zone::parse(s), Some(z), "{s}");
            assert_eq!(z.spec(), s);
        }
        assert_eq!(Zone::parse("+0200"), Some(Zone::Fixed(7200.0)));
        assert_eq!(Zone::parse("+2"), Some(Zone::Fixed(7200.0)));
        assert_eq!(Zone::parse("UTC"), Some(Zone::Utc));
        for bad in ["", "mars", "+25", "02:00", "+02:60"] {
            assert_eq!(Zone::parse(bad), None, "{bad}");
        }
        for n in ["auto", "s", "ms", "us", "ns"] {
            assert_eq!(Numbers::parse(n).unwrap().spec(), n);
        }
        assert_eq!(parse_time_as("12.5", Zone::Utc, Numbers::S), Some(12.5));
        assert_eq!(parse_time_as("1500", Zone::Utc, Numbers::Ms), Some(1.5));
        assert_eq!(parse_time_as("1760695201250", Zone::Utc, Numbers::Auto), Some(1_760_695_201.25));
        for (s, d) in [("+3h", 10_800.0), ("-0.25s", -0.25), ("90", 90.0), ("-2m", -120.0), ("500ms", 0.5)] {
            assert_eq!(parse_offset(s), Some(d), "{s}");
        }
        assert_eq!(parse_offset("x"), None);
        assert_eq!(parse_offset(""), None);
        for d in [10_800.0, -0.25, 90.0, -120.0, 0.0, 1.0e-3, 86_400.0 * 365.0 * 55.0] {
            assert_eq!(parse_offset(&format_offset(d)), Some(d), "{d}: {}", format_offset(d));
        }
        assert_eq!(format_offset(10_800.0), "+3h");
        assert_eq!(format_offset(-0.25), "-0.25s");
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
