//! Compare (experimental; `docs/dev/patterns.md`, round α2), in the
//! Patterns window: two sets of lines grouped into patterns together, so a
//! pattern is the same on both sides, and what differs between them: patterns
//! and slot values new in B, gone from B, then more or less often, surest
//! first (`nerka_core::pattern::differences`, as `nerka compare`).
//!
//! A side is all lines, the filter result, a typed filter, lifecycles of a
//! tab (picked by number, or every one of an outcome: owner, 2026-11-23,
//! groups in the first round), or another tab. **Only** and **Ignore**
//! filters choose the content of both (owner, 2026-11-23); rows can be
//! ignored from the results, and a pattern saved as an `ignore` rule.
//! Nothing runs until ▶ Compare.

use crate::job::{Job, Poll};
use crate::patterns::{Input, Output, TabRef, template_job};
use eframe::egui::{self, RichText};
use nerka_core::lifecycle::{Lifecycles, Outcome};
use nerka_core::pattern::{self, Change, Difference, PatternConfig, PatternOptions, Patterns};
use nerka_core::{Filter, LineNo, LogSource, SearchResult};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

/// Rows listed at first; "Show more" adds as many.
const PAGE: usize = 300;
/// The score a change needs at first (G², p = 0.001).
const MIN_SCORE: f64 = 10.8;
const OUTCOMES: [Outcome; 5] = [Outcome::Planned, Outcome::Crash, Outcome::Unclean, Outcome::Lost, Outcome::Open];

/// Where a side's lines come from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SideFrom {
    All,
    Filter,
    #[default]
    Query,
    /// Lifecycles of this tab.
    Lifecycles,
    /// Another tab.
    Tab,
}

/// Which lifecycles of a `@name`.
#[derive(Debug, Clone, PartialEq)]
pub enum Pick {
    /// `2`, `2, 5`, `3-7`.
    Numbers(String),
    /// Every lifecycle that ended so.
    Outcome(Outcome),
}

/// One side of a comparison.
#[derive(Debug, Clone, PartialEq)]
pub struct Side {
    pub from: SideFrom,
    pub query: String,
    /// The lifecycles' `@name`, and which of them.
    pub life: String,
    pub pick: Pick,
    /// Another tab (its id), and only its filter result.
    pub tab: u64,
    pub filtered: bool,
}

impl Default for Side {
    fn default() -> Self {
        Side {
            from: SideFrom::Query,
            query: String::new(),
            life: String::new(),
            pick: Pick::Numbers(String::new()),
            tab: 0,
            filtered: false,
        }
    }
}

/// A side made ready to run: its lines' source and what keeps them.
struct Prepared {
    file: Arc<dyn LogSource>,
    base: Base,
    /// Only and Ignore, compiled for this side's file.
    content: Option<Filter>,
    /// Lifecycles: theirs, and how many were picked.
    lives: Option<(Arc<Lifecycles>, usize)>,
    tab: u64,
    what: String,
}

enum Base {
    All,
    Kept(Arc<SearchResult>),
    Search(Filter),
}

/// A row of the results: a pattern (its fields and template), or one of
/// its slots' values. Templates stay from run to run, so an ignored row
/// stays ignored.
#[derive(Debug, Clone, PartialEq, Eq)]
struct RowKey {
    pattern: String,
    value: Option<(usize, String)>,
}

/// A finished comparison.
struct Compared {
    ps: Patterns,
    diffs: Vec<Difference>,
    /// Per side with lifecycles: how many were picked, and per pattern in
    /// how many of them it has lines.
    presence: [Option<(usize, Vec<usize>)>; 2],
    /// Per side: its tab, its file (the `Arc`'s address), its lines in words.
    tabs: [u64; 2],
    files: [usize; 2],
    what: [String; 2],
    config: Option<(SystemTime, u64)>,
    took: Duration,
}

/// The Compare part of a tab's Patterns window.
pub struct CompareUi {
    pub sides: [Side; 2],
    only: String,
    ignore: String,
    min_score: f64,
    job: Option<(Job<Compared>, [String; 2])>,
    run: Option<Compared>,
    error: Option<String>,
    /// What saving a rule did.
    note: Option<String>,
    ignored: Vec<RowKey>,
    show_ignored: bool,
    shown: usize,
    /// Run once the window shows (asked for from the Signals panel).
    pub pending: bool,
}

impl Default for CompareUi {
    fn default() -> Self {
        CompareUi {
            sides: Default::default(),
            only: String::new(),
            ignore: String::new(),
            min_score: MIN_SCORE,
            job: None,
            run: None,
            error: None,
            note: None,
            ignored: Vec::new(),
            show_ignored: false,
            shown: PAGE,
            pending: false,
        }
    }
}

/// `2, 5, 7-9` as lifecycle numbers (1 to `max`).
fn parse_numbers(text: &str, max: usize) -> Result<Vec<u32>, String> {
    let mut out = Vec::new();
    for part in text.split([',', ' ']).map(str::trim).filter(|p| !p.is_empty()) {
        let number = |s: &str| -> Result<u32, String> {
            match s.trim().parse::<u32>() {
                Ok(n) if n >= 1 && n as usize <= max => Ok(n),
                _ => Err(format!("'{s}' isn't a lifecycle's number (1 to {max})")),
            }
        };
        match part.split_once('-') {
            Some((a, b)) => {
                let (a, b) = (number(a)?, number(b)?);
                if a > b {
                    return Err(format!("'{part}': from more to less"));
                }
                out.extend(a..=b);
            }
            None => out.push(number(part)?),
        }
    }
    out.sort_unstable();
    out.dedup();
    if out.is_empty() {
        return Err("no lifecycles: give their numbers, e.g. 2, 5-7".into());
    }
    Ok(out)
}

/// Only and Ignore as one NFD (`(only) AND NOT (ignore)`), if either.
pub(crate) fn content_nfd(only: &str, ignore: &str) -> Option<String> {
    let (only, ignore) = (only.trim(), ignore.trim());
    match (only.is_empty(), ignore.is_empty()) {
        (true, true) => None,
        (false, true) => Some(format!("({only})")),
        (true, false) => Some(format!("NOT ({ignore})")),
        (false, false) => Some(format!("({only}) AND NOT ({ignore})")),
    }
}

/// Lines of both, ascending.
pub(crate) fn intersect(a: &[LineNo], b: &[LineNo]) -> Vec<LineNo> {
    let (mut i, mut j, mut out) = (0, 0, Vec::new());
    while i < a.len() && j < b.len() {
        match a[i].cmp(&b[j]) {
            std::cmp::Ordering::Less => i += 1,
            std::cmp::Ordering::Greater => j += 1,
            std::cmp::Ordering::Equal => {
                out.push(a[i]);
                i += 1;
                j += 1;
            }
        }
    }
    out
}

/// A side's lines (`None`: all).
fn lines_of(p: &Prepared, progress: &nerka_core::Progress) -> nerka_core::Result<Option<Vec<LineNo>>> {
    let base = match &p.base {
        Base::All => None,
        Base::Kept(r) => Some(r.lines.clone()),
        Base::Search(f) => Some(nerka_core::search(&*p.file, f, progress)?.lines),
    };
    Ok(match &p.content {
        None => base,
        Some(f) => {
            let kept = nerka_core::search(&*p.file, f, progress)?.lines;
            Some(match base {
                None => kept,
                Some(b) => intersect(&b, &kept),
            })
        }
    })
}

/// In how many lifecycles `lines` (ascending) are.
fn lives_of(lives: &Lifecycles, lines: &[LineNo]) -> usize {
    let mut last = None;
    let mut n = 0;
    for &l in lines {
        let i = lives.at(l).map(|(i, _)| i);
        if i.is_some() && i != last {
            n += 1;
            last = i;
        }
    }
    n
}

impl CompareUi {
    pub fn cancel(&mut self) {
        if let Some((j, _)) = self.job.take() {
            j.cancel();
        }
    }

    pub fn is_busy(&self) -> bool {
        self.job.is_some()
    }

    /// Lifecycles `a` and `b` of `@name` (the Signals panel's "compare with
    /// the previous one"), run once shown.
    pub fn lifecycles(&mut self, name: &str, a: u32, b: u32) {
        for (side, n) in self.sides.iter_mut().zip([a, b]) {
            *side = Side {
                from: SideFrom::Lifecycles,
                life: name.to_owned(),
                pick: Pick::Numbers(n.to_string()),
                ..Side::default()
            };
        }
        self.pending = true;
    }

    /// The tab a side reads.
    fn tab_of<'a>(&self, side: &Side, input: &'a Input) -> Result<&'a TabRef, String> {
        match side.from {
            SideFrom::Tab => input.tabs.iter().find(|t| t.id == side.tab).ok_or_else(|| "pick a tab".to_owned()),
            _ => Ok(input.tab),
        }
    }

    fn prepare(&self, side: &Side, input: &Input) -> Result<Prepared, String> {
        let tab = self.tab_of(side, input)?;
        let (base, what, lives) = match side.from {
            SideFrom::All => (Base::All, "all lines".to_owned(), None),
            SideFrom::Filter => match &tab.kept {
                Some(r) => (Base::Kept(r.clone()), "the filter result".to_owned(), None),
                None => (Base::All, "all lines (no filter)".to_owned(), None),
            },
            SideFrom::Query => {
                let q = side.query.trim();
                if q.is_empty() {
                    return Err("type a filter, e.g. @ecu1 == 2".into());
                }
                (Base::Search(tab.compile(q)?), q.to_owned(), None)
            }
            SideFrom::Lifecycles => {
                let l = tab
                    .lifecycles
                    .iter()
                    .find(|l| l.name == side.life)
                    .ok_or_else(|| format!("no lifecycles @{} found (yet)", side.life))?;
                let (numbers, what) = match &side.pick {
                    Pick::Numbers(t) => {
                        let n = parse_numbers(t, l.found.lives.len())?;
                        let what = match n.as_slice() {
                            [one] => format!("@{} #{one}", l.name),
                            _ => format!("@{} #{} ({})", l.name, t.trim(), n.len()),
                        };
                        (n, what)
                    }
                    Pick::Outcome(o) => {
                        let n: Vec<u32> = l.found.lives.iter().filter(|x| x.outcome == *o).map(|x| x.number).collect();
                        if n.is_empty() {
                            return Err(format!("no lifecycle @{} ended {}", l.name, o.as_str()));
                        }
                        let what = format!("@{} every {} ({})", l.name, o.as_str(), n.len());
                        (n, what)
                    }
                };
                let nfd = tab.lifecycle_nfd(l, &numbers);
                (Base::Search(tab.compile(&nfd)?), what, Some((l.found.clone(), numbers.len())))
            }
            SideFrom::Tab => {
                let what = match (&tab.kept, side.filtered) {
                    (Some(_), true) => format!("{}, its filter result", tab.title),
                    _ => tab.title.clone(),
                };
                let base = match (&tab.kept, side.filtered) {
                    (Some(r), true) => Base::Kept(r.clone()),
                    _ => Base::All,
                };
                (base, what, None)
            }
        };
        let content = content_nfd(&self.only, &self.ignore).map(|q| tab.compile(&q)).transpose()?;
        let what = if content.is_some() { format!("{what}, content chosen") } else { what };
        Ok(Prepared { file: tab.file.clone(), base, content, lives, tab: tab.id, what })
    }

    /// ▶ Compare.
    fn start(&mut self, ctx: &egui::Context, input: &Input, config: Option<(SystemTime, u64)>) {
        self.cancel();
        self.error = None;
        self.note = None;
        let options = match (input.config)() {
            Ok(config) => PatternOptions { lines: true, config, ..Default::default() },
            Err(e) => {
                self.error = Some(format!("Pattern configuration: {e}"));
                return;
            }
        };
        let mut prepared = Vec::new();
        for (k, side) in self.sides.iter().enumerate() {
            match self.prepare(side, input) {
                Ok(p) => prepared.push(p),
                Err(e) => {
                    self.error = Some(format!("{}: {e}", ["A", "B"][k]));
                    return;
                }
            }
        }
        let [a, b]: [Prepared; 2] = prepared.try_into().ok().expect("two sides");
        let what = [a.what.clone(), b.what.clone()];
        let job = Job::spawn(ctx, move |progress| {
            let started = std::time::Instant::now();
            let lines = [lines_of(&a, progress)?, lines_of(&b, progress)?];
            let sa = pattern::Set { source: &*a.file, lines: lines[0].as_deref() };
            let sb = pattern::Set { source: &*b.file, lines: lines[1].as_deref() };
            let ps = pattern::compare(sa, sb, &options, progress)?;
            let diffs = pattern::differences(&ps, 0.0);
            let presence = [&a, &b].map(|p| p.lives.as_ref());
            let presence = [0, 1].map(|s| {
                presence[s].map(|(lives, n)| (*n, ps.patterns.iter().map(|p| lives_of(lives, &p.lines[s])).collect()))
            });
            Ok(Compared {
                diffs,
                presence,
                tabs: [a.tab, b.tab],
                files: [Arc::as_ptr(&a.file) as *const u8 as usize, Arc::as_ptr(&b.file) as *const u8 as usize],
                what: [a.what.clone(), b.what.clone()],
                config,
                took: started.elapsed(),
                ps,
            })
        });
        self.job = Some((job, what));
    }

    pub fn poll(&mut self) {
        let Some((job, _)) = &self.job else { return };
        let Poll::Done(r, _) = job.poll() else { return };
        self.job = None;
        match r {
            Ok(c) => {
                self.shown = PAGE;
                self.run = Some(c);
            }
            Err(nerka_core::Error::Cancelled) => {}
            Err(e) => self.error = Some(e.to_string()),
        }
    }

    /// The Compare part of the window.
    pub fn ui(&mut self, ui: &mut egui::Ui, input: &Input, config: Option<(SystemTime, u64)>, out: &mut Output) {
        if std::mem::take(&mut self.pending) {
            self.start(ui.ctx(), input, config);
        }
        egui::Grid::new("compare-sides").num_columns(2).show(ui, |ui| {
            for k in 0..2 {
                ui.label(RichText::new(["A:", "B:"][k]).strong());
                ui.horizontal(|ui| side_ui(ui, ("side", k), &mut self.sides[k], input));
                ui.end_row();
            }
            ui.label("Only:").on_hover_text("Both sides: only the lines this NFD filter keeps");
            ui.horizontal(|ui| {
                ui.add(
                    egui::TextEdit::singleline(&mut self.only)
                        .hint_text("NFD, e.g. ecu == ECU1 AND level >= WARN")
                        .desired_width(300.0),
                );
                ui.label("Ignore:").on_hover_text(
                    "Both sides: leave out the lines this NFD filter keeps (periodic or noisy messages \
                     that differ every boot and say nothing)",
                );
                ui.add(
                    egui::TextEdit::singleline(&mut self.ignore)
                        .hint_text("NFD, e.g. apid == GPS OR payload ~ /heartbeat/")
                        .desired_width(300.0),
                );
            });
            ui.end_row();
        });
        ui.horizontal(|ui| {
            if let Some((job, what)) = &self.job {
                if ui.button("■ Cancel").clicked() {
                    job.cancel();
                }
                ui.add(egui::ProgressBar::new(job.progress.fraction()).desired_width(160.0).show_percentage());
                ui.weak(format!("comparing {} with {}…", what[0], what[1]));
            } else if ui
                .button("▶ Compare")
                .on_hover_text("Group both sides' lines into patterns together and list what differs")
                .clicked()
            {
                self.start(ui.ctx(), input, config);
            }
            ui.label("Changes scoring")
                .on_hover_text("G²: 3.84 is p = 0.05, 10.8 is p = 0.001. New and gone ones always show.");
            ui.add(egui::DragValue::new(&mut self.min_score).range(0.0..=10_000.0).speed(0.1).max_decimals(1));
            ui.label("or more");
        });
        if let Some(r) = &self.run {
            let now_files = [0, 1].map(|s| {
                input.tabs.iter().find(|t| t.id == r.tabs[s]).map(|t| Arc::as_ptr(&t.file) as *const u8 as usize)
            });
            let why = if now_files.iter().zip(&r.files).any(|(n, f)| *n != Some(*f)) {
                Some("a file was read again or its tab closed")
            } else if r.config != config {
                Some("the pattern configuration changed")
            } else {
                None
            };
            if let Some(why) = why {
                ui.colored_label(ui.visuals().warn_fg_color, format!("Out of date: {why}. ▶ Compare runs again."));
            }
        }
        if let Some(e) = &self.error {
            ui.colored_label(ui.visuals().error_fg_color, e);
        }
        if let Some(n) = &self.note {
            ui.label(n);
        }
        ui.separator();
        if self.run.is_some() {
            self.results(ui, input, out);
        } else if self.job.is_none() {
            ui.weak(
                "Compare two sets of lines: two lifecycles (or groups of them: every crash against every \
                 planned), the filter result against a filter, or two tabs. Both are grouped into patterns \
                 together; then the patterns and slot values new in B, gone from B, and those more or less \
                 often, surest first. Nothing runs until ▶ Compare.",
            );
        }
    }

    fn results(&mut self, ui: &mut egui::Ui, input: &Input, out: &mut Output) {
        let Some(run) = &self.run else { return };
        let ps = &run.ps;
        let rows: Vec<(&Difference, RowKey)> = run
            .diffs
            .iter()
            .filter(|d| d.change() != Change::Changed || d.score >= self.min_score)
            .map(|d| (d, row_key(ps, d)))
            .collect();
        let hidden = rows.iter().filter(|(_, k)| self.ignored.contains(k)).count();
        ui.horizontal(|ui| {
            ui.label(format!(
                "A: {} ({} lines) · B: {} ({} lines) · {} patterns, {:.1?}",
                run.what[0],
                ps.lines[0],
                run.what[1],
                ps.lines[1],
                ps.patterns.len(),
                run.took
            ));
            if hidden > 0 || self.show_ignored {
                ui.separator();
                ui.weak(format!("{hidden} ignored"));
                ui.checkbox(&mut self.show_ignored, "Show ignored");
            }
        });
        let (state, weak) = (ui.visuals().hyperlink_color, ui.visuals().weak_text_color());
        let text = ui.visuals().text_color();
        let mut ignore = None;
        let mut unignore = None;
        let mut show_more = false;
        let mut note = None;
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            let mut section = None;
            let mut listed = 0;
            for (d, k) in &rows {
                let is_ignored = self.ignored.contains(k);
                if is_ignored && !self.show_ignored {
                    continue;
                }
                if listed >= self.shown {
                    show_more = true;
                    break;
                }
                listed += 1;
                if section != Some(d.change()) {
                    section = Some(d.change());
                    let title = match d.change() {
                        Change::New => "New in B".to_owned(),
                        Change::Gone => "Gone from B".to_owned(),
                        Change::Changed => format!("Changed (score {} or more)", self.min_score),
                    };
                    ui.add_space(4.0);
                    ui.label(RichText::new(title).strong());
                }
                let p = &ps.patterns[d.pattern];
                let side = usize::from(d.counts[0] == 0);
                let value = d.value.and_then(|(s, v)| Some((s, &p.slots().nth(s)?.values[v])));
                let first = value.map_or(p.sides[side].first, |(_, v)| v.sides[side].first);
                crate::patterns::lit_row(ui, |ui| {
                    if is_ignored {
                        ui.multiply_opacity(0.5);
                    }
                    // The line and the menu first, at the right: a long
                    // template is cut, not drawn under them.
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.menu_button("⋮", |ui| {
                            let what = if d.value.is_some() { "value" } else { "pattern" };
                            if is_ignored {
                                if ui.button(format!("Show this {what} again")).clicked() {
                                    unignore = Some(k.clone());
                                    ui.close();
                                }
                            } else if ui
                                .button(format!("Ignore this {what}"))
                                .on_hover_text("Leave it out of these results (Show ignored brings it back)")
                                .clicked()
                            {
                                ignore = Some(k.clone());
                                ui.close();
                            }
                            if d.value.is_none() {
                                let rule = ps.ignore_rule(p);
                                if ui
                                    .button("Save as ignore rule")
                                    .on_hover_text(format!(
                                        "Add to the pattern configuration, so patterns and compares leave its \
                                         lines out:\n{rule}"
                                    ))
                                    .clicked()
                                {
                                    note = Some(match PatternConfig::append_saved(&rule) {
                                        Ok(path) => Ok(format!("Saved to {}: {rule}", path.display())),
                                        Err(e) => Err(format!("Not saved: {e}")),
                                    });
                                    ignore = Some(k.clone());
                                    ui.close();
                                }
                            }
                            // In this window's tab, whichever tabs the sides
                            // read (another tab: copy the query there).
                            let q = ps.query(p);
                            let here = format!("In this tab ({}), all its lines:\n{q}", input.tab.title);
                            if ui.button("▾ Filter to its lines").on_hover_text(&here).clicked() {
                                out.filter = Some(q.clone());
                                ui.close();
                            }
                            if ui.button("🔍 Search for its lines").on_hover_text(&here).clicked() {
                                out.search = Some(q.clone());
                                ui.close();
                            }
                            if ui.button("📋 Copy its query").on_hover_text(q.clone()).clicked() {
                                ui.ctx().copy_text(q);
                                ui.close();
                            }
                        });
                        let label = format!("{}:{}", ["A", "B"][side], first + 1);
                        if ui.link(label).on_hover_text("Go to its first line on that side").clicked() {
                            out.goto = Some((run.tabs[side], first));
                        }
                        ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                            ui.monospace(format!("{:>7} → {:<7}", d.counts[0], d.counts[1]));
                            let change = match d.change() {
                                Change::New => "new".to_owned(),
                                Change::Gone => "gone".to_owned(),
                                Change::Changed => format!("×{:.2}", d.ratio()),
                            };
                            ui.monospace(format!("{change:>6}"));
                            ui.monospace(format!("{:>8.1}", d.score)).on_hover_text(
                                "How surely it changed (G²): 3.84 is p = 0.05, 10.8 is p = 0.001; 1 line against \
                                 none scores low, 500 against none high",
                            );
                            let presence: Vec<String> = (0..2)
                                .filter_map(|s| {
                                    run.presence[s]
                                        .as_ref()
                                        .map(|(n, per)| format!("{}{}/{n}", ["A ", "B "][s], per[d.pattern]))
                                })
                                .collect();
                            if !presence.is_empty() && d.value.is_none() {
                                ui.label(RichText::new(presence.join(" · ")).color(weak))
                                    .on_hover_text("In how many of each side's lifecycles it has lines");
                            }
                            for (name, value) in &p.fields {
                                ui.label(RichText::new(format!("{name}={value}")).color(weak));
                            }
                            match value {
                                None => {
                                    ui.add(egui::Label::new(template_job(p, text, weak, state)).truncate())
                                        .on_hover_text(p.template());
                                }
                                Some((s, v)) => {
                                    ui.label(RichText::new(format!("slot {}:", s + 1)).color(weak));
                                    ui.label(RichText::new(v.value()).color(state).monospace());
                                    ui.add(egui::Label::new(template_job(p, weak, weak, weak)).truncate())
                                        .on_hover_text(p.template());
                                }
                            }
                        });
                    });
                });
            }
            if show_more && ui.button("Show more").clicked() {
                self.shown += PAGE;
            }
        });
        if let Some(k) = ignore {
            self.ignored.push(k);
        }
        if let Some(k) = unignore {
            self.ignored.retain(|x| *x != k);
        }
        match note {
            Some(Ok(n)) => self.note = Some(n),
            Some(Err(e)) => self.error = Some(e),
            None => {}
        }
    }
}

/// A difference's row key.
fn row_key(ps: &Patterns, d: &Difference) -> RowKey {
    let p = &ps.patterns[d.pattern];
    let mut pattern: String = p.fields.iter().map(|(n, v)| format!("{n}={v} ")).collect();
    pattern += &p.template();
    let value = d.value.map(|(s, v)| (s, p.slots().nth(s).map(|x| x.values[v].value()).unwrap_or_default()));
    RowKey { pattern, value }
}

#[cfg(test)]
impl CompareUi {
    /// The rows listed, as `new gear {D|R} slot 1: R`.
    pub fn rows_for_test(&self) -> Vec<String> {
        let Some(run) = &self.run else { return Vec::new() };
        run.diffs
            .iter()
            .filter(|d| d.change() != Change::Changed || d.score >= self.min_score)
            .filter(|d| !self.ignored.contains(&row_key(&run.ps, d)))
            .map(|d| {
                let p = &run.ps.patterns[d.pattern];
                let change = match d.change() {
                    Change::New => "new",
                    Change::Gone => "gone",
                    Change::Changed => "changed",
                };
                match d.value {
                    None => format!("{change} {}", p.template()),
                    Some((s, v)) => {
                        let value = p.slots().nth(s).map(|x| x.values[v].value()).unwrap_or_default();
                        format!("{change} {} slot {}: {value}", p.template(), s + 1)
                    }
                }
            })
            .collect()
    }

    /// Ignore a pattern (no fields) by its template.
    pub fn ignore_for_test(&mut self, template: &str) {
        self.ignored.push(RowKey { pattern: template.to_owned(), value: None });
    }
}

/// A side's choice: where its lines come from, and what of it.
fn side_ui(ui: &mut egui::Ui, id: (&str, usize), side: &mut Side, input: &Input) {
    let shown = match side.from {
        SideFrom::All => "All lines".to_owned(),
        SideFrom::Filter => "Filter result".to_owned(),
        SideFrom::Query => "Lines of a filter".to_owned(),
        SideFrom::Lifecycles => format!("Lifecycles @{}", side.life),
        SideFrom::Tab => {
            let title = input.tabs.iter().find(|t| t.id == side.tab).map_or("(closed)", |t| t.title.as_str());
            format!("Tab {title}")
        }
    };
    egui::ComboBox::from_id_salt(id).width(220.0).selected_text(shown).show_ui(ui, |ui| {
        ui.selectable_value(&mut side.from, SideFrom::All, "All lines");
        ui.selectable_value(&mut side.from, SideFrom::Filter, "Filter result");
        ui.selectable_value(&mut side.from, SideFrom::Query, "Lines of a filter…");
        for l in &input.tab.lifecycles {
            let here = side.from == SideFrom::Lifecycles && side.life == l.name;
            if ui.selectable_label(here, format!("Lifecycles @{}", l.name)).clicked() && !here {
                side.from = SideFrom::Lifecycles;
                side.life = l.name.clone();
            }
        }
        for t in input.tabs.iter().filter(|t| t.id != input.tab.id) {
            let here = side.from == SideFrom::Tab && side.tab == t.id;
            if ui.selectable_label(here, format!("Tab {}", t.title)).clicked() {
                side.from = SideFrom::Tab;
                side.tab = t.id;
            }
        }
    });
    match side.from {
        SideFrom::Query => {
            ui.add(egui::TextEdit::singleline(&mut side.query).hint_text("NFD, e.g. @ecu1 == 2").desired_width(300.0));
        }
        SideFrom::Lifecycles => {
            let count = input.tab.lifecycles.iter().find(|l| l.name == side.life).map_or(0, |l| l.found.lives.len());
            let shown = match &side.pick {
                Pick::Numbers(_) => "numbers".to_owned(),
                Pick::Outcome(o) => format!("every {}", o.as_str()),
            };
            egui::ComboBox::from_id_salt((id, "pick")).selected_text(shown).show_ui(ui, |ui| {
                if ui.selectable_label(matches!(side.pick, Pick::Numbers(_)), "numbers").clicked()
                    && !matches!(side.pick, Pick::Numbers(_))
                {
                    side.pick = Pick::Numbers(String::new());
                }
                for o in OUTCOMES {
                    ui.selectable_value(&mut side.pick, Pick::Outcome(o), format!("every {}", o.as_str()));
                }
            });
            if let Pick::Numbers(t) = &mut side.pick {
                ui.add(egui::TextEdit::singleline(t).hint_text("2, 5-7").desired_width(90.0));
            }
            ui.weak(format!("of {count}"));
        }
        SideFrom::Tab => {
            let has_filter = input.tabs.iter().find(|t| t.id == side.tab).is_some_and(|t| t.kept.is_some());
            ui.add_enabled(has_filter, egui::Checkbox::new(&mut side.filtered, "its filter result"));
        }
        SideFrom::All | SideFrom::Filter => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lifecycle_numbers() {
        assert_eq!(parse_numbers("2", 5), Ok(vec![2]));
        assert_eq!(parse_numbers("5, 2 3-4,2", 5), Ok(vec![2, 3, 4, 5]));
        assert!(parse_numbers("6", 5).unwrap_err().contains("1 to 5"));
        assert!(parse_numbers("4-2", 5).unwrap_err().contains("from more to less"));
        assert!(parse_numbers(" ", 5).unwrap_err().contains("no lifecycles"));
    }

    #[test]
    fn content_and_intersect() {
        assert_eq!(content_nfd(" ", ""), None);
        assert_eq!(content_nfd("ecu == A", ""), Some("(ecu == A)".into()));
        assert_eq!(content_nfd("", "/hb/"), Some("NOT (/hb/)".into()));
        assert_eq!(content_nfd("ecu == A", "/hb/"), Some("(ecu == A) AND NOT (/hb/)".into()));
        assert_eq!(intersect(&[1, 3, 5, 7], &[2, 3, 7, 9]), [3, 7]);
    }
}
