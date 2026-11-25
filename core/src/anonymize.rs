//! Logs made shareable (`nerka anonymize`; `docs/dev/patterns.md`, round
//! α3): every word replaced by a made-up one, so a trace can leave the
//! company for tuning or a bug report while its messages keep their
//! structure, and so their patterns.
//!
//! - A run of letters becomes letters of the same length and case, the
//!   same everywhere for the same word (any case), and two words never the
//!   same: `gear=D` and `gear=P` stay two values of one slot.
//! - A run holding a digit (numbers, hex, times, addresses) keeps its
//!   digits as digits and its letters as letters, made up per run: values
//!   stay values, without the original's.
//! - Everything else (spaces, punctuation) stays.
//!
//! The words are keyed by a secret (random per run unless a seed is
//! given): without it the mapping can't be undone or repeated. Words to
//! keep (`--keep`) stay as they are, and so do level names (`INFO`,
//! `ERROR`, upper case): the levels read from the text, and the patterns
//! (`lone-word` takes them for header), stay the same.

use crate::input::LogSource;
use crate::input::dlt::writer::{Arg, Msg, Value};
use crate::{LineNo, Progress, Result};
use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::io::Write;

/// Tries to find a made-up word no other word has, before giving up (two
/// words then share one: only possible for words of a letter or two).
const TRIES: u32 = 1000;

/// Replaces the words of texts, consistently ([`Anonymizer::text`]).
pub struct Anonymizer {
    key: u64,
    /// Kept as they are, lower case.
    keep: HashSet<String>,
    /// Word (lower case) → its made-up word (lower case), and back.
    words: HashMap<String, String>,
    taken: HashSet<String>,
}

/// What [`anonymize`] wrote.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Written {
    /// DLT, a message per line.
    Dlt,
    /// Text, a line per line.
    Text,
}

impl Anonymizer {
    /// `seed`: the same seed makes the same words (none: a random one, so
    /// nobody can redo the mapping); `keep`: words left as they are.
    pub fn new(seed: Option<&str>, keep: impl IntoIterator<Item = String>) -> Anonymizer {
        let mut h = std::hash::DefaultHasher::new();
        match seed {
            Some(s) => s.hash(&mut h),
            None => {
                let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
                (now.as_nanos(), std::process::id(), &h as *const _ as usize).hash(&mut h);
            }
        }
        Anonymizer {
            key: h.finish(),
            keep: keep.into_iter().map(|w| w.to_lowercase()).collect(),
            words: HashMap::new(),
            taken: HashSet::new(),
        }
    }

    /// A number from the key, `what` and `n`.
    fn hash(&self, what: &str, n: u32) -> u64 {
        let mut h = std::hash::DefaultHasher::new();
        (self.key, what, n).hash(&mut h);
        h.finish()
    }

    /// `run` (letters and digits) made up: each character of its class from
    /// `seed`, upper case where it was.
    fn made_up(run: &str, mut seed: u64) -> String {
        run.chars()
            .map(|c| {
                // splitmix64
                seed = seed.wrapping_add(0x9E37_79B9_7F4A_7C15);
                let mut z = seed;
                z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
                z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
                z ^= z >> 31;
                if c.is_ascii_digit() {
                    (b'0' + (z % 10) as u8) as char
                } else {
                    let l = (b'a' + (z % 26) as u8) as char;
                    if c.is_uppercase() { l.to_ascii_uppercase() } else { l }
                }
            })
            .collect()
    }

    /// A word of letters: its made-up word, with `word`'s case.
    fn word(&mut self, word: &str) -> String {
        let lower = word.to_lowercase();
        if self.keep.contains(&lower) || crate::event::level_name(word.as_bytes()).is_some() {
            return word.to_owned();
        }
        let mapped = match self.words.get(&lower) {
            Some(m) => m.clone(),
            None => {
                let mut m = String::new();
                for n in 0..TRIES {
                    m = Self::made_up(&lower, self.hash(&lower, n));
                    if !self.taken.contains(&m) {
                        break;
                    }
                }
                self.taken.insert(m.clone());
                self.words.insert(lower, m.clone());
                m
            }
        };
        // The case of the original, letter by letter.
        mapped
            .chars()
            .zip(word.chars().chain(std::iter::repeat('a')))
            .map(|(m, o)| if o.is_uppercase() { m.to_ascii_uppercase() } else { m })
            .collect()
    }

    /// `text` with every word replaced.
    pub fn text(&mut self, text: &str) -> String {
        let mut out = String::with_capacity(text.len());
        let mut run = String::new();
        let flush = |run: &mut String, out: &mut String, me: &mut Self| {
            if run.is_empty() {
                return;
            }
            if run.chars().any(|c| c.is_ascii_digit()) {
                if me.keep.contains(&run.to_lowercase()) {
                    out.push_str(run);
                } else {
                    out.push_str(&Self::made_up(run, me.hash(run, 0)));
                }
            } else {
                out.push_str(&me.word(run));
            }
            run.clear();
        };
        for c in text.chars() {
            if c.is_alphanumeric() {
                run.push(c);
            } else {
                flush(&mut run, &mut out, self);
                out.push(c);
            }
        }
        flush(&mut run, &mut out, self);
        out
    }
}

/// Write `source`'s lines (or only `lines`) anonymized into `out`: a DLT
/// source as DLT (its times, levels and IDs kept, unless `ids`: then made
/// up too; its payload as one string), any other as text lines.
pub fn anonymize(
    source: &dyn LogSource,
    lines: Option<&[LineNo]>,
    a: &mut Anonymizer,
    ids: bool,
    out: &mut dyn Write,
    progress: &Progress,
) -> Result<Written> {
    let schema = source.schema();
    let field = |name: &str| schema.iter().position(|d| d.name == name);
    let dlt = (field("ecu"), field("apid"), field("ctid"), field("payload"));
    let n = lines.map_or(source.line_count(), |l| l.len() as u64);
    progress.set_total(n);
    let text_of = |line: LineNo, id: Option<usize>| -> String { field_text(source, line, id) };
    let mut buf = Vec::new();
    for k in 0..n {
        if k % 65_536 == 0 && progress.is_cancelled() {
            return Err(crate::Error::Cancelled);
        }
        let line = lines.map_or(k, |l| l[k as usize]);
        buf.clear();
        match dlt {
            (Some(ecu), Some(apid), Some(ctid), Some(payload)) => {
                let mut id = |id: usize| {
                    let t = text_of(line, Some(id));
                    if ids { a.text(&t) } else { t }
                };
                let (ecu, apid, ctid) = (id(ecu), id(apid), id(ctid));
                let level = source.supplied_level(line).flatten().unwrap_or(crate::Level::Info);
                let text = a.text(&text_of(line, Some(payload)));
                let mut m = Msg::log(level, &ecu, &apid, &ctid, vec![Arg::from(Value::Str(text))]);
                if let Some(t) = source.time(line) {
                    m.secs = t.floor() as u32;
                    m.usecs = ((t - t.floor()) * 1e6) as i32;
                }
                m.timestamp = source.uptime(line).map(|u| (u * 10_000.0) as u32);
                m.mcnt = k as u8;
                m.encode(&mut buf);
            }
            _ => {
                let text = source.line_text(line).unwrap_or_default();
                buf.extend_from_slice(a.text(&text).as_bytes());
                buf.push(b'\n');
            }
        }
        out.write_all(&buf)?;
        progress.add(1);
    }
    Ok(match dlt {
        (Some(_), Some(_), Some(_), Some(_)) => Written::Dlt,
        _ => Written::Text,
    })
}

/// A field's value as text (none: empty).
fn field_text(source: &dyn LogSource, line: LineNo, id: Option<usize>) -> String {
    match id.and_then(|id| source.field(line, id)) {
        Some(crate::FieldValue::Str(b)) => String::from_utf8_lossy(&b).into_owned(),
        Some(crate::FieldValue::U64(n)) => n.to_string(),
        Some(crate::FieldValue::F64(f)) => f.to_string(),
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::TextFile;
    use crate::pattern::{PatternOptions, detect};

    #[test]
    fn words_are_replaced_the_same_everywhere() {
        let original = "Gear=D speed 12.5 km/h; GEAR=d Elapsed[us]: 7";
        let mut a = Anonymizer::new(Some("s"), ["Elapsed".to_owned()]);
        let x = a.text(original);
        let w: Vec<&str> = x.split(|c: char| !c.is_alphanumeric()).filter(|s| !s.is_empty()).collect();
        assert_eq!(w.len(), 12, "{x}");
        // Gear D speed 12 5 km h GEAR d Elapsed us 7: the same word, the
        // same made-up one, in its own case.
        assert_eq!((w[0].to_lowercase(), w[1].to_lowercase()), (w[7].to_lowercase(), w[8].to_lowercase()), "{x}");
        assert!(w[0].starts_with(char::is_uppercase) && w[7].chars().all(char::is_uppercase), "{x}");
        assert_ne!(w[0], "Gear");
        assert_eq!(w[9], "Elapsed");
        assert!(w[3].len() == 2 && w[3].bytes().all(|b| b.is_ascii_digit()), "{x}");
        // Everything but letters and digits stays.
        let shape = |s: &str| s.chars().map(|c| if c.is_alphanumeric() { 'x' } else { c }).collect::<String>();
        assert_eq!(shape(&x), shape(original));
        // The same seed: the same words; another: others.
        assert_eq!(Anonymizer::new(Some("s"), ["Elapsed".to_owned()]).text("Gear=D"), a.text("Gear=D"));
        assert_ne!(Anonymizer::new(Some("t"), []).text("Gear=D"), a.text("Gear=D"));
    }

    #[test]
    fn two_words_never_share_one() {
        let mut a = Anonymizer::new(Some("x"), []);
        let letters: Vec<String> = ('a'..='z').map(|c| a.text(&c.to_string())).collect();
        let distinct: HashSet<&String> = letters.iter().collect();
        assert_eq!(distinct.len(), 26);
    }

    /// The point of it: the anonymized trace has the original's patterns,
    /// with the same counts.
    #[test]
    fn patterns_survive() {
        let lines: Vec<String> = (0..400)
            .map(|i| match i % 5 {
                0 => format!("speed={} km/h heading={}", i % 90, i % 360),
                1 => format!("gear={}", ["D", "P", "N"][i % 3]),
                2 => format!("session {} to 10.0.{}.{} established", i, i % 7, i % 200),
                3 => format!("OTA state={}", ["idle", "downloading", "done"][i % 3]),
                _ => format!("user u{} logged in", i % 11),
            })
            .collect();
        let f = TextFile::from_bytes("t", lines.join("\n").into_bytes());
        let mut out = Vec::new();
        let mut a = Anonymizer::new(None, []);
        assert_eq!(anonymize(&f, None, &mut a, false, &mut out, &Progress::new()).unwrap(), Written::Text);
        let g = TextFile::from_bytes("a", out);
        let shape = |s: &dyn LogSource| {
            let ps = detect(s, None, &PatternOptions::default(), &Progress::new()).unwrap();
            ps.patterns.iter().map(|p| (p.count(), p.slots().count())).collect::<Vec<_>>()
        };
        assert_eq!(shape(&f), shape(&g));
        assert!(!g.line_text(1).unwrap().contains("gear"));
    }

    #[test]
    fn dlt_stays_dlt() {
        use crate::input::dlt::writer::Msg;
        let mut bytes = Vec::new();
        for (i, (apid, text)) in [("NAV", "gear D"), ("NAV", "gear P"), ("PWR", "voltage 12000 mV")].iter().enumerate()
        {
            let mut m = Msg::log(crate::Level::Warn, "ECU1", apid, "CTX", vec![Arg::from(Value::Str((*text).into()))]);
            m.secs += i as u32;
            m.encode(&mut bytes);
        }
        let f = crate::DltFile::from_bytes("t.dlt", bytes);
        let mut out = Vec::new();
        let mut a = Anonymizer::new(Some("k"), []);
        assert_eq!(anonymize(&f, None, &mut a, false, &mut out, &Progress::new()).unwrap(), Written::Dlt);
        let g = crate::DltFile::from_bytes("a.dlt", out);
        assert_eq!(g.line_count(), 3);
        let apid = g.schema().iter().position(|d| d.name == "apid").unwrap();
        assert_eq!(field_text(&g, 2, Some(apid)), "PWR");
        assert_eq!(g.time(1), f.time(1));
        assert_eq!(g.supplied_level(0), f.supplied_level(0));
        let payload = g.schema().iter().position(|d| d.name == "payload").unwrap();
        let p = field_text(&g, 2, Some(payload));
        assert!(!p.contains("voltage") && p.split(' ').count() == 3, "{p}");
    }
}
