//! Settings bundles: the user's analysis setup (packs, presets, plots,
//! checks, signals) as one text file to export, share and import, and the
//! format numbers and migration steps that let a later nerka read what an
//! earlier one wrote. Design: `docs/dev/bundles.md`.
//!
//! File format (`*.nerka-bundle`, UTF-8):
//!
//! ```text
//! nerka-bundle 1
//! nerka 0.1.0
//! # Comments and blank lines before the first item are ignored.
//!
//! @@ pack vision format 1
//! pack vision
//!
//! query cam
//!     apid == CAM
//! @@ check storm format 1
//! storm: lines level >= ERROR; rate 25 per 1s
//! ```
//!
//! ```text
//! file   := "nerka-bundle" VERSION NEWLINE ["nerka" TEXT NEWLINE] (comment | blank)* item*
//! item   := "@@" KIND NAME "format" N NEWLINE line*   -- up to the next header or the end
//! KIND   := "pack" | "preset" | "plot" | "check" | "signal"
//! NAME   := a word, or "…" with \" and \\           -- as in presets
//! line   := any line; one starting with "@@" is written with one more "@"
//! ```
//!
//! An item is a pack, preset or plot file, or one check or signal line, as
//! it is. Its format number says which format its text is in; [`STEPS`]
//! turn a kind's text from one format into the next, so an older item is
//! upgraded ([`upgrade`]) before it is parsed. A [`Store`] is a config
//! folder: its items, an export of them, the upgrade of the folder itself,
//! and an import planned ([`Store::plan`]) and applied ([`Store::apply`]).

use crate::check::CheckDef;
use crate::pack::{PackError, is_name, parse_pack};
use crate::plot::parse_plot;
use crate::preset::{parse_preset, quote, words};
use crate::signal::SignalDef;
use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

/// The bundle file's own layout.
pub const BUNDLE_VERSION: u32 = 1;
/// File extension of bundles.
pub const BUNDLE_EXTENSION: &str = "nerka-bundle";
/// The file in a config folder that says which format its files are in.
pub const FORMAT_FILE: &str = "format";
/// Where backups go in a config folder.
pub const BACKUP_DIR: &str = "backups";

/// What an item is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Kind {
    Pack,
    Preset,
    Plot,
    Check,
    Signal,
}

impl Kind {
    pub const ALL: [Kind; 5] = [Kind::Pack, Kind::Preset, Kind::Plot, Kind::Check, Kind::Signal];

    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Pack => "pack",
            Kind::Preset => "preset",
            Kind::Plot => "plot",
            Kind::Check => "check",
            Kind::Signal => "signal",
        }
    }

    pub fn plural(self) -> &'static str {
        match self {
            Kind::Pack => "packs",
            Kind::Preset => "presets",
            Kind::Plot => "plots",
            Kind::Check => "checks",
            Kind::Signal => "signals",
        }
    }

    /// `pack` or `packs` (any case).
    pub fn parse(s: &str) -> Option<Kind> {
        Kind::ALL.into_iter().find(|k| s.eq_ignore_ascii_case(k.as_str()) || s.eq_ignore_ascii_case(k.plural()))
    }

    /// The format this nerka reads and writes.
    pub fn format(self) -> u32 {
        current(self, STEPS)
    }

    /// Kinds kept one item per line in a shared file (`checks`, `signals`).
    pub fn is_line(self) -> bool {
        matches!(self, Kind::Check | Kind::Signal)
    }

    /// The folder (pack, preset, plot) or file (check, signal) in a config folder.
    fn place(self, root: &Path) -> PathBuf {
        root.join(self.plural())
    }

    fn extension(self) -> &'static str {
        match self {
            Kind::Pack => crate::pack::PACK_EXTENSION,
            Kind::Preset => crate::preset::PRESET_EXTENSION,
            Kind::Plot => crate::plot::PLOT_EXTENSION,
            Kind::Check | Kind::Signal => "",
        }
    }

    /// The first line of a new `checks` / `signals` file.
    fn file_header(self) -> String {
        match self {
            Kind::Check => crate::check::Checks::default().to_text(),
            Kind::Signal => crate::Signals::default().to_text(),
            _ => String::new(),
        }
    }
}

impl std::fmt::Display for Kind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One migration: a kind's text in format `from` to format `from + 1`.
/// The step keeps the item's name.
pub struct Step {
    pub kind: Kind,
    pub from: u32,
    pub run: fn(&str) -> Result<String, String>,
}

/// Every migration step. A breaking change to a kind's file format adds
/// one here (and a test on a sample of the old format); the kind's
/// [`Kind::format`] is 1 plus its number of steps.
pub const STEPS: &[Step] = &[];

fn current(kind: Kind, steps: &[Step]) -> u32 {
    1 + steps.iter().filter(|s| s.kind == kind).count() as u32
}

/// `text`, an item of `kind` in format `from`, in the current format.
pub fn upgrade(kind: Kind, from: u32, text: &str) -> Result<String, String> {
    upgrade_with(STEPS, kind, from, text)
}

fn upgrade_with(steps: &[Step], kind: Kind, from: u32, text: &str) -> Result<String, String> {
    let to = current(kind, steps);
    if from == 0 {
        return Err(format!("{kind} format 0 doesn't exist"));
    }
    if from > to {
        return Err(format!(
            "made by a newer nerka: {kind} format {from}, this nerka reads up to {to}; update nerka to import it"
        ));
    }
    let mut text = text.to_owned();
    for f in from..to {
        let step = steps
            .iter()
            .find(|s| s.kind == kind && s.from == f)
            .ok_or_else(|| format!("no upgrade from {kind} format {f}"))?;
        text = (step.run)(&text).map_err(|e| format!("upgrading {kind} format {f} to {}: {e}", f + 1))?;
    }
    Ok(text)
}

/// A pack, preset or plot file, or a check or signal line, in a format.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    pub kind: Kind,
    /// Read from the text for items in the current format; the header's
    /// (or the file's) name for others.
    pub name: String,
    pub format: u32,
    /// Ends with a line break.
    pub text: String,
}

impl Item {
    /// An item of the current format, its name read from `text`; the
    /// error says why it isn't one.
    pub fn new(kind: Kind, text: &str) -> Result<Item, String> {
        let text = tidy(text);
        let name = name_of(kind, &text)?;
        Ok(Item { kind, name, format: kind.format(), text })
    }

    /// `kind name`, for messages.
    pub fn label(&self) -> String {
        format!("{} {}", self.kind, self.name)
    }
}

/// Line breaks as `\n`, one at the end.
fn tidy(text: &str) -> String {
    let mut t = text.replace("\r\n", "\n");
    if !t.ends_with('\n') {
        t.push('\n');
    }
    t
}

/// The lines of a check or signal item that aren't blank or comments: it
/// must be one.
fn one_line(kind: Kind, text: &str) -> Result<String, String> {
    let mut lines = text.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#'));
    match (lines.next(), lines.next()) {
        (Some(l), None) => Ok(l.to_owned()),
        (None, _) => Err(format!("a {kind} is one line; this one is empty")),
        (Some(_), Some(_)) => Err(format!("a {kind} is one line; this one has more")),
    }
}

/// Parse an item's text of the current format: its name, or why it can't be
/// read.
pub fn name_of(kind: Kind, text: &str) -> Result<String, String> {
    let file = kind.as_str();
    let pe = |e: PackError| e.to_string();
    match kind {
        Kind::Pack => parse_pack(file, text).map(|p| p.name).map_err(pe),
        Kind::Preset => parse_preset(file, text).map(|p| p.name).map_err(pe),
        Kind::Plot => parse_plot(file, text).map(|p| p.name).map_err(pe),
        Kind::Check => {
            let line = one_line(kind, text)?;
            CheckDef::parse_spec(&line).map(|c| c.name().to_owned()).map_err(|e| e.at(&line))
        }
        Kind::Signal => {
            let line = one_line(kind, text)?;
            SignalDef::parse_spec(&line).map(|d| d.name().to_owned()).map_err(|e| e.to_string())
        }
    }
}

/// The text two items compare by: line ends and trailing spaces aside, and
/// a check or signal as nerka writes it.
fn canonical(kind: Kind, text: &str) -> String {
    let spelled = match kind {
        Kind::Check => one_line(kind, text).ok().and_then(|l| CheckDef::parse_spec(&l).ok()).map(|c| c.spec()),
        Kind::Signal => one_line(kind, text).ok().and_then(|l| SignalDef::parse_spec(&l).ok()).map(|d| d.spec()),
        _ => None,
    };
    if let Some(s) = spelled {
        return s;
    }
    let lines: Vec<&str> = text.lines().map(str::trim_end).collect();
    lines.join("\n").trim_end().to_owned()
}

/// `text` (an item of the current format) with its name changed to `new`.
pub fn renamed(kind: Kind, text: &str, new: &str) -> Result<String, String> {
    let mut lines: Vec<String> = text.lines().map(str::to_owned).collect();
    let first = lines
        .iter()
        .position(|l| !l.trim().is_empty() && !l.trim_start().starts_with('#'))
        .ok_or_else(|| format!("an empty {kind}"))?;
    let line = lines[first].trim().to_owned();
    lines[first] = match kind {
        Kind::Pack => format!("pack {}", if is_name(new) { new.to_owned() } else { format!("\"{new}\"") }),
        Kind::Preset | Kind::Plot => format!("{kind} {}", quote(new)),
        Kind::Check => {
            let (off, rest) = line.strip_prefix("off ").map_or(("", line.as_str()), |r| ("off ", r));
            let colon = rest.find(':').ok_or("a check without ':'")?;
            format!("{off}{new}{}", &rest[colon..])
        }
        Kind::Signal => {
            let (off, rest) = line.strip_prefix("off ").map_or(("", line.as_str()), |r| ("off ", r.trim_start()));
            let (life, rest) = rest.strip_prefix("lifecycle ").map_or(("", rest), |r| ("lifecycle ", r.trim_start()));
            let end = if life.is_empty() { rest.find(['(', ':', '=']) } else { rest.find(':') };
            let end = end.ok_or("a signal without a name")?;
            // A signal without a group captures the group of its name: it keeps that one.
            let old = rest[..end].trim();
            let group =
                if life.is_empty() && !rest[end..].starts_with('(') { format!("({old})") } else { String::new() };
            format!("{off}{life}{new}{group}{}", &rest[end..])
        }
    };
    let out = tidy(&lines.join("\n"));
    match name_of(kind, &out) {
        Ok(n) if n == new => Ok(out),
        Ok(n) => Err(format!("renamed to '{new}', it reads as '{n}'")),
        Err(e) => Err(format!("renamed to '{new}': {e}")),
    }
}

/// An item header being read: (its line, the kind or an unknown kind's
/// word, name, format).
type Header = (usize, Result<Kind, String>, String, u32);

/// A bundle file's contents.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Bundle {
    /// The nerka version that wrote it (`nerka` line), if it says.
    pub nerka: Option<String>,
    /// As written: names from the headers, texts in their formats.
    pub items: Vec<Item>,
    /// Items of kinds this nerka doesn't know: (line, kind, name), 0-based.
    pub unknown: Vec<(usize, String, String)>,
}

impl Bundle {
    pub fn new(items: Vec<Item>) -> Bundle {
        Bundle { nerka: Some(env!("CARGO_PKG_VERSION").to_owned()), items, unknown: Vec::new() }
    }

    /// The file text.
    pub fn to_text(&self) -> String {
        let mut out = format!("nerka-bundle {BUNDLE_VERSION}\n");
        if let Some(v) = &self.nerka {
            out += &format!("nerka {v}\n");
        }
        out.push('\n');
        for it in &self.items {
            out += &format!("@@ {} {} format {}\n", it.kind, quote(&it.name), it.format);
            for l in it.text.lines() {
                if l.starts_with("@@") {
                    out.push('@');
                }
                out += l;
                out.push('\n');
            }
        }
        out
    }

    /// Read a bundle; `file` is used in error messages.
    pub fn parse(file: &str, text: &str) -> Result<Bundle, PackError> {
        let err = |ln: usize, col: usize, msg: String| PackError { file: file.to_owned(), pos: Some((ln, col)), msg };
        let lines: Vec<&str> = text.lines().collect();
        let mut i = 0;
        while i < lines.len() && lines[i].trim().is_empty() {
            i += 1;
        }
        let Some(first) = lines.get(i) else {
            return Err(PackError {
                file: file.to_owned(),
                pos: None,
                msg: "an empty file, not a nerka bundle".into(),
            });
        };
        let version = first
            .trim()
            .strip_prefix("nerka-bundle ")
            .and_then(|v| v.trim().parse::<u32>().ok())
            .ok_or_else(|| err(i, 0, "not a nerka bundle (its first line would be 'nerka-bundle 1')".into()))?;
        let mut b = Bundle::default();
        if let Some(v) = lines.get(i + 1).and_then(|l| l.trim().strip_prefix("nerka ")) {
            b.nerka = Some(v.trim().to_owned());
        }
        if version > BUNDLE_VERSION || version == 0 {
            let by = b.nerka.as_ref().map(|v| format!(" (nerka {v})")).unwrap_or_default();
            return Err(err(
                i,
                13,
                format!(
                    "bundle layout {version}{by}: this nerka reads layout {BUNDLE_VERSION}; update nerka to read it"
                ),
            ));
        }
        i += 1 + usize::from(b.nerka.is_some());
        let mut cur: Option<Header> = None;
        let mut body = String::new();
        fn finish(b: &mut Bundle, cur: Option<Header>, body: &mut String) {
            match cur {
                Some((_, Ok(kind), name, format)) => {
                    b.items.push(Item { kind, name, format, text: std::mem::take(body) })
                }
                Some((ln, Err(kind), name, _)) => {
                    b.unknown.push((ln, kind, name));
                    body.clear();
                }
                None => {}
            }
        }
        for (ln, &line) in lines.iter().enumerate().skip(i) {
            let header = line == "@@" || line.starts_with("@@ ");
            if !header {
                if cur.is_none() {
                    let t = line.trim();
                    if t.is_empty() || t.starts_with('#') {
                        continue;
                    }
                    return Err(err(ln, 0, "expected an item: @@ KIND NAME format N".into()));
                }
                body += line.strip_prefix('@').filter(|_| line.starts_with("@@@")).unwrap_or(line);
                body.push('\n');
                continue;
            }
            finish(&mut b, cur.take(), &mut body);
            let ws = words(&line[2..]).map_err(|c| err(ln, c + 2, "unclosed quote".into()))?;
            let col = |k: usize| ws.get(k).map_or(line.chars().count(), |w| w.1 + 2);
            let [kind, name, fmt, n] = &ws[..] else {
                // At the end of a short header, at the word too many of a long one.
                return Err(err(ln, col(4), "expected @@ KIND NAME format N".into()));
            };
            if fmt.0 != "format" {
                return Err(err(ln, col(2), format!("expected 'format' but found '{}'", fmt.0)));
            }
            let n: u32 =
                n.0.parse().map_err(|_| err(ln, col(3), format!("expected a format number but found '{}'", n.0)))?;
            let kind = Kind::ALL.into_iter().find(|k| k.as_str() == kind.0).ok_or_else(|| kind.0.clone());
            cur = Some((ln, kind, name.0.clone(), n));
        }
        finish(&mut b, cur, &mut body);
        Ok(b)
    }
}

/// An item in a config folder, and the file it is in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stored {
    pub item: Item,
    pub path: PathBuf,
}

/// A config folder (`<config dir>`, `%APPDATA%\nerka`): the analysis setup
/// in it, as items.
#[derive(Debug, Clone)]
pub struct Store {
    root: PathBuf,
}

/// What [`Store::upgrade`] did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Upgraded {
    /// (kind, from, to) of each kind upgraded.
    pub kinds: Vec<(Kind, u32, u32)>,
    /// The bundle of everything as it was before.
    pub backup: Option<PathBuf>,
    /// Kinds written by a newer nerka, left as they are.
    pub newer: Vec<String>,
    pub errors: Vec<String>,
}

impl Upgraded {
    /// What to tell the user, if anything.
    pub fn message(&self) -> Option<String> {
        let mut parts = Vec::new();
        if !self.kinds.is_empty() {
            let kinds: Vec<String> = self.kinds.iter().map(|(k, f, t)| format!("{} {f}→{t}", k.plural())).collect();
            let backup = self.backup.as_ref().map(|b| format!(" (backup: {})", b.display())).unwrap_or_default();
            parts.push(format!("settings upgraded: {}{backup}", kinds.join(", ")));
        }
        parts.extend(self.newer.iter().cloned());
        parts.extend(self.errors.iter().cloned());
        (!parts.is_empty()).then(|| parts.join("; "))
    }
}

impl Store {
    pub fn new(root: impl Into<PathBuf>) -> Store {
        Store { root: root.into() }
    }

    /// The user's config folder, if there is one.
    pub fn user() -> Option<Store> {
        crate::config_dir().map(Store::new)
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Each kind's format in the folder (a folder without a format file:
    /// format 1).
    pub fn formats(&self) -> Result<BTreeMap<Kind, u32>, String> {
        let mut out: BTreeMap<Kind, u32> = Kind::ALL.into_iter().map(|k| (k, 1)).collect();
        let path = self.root.join(FORMAT_FILE);
        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(out),
            Err(e) => return Err(format!("{}: {e}", path.display())),
        };
        for line in text.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')) {
            let (k, n) = line.split_once(' ').unwrap_or((line, ""));
            // A kind a later nerka added: not ours to read.
            let Some(k) = Kind::parse(k) else { continue };
            let n = n.trim().parse().map_err(|_| format!("{}: '{line}' isn't KIND NUMBER", path.display()))?;
            out.insert(k, n);
        }
        Ok(out)
    }

    fn write_formats(&self, formats: &BTreeMap<Kind, u32>) -> Result<(), String> {
        let mut text =
            String::from("# The formats of nerka's files in this folder (docs/dev/bundles.md); nerka keeps it.\n");
        for (k, n) in formats {
            text += &format!("{k} {n}\n");
        }
        let path = self.root.join(FORMAT_FILE);
        std::fs::create_dir_all(&self.root)
            .and_then(|_| crate::write_atomic(&path, text))
            .map_err(|e| format!("{}: {e}", path.display()))
    }

    /// Every item of the folder, and what couldn't be read.
    pub fn items(&self) -> (Vec<Stored>, Vec<String>) {
        self.items_with(STEPS)
    }

    fn items_with(&self, steps: &[Step]) -> (Vec<Stored>, Vec<String>) {
        let (mut out, mut errors) = (Vec::new(), Vec::new());
        let formats = match self.formats() {
            Ok(f) => f,
            Err(e) => return (out, vec![e]),
        };
        for kind in Kind::ALL {
            let format = formats[&kind];
            let is_current = format == current(kind, steps);
            // An item of another format is named by its file or line.
            let item = |text: String, fallback: String| -> Result<Item, String> {
                let text = tidy(&text);
                let name = if is_current { name_of(kind, &text)? } else { fallback };
                Ok(Item { kind, name, format, text })
            };
            let place = kind.place(&self.root);
            if kind.is_line() {
                let text = match std::fs::read_to_string(&place) {
                    Ok(t) => t,
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                    Err(e) => {
                        errors.push(format!("{}: {e}", place.display()));
                        continue;
                    }
                };
                for (i, line) in text.lines().enumerate() {
                    let t = line.trim();
                    if t.is_empty() || t.starts_with('#') {
                        continue;
                    }
                    match item(t.to_owned(), format!("line {}", i + 1)) {
                        Ok(it) => out.push(Stored { item: it, path: place.clone() }),
                        Err(e) => errors.push(format!("{}:{}: {e}", place.display(), i + 1)),
                    }
                }
                continue;
            }
            for path in files(&place, kind.extension()) {
                let stem = path.file_stem().unwrap_or_default().to_string_lossy().into_owned();
                match std::fs::read_to_string(&path).map_err(|e| e.to_string()).and_then(|t| item(t, stem)) {
                    Ok(it) => out.push(Stored { item: it, path }),
                    Err(e) => errors.push(format!("{}: {e}", path.display())),
                }
            }
        }
        (out, errors)
    }

    /// A bundle of the items `pick` takes, and what couldn't be read.
    pub fn export(&self, pick: &dyn Fn(&Item) -> bool) -> (Bundle, Vec<String>) {
        let (items, errors) = self.items();
        (Bundle::new(items.into_iter().map(|s| s.item).filter(|i| pick(i)).collect()), errors)
    }

    /// Bring the folder's files to this nerka's formats: a backup bundle of
    /// everything first, then each file of an older format upgraded in
    /// place, then the format file. A folder that doesn't exist is left
    /// alone.
    pub fn upgrade(&self) -> Upgraded {
        self.upgrade_with(STEPS)
    }

    fn upgrade_with(&self, steps: &[Step]) -> Upgraded {
        let mut up = Upgraded::default();
        if !self.root.is_dir() {
            return up;
        }
        let mut formats = match self.formats() {
            Ok(f) => f,
            Err(e) => {
                up.errors.push(e);
                return up;
            }
        };
        let has_file = self.root.join(FORMAT_FILE).is_file();
        let mut todo = Vec::new();
        for (&k, &f) in &formats {
            let to = current(k, steps);
            if f > to {
                up.newer.push(format!(
                    "{} here were saved by a newer nerka ({k} format {f}, this nerka reads up to {to}): update nerka",
                    k.plural()
                ));
            } else if f < to {
                todo.push((k, f, to));
            }
        }
        if todo.is_empty() {
            if !has_file && let Err(e) = self.write_formats(&formats) {
                up.errors.push(e);
            }
            return up;
        }
        let (items, errors) = self.items_with(steps);
        up.errors.extend(errors);
        match self.backup("before-upgrade", &Bundle::new(items.into_iter().map(|s| s.item).collect())) {
            Ok(p) => up.backup = Some(p),
            Err(e) => {
                up.errors.push(format!("not upgraded: no backup: {e}"));
                return up;
            }
        }
        for (kind, from, to) in todo {
            let place = kind.place(&self.root);
            let run = |text: &str| upgrade_with(steps, kind, from, text);
            let mut failed = false;
            if kind.is_line() {
                if let Ok(text) = std::fs::read_to_string(&place) {
                    let mut out = String::new();
                    for line in text.lines() {
                        let t = line.trim();
                        if t.is_empty() || t.starts_with('#') {
                            out += line;
                            out.push('\n');
                            continue;
                        }
                        match run(&tidy(t)) {
                            Ok(new) => out += &new,
                            Err(e) => {
                                up.errors.push(format!("{}: {e}", place.display()));
                                failed = true;
                                out += &tidy(line);
                            }
                        }
                    }
                    if let Err(e) = crate::write_atomic(&place, out) {
                        up.errors.push(format!("{}: {e}", place.display()));
                        failed = true;
                    }
                }
            } else {
                for path in files(&place, kind.extension()) {
                    let r = std::fs::read_to_string(&path)
                        .map_err(|e| e.to_string())
                        .and_then(|t| run(&tidy(&t)))
                        .and_then(|new| crate::write_atomic(&path, new).map_err(|e| e.to_string()));
                    if let Err(e) = r {
                        up.errors.push(format!("{}: {e}", path.display()));
                        failed = true;
                    }
                }
            }
            if !failed {
                formats.insert(kind, to);
                up.kinds.push((kind, from, to));
            }
        }
        if let Err(e) = self.write_formats(&formats) {
            up.errors.push(e);
        }
        up
    }

    /// Write `bundle` to `<root>/backups/<what>-<UTC time>.nerka-bundle`.
    fn backup(&self, what: &str, bundle: &Bundle) -> Result<PathBuf, String> {
        let dir = self.root.join(BACKUP_DIR);
        std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        let stamp = stamp();
        let mut path = dir.join(format!("{what}-{stamp}.{BUNDLE_EXTENSION}"));
        let mut n = 2;
        while path.exists() {
            path = dir.join(format!("{what}-{stamp}-{n}.{BUNDLE_EXTENSION}"));
            n += 1;
        }
        crate::write_atomic(&path, bundle.to_text()).map_err(|e| format!("{}: {e}", path.display()))?;
        Ok(path)
    }

    /// What importing `bundle` would do: each item's status and the Auto
    /// choice. Run [`upgrade`](Self::upgrade) on the folder first.
    pub fn plan(&self, bundle: &Bundle) -> Plan {
        let (mine, errors) = self.items();
        let key = |k: Kind, n: &str| (k, n.to_lowercase());
        let mut by_name: std::collections::HashMap<(Kind, String), &Stored> = Default::default();
        for s in &mine {
            by_name.entry(key(s.item.kind, &s.item.name)).or_insert(s);
        }
        // Names that are taken: mine and the bundle's (as read from the texts).
        let mut taken: HashSet<(Kind, String)> = by_name.keys().cloned().collect();
        let mut rows = Vec::new();
        let mut seen = HashSet::new();
        for it in &bundle.items {
            let read = upgrade(it.kind, it.format, &it.text).and_then(|t| Item::new(it.kind, &t));
            let row = match read {
                Err(e) => Planned::invalid(it.kind, &it.name, e),
                Ok(theirs) => {
                    let k = key(theirs.kind, &theirs.name);
                    taken.insert(k.clone());
                    if !seen.insert(k.clone()) {
                        Planned::invalid(it.kind, &theirs.name, "twice in the bundle; the first one is imported".into())
                    } else {
                        let mine = by_name.get(&k);
                        let status = match mine {
                            None => Status::New,
                            Some(m) if canonical(m.item.kind, &m.item.text) == canonical(theirs.kind, &theirs.text) => {
                                Status::Same
                            }
                            Some(_) => Status::Different,
                        };
                        Planned {
                            kind: theirs.kind,
                            name: theirs.name.clone(),
                            mine: mine.map(|m| m.path.clone()),
                            both_name: String::new(),
                            choice: Choice::Skip,
                            status,
                            item: Some(theirs),
                        }
                    }
                }
            };
            rows.push(row);
        }
        for r in rows.iter_mut().filter(|r| r.status == Status::Different) {
            let mut n = 1;
            loop {
                let cand = match n {
                    1 => format!("{}_imported", r.name),
                    n => format!("{}_imported_{n}", r.name),
                };
                if taken.insert(key(r.kind, &cand)) {
                    r.both_name = cand;
                    break;
                }
                n += 1;
            }
        }
        let mut notes = errors;
        for (ln, kind, name) in &bundle.unknown {
            notes.push(format!("line {}: {kind} {name}: a kind this nerka doesn't know, skipped", ln + 1));
        }
        let mut plan = Plan { items: rows, notes };
        plan.set_auto();
        plan
    }

    /// Import what `plan` says: items replaced are written into a backup
    /// bundle first (none is replaced if that fails).
    pub fn apply(&self, plan: &Plan) -> Applied {
        let mut done = Applied::default();
        let replaced: Vec<&Planned> =
            plan.items.iter().filter(|p| p.status == Status::Different && p.choice == Choice::Take).collect();
        if !replaced.is_empty() {
            let (mine, _) = self.items();
            let old: Vec<Item> = mine
                .into_iter()
                .filter(|s| replaced.iter().any(|p| p.kind == s.item.kind && p.name.eq_ignore_ascii_case(&s.item.name)))
                .map(|s| s.item)
                .collect();
            match self.backup("before-import", &Bundle::new(old)) {
                Ok(p) => done.backup = Some(p),
                Err(e) => {
                    done.errors.push(format!("nothing imported: no backup of what it would replace: {e}"));
                    return done;
                }
            }
        }
        if !self.root.join(FORMAT_FILE).is_file()
            && let Ok(f) = self.formats()
            && let Err(e) = self.write_formats(&f)
        {
            done.errors.push(e);
        }
        // Check and signal lines, per kind: (plan row, replace mine, new text, name written).
        let mut lines: BTreeMap<Kind, Vec<(usize, bool, String, String)>> = BTreeMap::new();
        for (row, p) in plan.items.iter().enumerate() {
            let Some(it) = &p.item else { continue };
            let (text, name, replace) = match (p.choice, &p.status) {
                (Choice::Skip, _) | (_, Status::Same | Status::Invalid(_)) => continue,
                (Choice::Take, Status::New) => (it.text.clone(), it.name.clone(), false),
                (Choice::Take, Status::Different) => (it.text.clone(), it.name.clone(), true),
                (Choice::Both, _) => match renamed(it.kind, &it.text, &p.both_name) {
                    Ok(t) => (t, p.both_name.clone(), false),
                    Err(e) => {
                        done.errors.push(format!("{}: {e}", it.label()));
                        continue;
                    }
                },
            };
            if it.kind.is_line() {
                lines.entry(it.kind).or_default().push((row, replace, text, name));
                continue;
            }
            let path = match (&p.mine, replace) {
                (Some(m), true) => m.clone(),
                _ => free_file(&it.kind.place(&self.root), &name, it.kind.extension()),
            };
            let write =
                path.parent().map_or(Ok(()), std::fs::create_dir_all).and_then(|_| crate::write_atomic(&path, &text));
            match write {
                Ok(()) => done.record(p, &name),
                Err(e) => done.errors.push(format!("{}: {e}", path.display())),
            }
        }
        for (kind, new) in lines {
            let place = kind.place(&self.root);
            let mut file: Vec<String> = match std::fs::read_to_string(&place) {
                Ok(t) => t.lines().map(str::to_owned).collect(),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    kind.file_header().lines().map(str::to_owned).collect()
                }
                Err(e) => {
                    done.errors.push(format!("{}: {e}", place.display()));
                    continue;
                }
            };
            let named = |l: &str, name: &str| {
                let t = l.trim();
                !t.is_empty() && !t.starts_with('#') && name_of(kind, t).is_ok_and(|n| n.eq_ignore_ascii_case(name))
            };
            for (_, replace, text, name) in &new {
                let line = text.trim_end().to_owned();
                match file.iter().position(|l| *replace && named(l, name)) {
                    Some(i) => file[i] = line,
                    None => file.push(line),
                }
            }
            let mut out = file.join("\n");
            out.push('\n');
            let write = std::fs::create_dir_all(&self.root).and_then(|_| crate::write_atomic(&place, out));
            match write {
                Ok(()) => new.iter().for_each(|(row, _, _, name)| done.record(&plan.items[*row], name)),
                Err(e) => done.errors.push(format!("{}: {e}", place.display())),
            }
        }
        done
    }
}

/// The files with `ext` in `dir`, sorted by name.
fn files(dir: &Path, ext: &str) -> Vec<PathBuf> {
    let Ok(rd) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut v: Vec<PathBuf> = rd
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.is_file() && p.extension().is_some_and(|x| x == ext))
        .collect();
    v.sort();
    v
}

/// `<dir>/<name>.<ext>`, or `<name>_2.<ext>` … if that exists; characters
/// a file name can't hold become `_`.
fn free_file(dir: &Path, name: &str, ext: &str) -> PathBuf {
    let safe: String = name
        .chars()
        .map(|c| if matches!(c, '/' | '\\' | '*' | '?' | '<' | '>' | '|' | ':' | '"') { '_' } else { c })
        .collect();
    let mut path = dir.join(format!("{safe}.{ext}"));
    let mut n = 2;
    while path.exists() {
        path = dir.join(format!("{safe}_{n}.{ext}"));
        n += 1;
    }
    path
}

/// Now in UTC, `YYYYMMDD-HHMMSS`.
fn stamp() -> String {
    let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs()) as i64;
    let (y, m, d) = crate::time::civil_from_days(secs.div_euclid(86_400));
    let s = secs.rem_euclid(86_400);
    format!("{y:04}{m:02}{d:02}-{:02}{:02}{:02}", s / 3600, s / 60 % 60, s % 60)
}

/// How an item of a bundle compares with the folder's.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    /// No item of that kind and name.
    New,
    /// The same text as mine.
    Same,
    /// Mine has the name, with other text.
    Different,
    /// Doesn't upgrade or parse, or is in the bundle twice: why.
    Invalid(String),
}

/// What to do with an item.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Choice {
    /// Add it (new), or replace mine with it (different).
    Take,
    /// Leave it out (keep mine).
    Skip,
    /// Import it under another name, keeping mine (different).
    Both,
}

/// One item of an import.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Planned {
    pub kind: Kind,
    /// Its name (the header's, if it can't be read).
    pub name: String,
    /// Upgraded to the current format; none if invalid.
    pub item: Option<Item>,
    pub status: Status,
    pub choice: Choice,
    /// The name it gets with [`Choice::Both`].
    pub both_name: String,
    /// The file mine is in.
    pub mine: Option<PathBuf>,
}

impl Planned {
    fn invalid(kind: Kind, name: &str, why: String) -> Planned {
        Planned {
            kind,
            name: name.to_owned(),
            item: None,
            status: Status::Invalid(why),
            choice: Choice::Skip,
            both_name: String::new(),
            mine: None,
        }
    }

    /// The choice that loses nothing and overwrites nothing.
    pub fn auto(&self) -> Choice {
        match self.status {
            Status::New => Choice::Take,
            Status::Different => Choice::Both,
            Status::Same | Status::Invalid(_) => Choice::Skip,
        }
    }

    /// The choices that make sense for its status.
    pub fn choices(&self) -> &'static [Choice] {
        match self.status {
            Status::New => &[Choice::Take, Choice::Skip],
            Status::Different => &[Choice::Both, Choice::Take, Choice::Skip],
            Status::Same | Status::Invalid(_) => &[Choice::Skip],
        }
    }

    /// What its choice does, in words.
    pub fn choice_text(&self, c: Choice) -> String {
        match (c, &self.status) {
            (Choice::Take, Status::New) => "add".into(),
            (Choice::Take, _) => "take theirs".into(),
            (Choice::Skip, Status::New) => "skip".into(),
            (Choice::Skip, Status::Different) => "keep mine".into(),
            (Choice::Skip, _) => "—".into(),
            (Choice::Both, _) => format!("keep both (as {})", self.both_name),
        }
    }

    pub fn status_text(&self) -> &'static str {
        match self.status {
            Status::New => "new",
            Status::Same => "same",
            Status::Different => "different",
            Status::Invalid(_) => "invalid",
        }
    }
}

/// An import, item by item.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Plan {
    pub items: Vec<Planned>,
    /// Items of unknown kinds, and folder items that couldn't be read.
    pub notes: Vec<String>,
}

impl Plan {
    /// Every item's choice to its Auto one.
    pub fn set_auto(&mut self) {
        for p in &mut self.items {
            p.choice = p.auto();
        }
    }

    /// Every different item's choice to `c`; the others Auto.
    pub fn set_conflicts(&mut self, c: Choice) {
        for p in &mut self.items {
            p.choice = if p.status == Status::Different { c } else { p.auto() };
        }
    }

    /// Items it would write.
    pub fn changes(&self) -> usize {
        self.items
            .iter()
            .filter(|p| p.choice != Choice::Skip && matches!(p.status, Status::New | Status::Different))
            .count()
    }

    pub fn invalid(&self) -> usize {
        self.items.iter().filter(|p| matches!(p.status, Status::Invalid(_))).count()
    }
}

/// What [`Store::apply`] did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Applied {
    pub added: Vec<String>,
    pub replaced: Vec<String>,
    /// (theirs, the name it got).
    pub renamed: Vec<(String, String)>,
    /// The bundle of what was replaced.
    pub backup: Option<PathBuf>,
    pub errors: Vec<String>,
}

impl Applied {
    fn record(&mut self, p: &Planned, written: &str) {
        let label = format!("{} {}", p.kind, p.name);
        match (p.choice, &p.status) {
            (Choice::Both, _) => self.renamed.push((label, written.to_owned())),
            (Choice::Take, Status::Different) => self.replaced.push(label),
            _ => self.added.push(label),
        }
    }

    /// `2 added, 1 replaced, 1 kept both`, or `nothing imported`.
    pub fn summary(&self) -> String {
        let mut parts = Vec::new();
        for (n, what) in
            [(self.added.len(), "added"), (self.replaced.len(), "replaced"), (self.renamed.len(), "kept both")]
        {
            if n > 0 {
                parts.push(format!("{n} {what}"));
            }
        }
        if parts.is_empty() {
            parts.push("nothing imported".into());
        }
        let mut s = parts.join(", ");
        if let Some(b) = &self.backup {
            s += &format!(" (replaced ones backed up to {})", b.display());
        }
        if !self.errors.is_empty() {
            s += &format!(
                "; {} error{}: {}",
                self.errors.len(),
                if self.errors.len() == 1 { "" } else { "s" },
                self.errors.join("; ")
            );
        }
        s
    }
}

#[cfg(test)]
mod tests;
