//! Protocols in captures, in the GUI (docs/dev/protocol-definitions.md):
//! definitions kept in `<config>/nerka/protocols`; the Settings section that
//! turns protocols on and off and edits, rule by rule, which UDP datagrams
//! are theirs; and the protocols of each capture of a tab, in its Files
//! window.

use crate::prefs::SearchPrefs;
use eframe::egui::{self, RichText};
use nerka_core::input::capture::route::{Match, Route};
use nerka_core::input::proto::ksy::KsyProtocol;
use nerka_core::input::proto::{LoadedProtocol, Protocol};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// The native protocols, as rules and the Off list name them.
pub const NATIVE: [&str; 3] = ["DLT", "SOMEIP", "syslog"];

/// Where definitions are kept.
pub fn folder() -> Option<PathBuf> {
    nerka_core::config_dir().map(|d| d.join("protocols"))
}

/// A definition file: read, or why not.
pub struct DefFile {
    pub path: PathBuf,
    pub loaded: Result<LoadedProtocol, String>,
}

/// The `.ksy` files of `dir`, by file name. A definition named like one
/// before it (or like a native protocol) is refused.
pub fn load(dir: &Path) -> Vec<DefFile> {
    let mut paths: Vec<PathBuf> = std::fs::read_dir(dir)
        .map(|rd| {
            rd.filter_map(|e| e.ok().map(|e| e.path()))
                .filter(|p| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("ksy")))
                .collect()
        })
        .unwrap_or_default();
    paths.sort();
    let mut out: Vec<DefFile> = Vec::new();
    for path in paths {
        let loaded = KsyProtocol::load(&path).map_err(|e| short_error(&e.to_string(), &path)).and_then(|p| {
            let taken =
                out.iter().any(|d| d.loaded.as_ref().is_ok_and(|l| l.protocol.name().eq_ignore_ascii_case(p.name())));
            if taken {
                return Err(format!("a protocol named {} is loaded already", p.name()));
            }
            Ok(LoadedProtocol::new(Arc::new(p)))
        });
        out.push(DefFile { path, loaded });
    }
    out
}

/// An error without the file's path at its start (the row shows the file).
fn short_error(e: &str, path: &Path) -> String {
    e.strip_prefix(&format!("{}: ", path.display())).unwrap_or(e).to_owned()
}

/// Copy the definition `src` into `dir` (checked first). Its new path.
pub fn add(src: &Path, dir: &Path, loaded: &[DefFile]) -> Result<PathBuf, String> {
    let p = KsyProtocol::load(src).map_err(|e| e.to_string())?;
    let name = p.name();
    if loaded.iter().any(|d| d.loaded.as_ref().is_ok_and(|l| l.protocol.name().eq_ignore_ascii_case(name))) {
        return Err(format!("a protocol named {name} is loaded already"));
    }
    let file = src.file_name().ok_or("no file name")?;
    let dest = dir.join(file);
    if dest.exists() {
        return Err(format!("{} is in the protocols folder already", Path::new(file).display()));
    }
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let text = std::fs::read(src).map_err(|e| e.to_string())?;
    nerka_core::write_atomic(&dest, text).map_err(|e| e.to_string())?;
    Ok(dest)
}

/// Every protocol captures can be read with: the native ones, then the
/// loaded definitions.
pub fn names(prefs: &SearchPrefs) -> Vec<String> {
    NATIVE.iter().map(|n| n.to_string()).chain(prefs.protocols.iter().map(|l| l.protocol.name().to_owned())).collect()
}

/// The rules a protocol has now (the user's, else its defaults), and
/// whether they are the user's.
fn rules_of(prefs: &SearchPrefs, name: &str) -> (Vec<Match>, bool) {
    let user: Vec<Match> = prefs
        .routes
        .iter()
        .filter_map(|r| r.parse::<Route>().ok())
        .filter(|r| r.protocol.eq_ignore_ascii_case(name))
        .map(|r| r.rule)
        .collect();
    if !user.is_empty() {
        return (user, true);
    }
    (defaults(prefs, name), false)
}

/// A protocol's default rules: a native one's ports, a definition's own.
fn defaults(prefs: &SearchPrefs, name: &str) -> Vec<Match> {
    let ports = match name {
        "DLT" => Some(&prefs.dlt_ports),
        "SOMEIP" => Some(&prefs.someip_ports),
        "syslog" => Some(&prefs.syslog_ports),
        _ => None,
    };
    match ports {
        Some(p) => p.iter().map(|&p| Match::port(p)).collect(),
        None => prefs
            .protocols
            .iter()
            .find(|l| l.protocol.name().eq_ignore_ascii_case(name))
            .map(|l| l.routes.clone())
            .unwrap_or_default(),
    }
}

/// Rules in a few words, for a collapsed row.
fn summary(rules: &[Match]) -> String {
    if rules.is_empty() {
        return "no datagrams".into();
    }
    rules.iter().map(ToString::to_string).collect::<Vec<_>>().join("; ")
}

/// One rule as edited: a field per condition, empty for any.
#[derive(Debug, Clone, Default, PartialEq)]
struct RuleRow {
    /// Destination port, source port, source IP, destination IP, VLAN.
    fields: [String; 5],
}

const KEYS: [&str; 5] = ["port", "sport", "src", "dst", "vlan"];
const HEADS: [(&str, &str); 5] = [
    ("Port", "UDP destination port or range: 3490, 30000-30100"),
    ("Source port", "UDP source port or range"),
    ("Source IP", "An address or a network: 10.0.0.7, 10.0.0.0/8, fe80::1"),
    ("Destination IP", "An address or a network: 10.0.0.2, 192.168.0.0/16"),
    ("VLAN", "A VLAN id the datagram is tagged with: 1 to 4094"),
];

impl RuleRow {
    fn of(m: &Match) -> RuleRow {
        let range = |r: Option<(u16, u16)>| {
            r.map_or(String::new(), |(a, b)| if a == b { a.to_string() } else { format!("{a}-{b}") })
        };
        RuleRow {
            fields: [
                range(m.port),
                range(m.sport),
                m.src.map_or(String::new(), |p| p.to_string()),
                m.dst.map_or(String::new(), |p| p.to_string()),
                m.vlan.map_or(String::new(), |v| v.to_string()),
            ],
        }
    }

    /// The rule, or the first field that is wrong and why.
    fn rule(&self) -> Result<Match, (usize, String)> {
        let mut m = Match::default();
        for (i, (k, v)) in KEYS.iter().zip(&self.fields).enumerate() {
            if !v.trim().is_empty() {
                m.set(k, v).map_err(|e| (i, e))?;
            }
        }
        Ok(m)
    }
}

/// The Settings window's protocol rules while it is open: per protocol,
/// whether its rules are shown and the rows as typed.
#[derive(Default)]
pub struct Edit {
    open: Vec<(String, Vec<RuleRow>)>,
}

/// What the Protocols section asks for.
#[derive(Default)]
pub struct Change {
    /// Protocol turned on or off.
    pub enabled: Vec<(String, bool)>,
    /// New valid rules for a protocol (empty: a definition's own).
    pub rules: Vec<(String, Vec<Match>)>,
    pub add: bool,
    pub remove: Option<PathBuf>,
    pub reload: bool,
}

/// The Protocols section of the Settings window.
pub fn settings_section(ui: &mut egui::Ui, edit: &mut Edit, prefs: &SearchPrefs, defs: &[DefFile]) -> Change {
    let mut change = Change::default();
    ui.label(RichText::new("Captures: protocols").strong()).on_hover_text(
        "Which protocols are read in captures (pcap, pcapng, MF4), and which UDP datagrams are each one's. \
         Used for captures opened from now on; a tab's Files… window turns protocols off per capture.",
    );
    ui.weak("Ticked protocols are read. Rules ▸ shows which datagrams are a protocol's: by port, source, destination, VLAN.");
    let mut rows: Vec<(String, Option<&DefFile>)> = NATIVE.iter().map(|n| (n.to_string(), None)).collect();
    for d in defs {
        let name = match &d.loaded {
            Ok(l) => l.protocol.name().to_owned(),
            Err(_) => d.path.file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default(),
        };
        rows.push((name, Some(d)));
    }
    for (name, def) in &rows {
        if let Some(DefFile { loaded: Err(e), path }) = def {
            ui.horizontal(|ui| {
                ui.add_space(24.0);
                ui.add_sized([90.0, 18.0], egui::Label::new(name.as_str()).truncate())
                    .on_hover_text(path.display().to_string());
                ui.colored_label(ui.visuals().error_fg_color, e);
                if ui.small_button("✖").on_hover_text("Remove this file from the protocols folder").clicked() {
                    change.remove = Some(path.clone());
                }
            });
            continue;
        }
        let (now, own) = rules_of(prefs, name);
        let slot = edit.open.iter().position(|(n, _)| n == name);
        ui.horizontal(|ui| {
            let off = prefs.disabled_protocols.iter().any(|d| d.eq_ignore_ascii_case(name));
            let mut on = !off;
            if ui.checkbox(&mut on, "").on_hover_text("Read this protocol in captures").changed() {
                change.enabled.push((name.clone(), on));
            }
            let label = ui.add_sized([90.0, 18.0], egui::Label::new(RichText::new(name.as_str()).strong()).truncate());
            if let Some(d) = def {
                label.on_hover_text(d.path.display().to_string());
            }
            let shown = slot.is_some();
            if ui
                .selectable_label(shown, if shown { "Rules ▾" } else { "Rules ▸" })
                .on_hover_text("Show and edit the rules: which datagrams are this protocol's")
                .clicked()
            {
                match slot {
                    Some(i) => {
                        edit.open.remove(i);
                    }
                    None => edit.open.push((name.clone(), now.iter().map(RuleRow::of).collect())),
                }
            }
            let text = summary(&now);
            ui.label(RichText::new(text).monospace()).on_hover_text(if own { "Your rules" } else { "Default rules" });
            if !own && def.is_some() {
                ui.weak("(its own)");
            }
            if let Some(d) = def
                && ui.small_button("✖").on_hover_text("Remove this definition from the protocols folder").clicked()
            {
                change.remove = Some(d.path.clone());
            }
        });
        let Some(slot) = edit.open.iter().position(|(n, _)| n == name) else { continue };
        let rows = &mut edit.open[slot].1;
        ui.indent(("rules", name), |ui| {
            let mut changed = false;
            let mut problem = None;
            egui::Grid::new(("rules-grid", name)).num_columns(6).spacing([6.0, 4.0]).show(ui, |ui| {
                for (head, tip) in HEADS {
                    ui.label(RichText::new(head).small()).on_hover_text(tip);
                }
                ui.label("");
                ui.end_row();
                let mut removed = None;
                for (r, row) in rows.iter_mut().enumerate() {
                    let wrong = row.rule().err();
                    for (i, (field, (_, tip))) in row.fields.iter_mut().zip(HEADS).enumerate() {
                        let width = if (2..=3).contains(&i) { 130.0 } else { 80.0 };
                        let bad = wrong.as_ref().is_some_and(|(at, _)| *at == i);
                        let mut te = egui::TextEdit::singleline(field).hint_text("any").desired_width(width);
                        if bad {
                            te = te.text_color(ui.visuals().error_fg_color);
                        }
                        // Sized, not desired: a grid cell would squeeze it
                        // (a port of five digits showed as `410(`).
                        let size = egui::vec2(width, ui.spacing().interact_size.y);
                        changed |= ui.add_sized(size, te).on_hover_text(tip).changed();
                    }
                    if ui.small_button("✖").on_hover_text("Remove this rule").clicked() {
                        removed = Some(r);
                    }
                    ui.end_row();
                    if problem.is_none() {
                        problem = wrong.map(|(_, e)| e);
                    }
                }
                if let Some(r) = removed {
                    rows.remove(r);
                    changed = true;
                }
            });
            ui.horizontal(|ui| {
                if ui
                    .button("+ Rule")
                    .on_hover_text("Another rule: a datagram is this protocol's if any rule matches")
                    .clicked()
                {
                    rows.push(RuleRow::default());
                }
                if def.is_some()
                    && ui.button("Its own rules").on_hover_text("The rules the definition gives itself").clicked()
                {
                    change.rules.push((name.clone(), Vec::new()));
                    *rows = defaults(prefs, name).iter().map(RuleRow::of).collect();
                }
                match &problem {
                    Some(e) => {
                        ui.colored_label(ui.visuals().error_fg_color, e);
                    }
                    None if rows.is_empty() && def.is_none() => {
                        ui.colored_label(ui.visuals().error_fg_color, "At least one rule (or untick it)");
                    }
                    None => {
                        ui.weak("Empty fields match anything; the most specific matching rule wins.");
                    }
                }
            });
            if changed
                && problem.is_none()
                && !(rows.is_empty() && def.is_none())
                && let Ok(rules) = rows.iter().map(RuleRow::rule).collect::<Result<Vec<_>, _>>()
            {
                change.rules.push((name.clone(), rules));
            }
        });
    }
    ui.horizontal(|ui| {
        change.add = ui
            .button("Add definition…")
            .on_hover_text("A protocol described in a .ksy file (Kaitai Struct with a -nerka: section); copied into the protocols folder")
            .clicked();
        change.reload = ui.button("Reload").on_hover_text("Read the protocols folder again").clicked();
        if let Some(dir) = folder() {
            ui.weak(dir.display().to_string());
        }
    });
    change
}

/// Apply a [`Change`] of rules or on/off to the prefs.
pub fn apply(prefs: &mut SearchPrefs, change: &Change) -> bool {
    let mut changed = false;
    for (name, on) in &change.enabled {
        prefs.disabled_protocols.retain(|d| !d.eq_ignore_ascii_case(name));
        if !on {
            prefs.disabled_protocols.push(name.clone());
        }
        changed = true;
    }
    for (name, rules) in &change.rules {
        let ports_only: Option<Vec<u16>> =
            rules.iter().map(|r| r.to_string().parse::<u16>().ok()).collect::<Option<Vec<u16>>>();
        let theirs = |r: &String| r.parse::<Route>().is_ok_and(|r| r.protocol.eq_ignore_ascii_case(name));
        prefs.routes.retain(|r| !theirs(r));
        match (name.as_str(), ports_only) {
            // A native protocol on ports only: its port list.
            ("DLT", Some(p)) if !p.is_empty() => prefs.dlt_ports = p,
            ("SOMEIP", Some(p)) if !p.is_empty() => prefs.someip_ports = p,
            ("syslog", Some(p)) if !p.is_empty() => prefs.syslog_ports = p,
            _ => {
                // A definition's own rules again: nothing to keep.
                if !rules.is_empty() && *rules != defaults(prefs, name) {
                    prefs.routes.extend(rules.iter().map(|r| format!("{name}:{r}")));
                }
            }
        }
        changed = true;
    }
    changed
}

/// A capture's protocols in a tab's Files window: a toggle per protocol;
/// `off` lists those turned off. Whether it changed.
pub fn file_toggles(ui: &mut egui::Ui, names: &[String], off: &mut Vec<String>) -> bool {
    let mut changed = false;
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        for n in names {
            let on = !off.iter().any(|o| o.eq_ignore_ascii_case(n));
            let r = ui.selectable_label(on, n).on_hover_text(if on {
                "Read in this capture; click to leave it out"
            } else {
                "Left out of this capture; click to read it"
            });
            if r.clicked() {
                if on {
                    off.push(n.clone());
                } else {
                    off.retain(|o| !o.eq_ignore_ascii_case(n));
                }
                changed = true;
            }
        }
    });
    changed
}

/// Whether `path` is a capture (by its first bytes).
pub fn is_capture(path: &Path) -> bool {
    let mut head = [0u8; 16];
    std::fs::File::open(path)
        .and_then(|mut f| std::io::Read::read(&mut f, &mut head))
        .is_ok_and(|n| nerka_core::input::capture::is_capture(&head[..n]))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("nerka-protocols-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    const DLT_KSY: &str = include_str!("../../examples/protocols/dlt.ksy");

    #[test]
    fn the_folder_reads_definitions_and_says_what_is_wrong() {
        let d = dir("load");
        std::fs::write(d.join("a.ksy"), DLT_KSY).unwrap();
        std::fs::write(d.join("b.ksy"), DLT_KSY).unwrap();
        std::fs::write(d.join("c.ksy"), "meta: { id: x }\n").unwrap();
        std::fs::write(d.join("notes.txt"), "not a definition").unwrap();
        let defs = load(&d);
        assert_eq!(defs.len(), 3);
        assert_eq!(defs[0].loaded.as_ref().map(|l| l.protocol.name().to_owned()).ok().as_deref(), Some("DLTK"));
        assert_eq!(defs[1].loaded.as_ref().err().map(String::as_str), Some("a protocol named DLTK is loaded already"));
        let c = defs[2].loaded.as_ref().err().cloned().unwrap_or_default();
        assert!(c.contains("-nerka") && !c.contains("c.ksy"), "the file's own error, without its path: {c}");
        let other = dir("load-src");
        std::fs::write(other.join("new.ksy"), DLT_KSY).unwrap();
        assert_eq!(add(&other.join("new.ksy"), &d, &defs).unwrap_err(), "a protocol named DLTK is loaded already");
        let fresh = dir("load-empty");
        let dest = add(&other.join("new.ksy"), &fresh, &[]).unwrap();
        assert_eq!(std::fs::read_to_string(dest).unwrap(), DLT_KSY);
        assert!(add(&other.join("new.ksy"), &fresh, &[]).unwrap_err().contains("in the protocols folder already"));
    }

    fn prefs_with_dltk() -> SearchPrefs {
        let p = Arc::new(KsyProtocol::parse(DLT_KSY).unwrap());
        let mut loaded = LoadedProtocol::new(p);
        loaded.routes = vec![Match::port(3491)];
        SearchPrefs { protocols: vec![loaded], ..SearchPrefs::default() }
    }

    fn rules(t: &str) -> Vec<Match> {
        t.split(';').map(str::trim).filter(|r| !r.is_empty()).map(|r| r.parse().unwrap()).collect()
    }

    #[test]
    fn rules_edited_in_settings() {
        let mut prefs = prefs_with_dltk();
        let change = |name: &str, t: &str| Change { rules: vec![(name.into(), rules(t))], ..Change::default() };
        // A native protocol on ports only: its port list.
        assert!(apply(&mut prefs, &change("DLT", "3490; 3500")));
        assert_eq!((prefs.dlt_ports.clone(), prefs.routes.clone()), (vec![3490, 3500], vec![]));
        // With conditions: rules of its own.
        apply(&mut prefs, &change("DLT", "port=3490,vlan=5; 3500"));
        assert_eq!(prefs.routes, ["DLT:port=3490,vlan=5", "DLT:3500"]);
        assert_eq!(rules_of(&prefs, "DLT"), (rules("port=3490,vlan=5; 3500"), true));
        // A definition: its own rules again (or none) keep nothing.
        apply(&mut prefs, &change("DLTK", "40000"));
        assert!(prefs.routes.contains(&"DLTK:40000".to_owned()));
        apply(&mut prefs, &change("DLTK", "3491"));
        assert!(!prefs.routes.iter().any(|r| r.starts_with("DLTK")));
        apply(&mut prefs, &change("DLTK", ""));
        assert_eq!(rules_of(&prefs, "DLTK"), (vec![Match::port(3491)], false));
        // On and off.
        apply(&mut prefs, &Change { enabled: vec![("syslog".into(), false)], ..Change::default() });
        apply(&mut prefs, &Change { enabled: vec![("SYSLOG".into(), false)], ..Change::default() });
        assert_eq!(prefs.disabled_protocols, ["SYSLOG"]);
        apply(&mut prefs, &Change { enabled: vec![("syslog".into(), true)], ..Change::default() });
        assert!(prefs.disabled_protocols.is_empty());
    }

    #[test]
    fn a_rule_row_per_condition() {
        let m: Match = "port=30000-30100,src=10.0.0.0/8,vlan=5".parse().unwrap();
        let row = RuleRow::of(&m);
        assert_eq!(row.fields, ["30000-30100", "", "10.0.0.0/8", "", "5"].map(str::to_owned));
        assert_eq!(row.rule(), Ok(m));
        let mut bad = row.clone();
        bad.fields[3] = "10.0.0.300".into();
        assert_eq!(bad.rule(), Err((3, "`10.0.0.300` isn't an IP address".into())));
        assert_eq!(RuleRow::default().rule().map(|m| m.to_string()), Ok("*".into()), "all empty: every datagram");
    }

    #[test]
    fn a_capture_turns_protocols_off() {
        let ctx = egui::Context::default();
        let names = names(&prefs_with_dltk());
        assert_eq!(names, ["DLT", "SOMEIP", "syslog", "DLTK"]);
        let mut off = vec!["dlt".to_owned()];
        let mut out = ctx.run_ui(Default::default(), |ui| {
            assert!(!file_toggles(ui, &names, &mut off));
        });
        out.textures_delta.clear();
        assert_eq!(off, ["dlt"]);
        let d = dir("capture");
        let pcap = d.join("a.pcap");
        let mut head = 0xa1b2_c3d4u32.to_le_bytes().to_vec();
        head.extend_from_slice(&[2, 0, 4, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 4, 0, 1, 0, 0, 0]);
        std::fs::write(&pcap, &head).unwrap();
        std::fs::write(d.join("b.log"), "x\n").unwrap();
        assert!(is_capture(&pcap) && !is_capture(&d.join("b.log")));
    }

    #[test]
    fn the_section_draws_and_opens_its_rules() {
        let ctx = egui::Context::default();
        let prefs = prefs_with_dltk();
        let defs = vec![DefFile { path: "x.ksy".into(), loaded: Err("line 1, column 1: broken".into()) }];
        let mut edit = Edit::default();
        edit.open.push(("DLT".into(), vec![RuleRow::of(&Match::port(3490))]));
        for _ in 0..2 {
            let mut out = ctx.run_ui(Default::default(), |ui| {
                let c = settings_section(ui, &mut edit, &prefs, &defs);
                assert!(c.rules.is_empty() && c.enabled.is_empty());
            });
            out.textures_delta.clear();
        }
        assert_eq!(edit.open.len(), 1);
    }
}
