//! The Patterns window (experimental; `docs/dev/patterns.md`, round α1): a
//! tab's lines grouped into message patterns by the core
//! (`nerka_core::pattern`), in a window of its own (owner, 2026-11-23).
//!
//! Nothing runs by itself (owner, 2026-11-23): only ▶ Run groups the lines
//! chosen (all, the filter result, a lifecycle or a typed filter), with the
//! saved pattern configuration (`<config>/nerka/patterns`). The result stays
//! until the next run and says when it is out of date: the file read
//! again, other lines, another field, the configuration or the signals
//! changed.

use crate::compare::CompareUi;
use crate::job::{Job, Poll};
use eframe::egui::{self, Color32, RichText};
use nerka_core::lifecycle::Lifecycles;
use nerka_core::pattern::{self, Pattern, PatternConfig, PatternOptions, Patterns, Piece};
use nerka_core::{Filter, LineNo, LogSource, QueryContext, SearchResult};
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime};

/// Patterns listed at first; "Show more" adds as many.
const PAGE: usize = 300;
/// A pattern's lines shown when it is unfolded; "more" adds as many.
const LINES: usize = 10;

/// Which lines are grouped.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum From {
    #[default]
    All,
    /// The lines the tab's filter keeps (all, without one).
    Filter,
    /// Lifecycle `n` (1-based) of `@name`.
    Lifecycle(String, usize),
    /// A typed NFD filter.
    Query,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Sort {
    #[default]
    Most,
    Rarest,
    First,
}

impl Sort {
    fn label(self) -> &'static str {
        match self {
            Sort::Most => "Most lines first",
            Sort::Rarest => "Rarest first",
            Sort::First => "By first line",
        }
    }
}

/// What a run grouped: a result whose key isn't the current one is out of
/// date.
#[derive(Debug, Clone, PartialEq)]
struct Key {
    /// The source (its `Arc`'s address: read again, another).
    file: usize,
    from: From,
    /// The typed filter (`From::Query`).
    query: String,
    /// The filter result's address (`From::Filter`).
    kept: usize,
    field: String,
    config: Option<(SystemTime, u64)>,
    /// The tab's signals' generation (lifecycles and typed filters use them).
    signals: u64,
}

impl Key {
    /// Why a result of `self` is out of date with `now`.
    fn why_stale(&self, now: &Key) -> Option<&'static str> {
        if self.file != now.file {
            Some("the file was read again")
        } else if self.from != now.from || (self.from == From::Query && self.query != now.query) {
            Some("other lines are chosen")
        } else if self.from == From::Filter && self.kept != now.kept {
            Some("the filter changed")
        } else if self.field != now.field {
            Some("another field is chosen")
        } else if self.config != now.config {
            Some("the pattern configuration changed")
        } else if matches!(self.from, From::Lifecycle(..) | From::Query) && self.signals != now.signals {
            Some("the signals changed")
        } else {
            None
        }
    }
}

/// What the window shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Mode {
    #[default]
    Patterns,
    Compare,
}

/// A finished run.
struct Run {
    patterns: Patterns,
    key: Key,
    took: Duration,
    /// The lines it grouped, in words.
    what: String,
}

/// A tab's Patterns window and its last result.
pub struct PatternsUi {
    pub open: bool,
    pub mode: Mode,
    pub compare: CompareUi,
    /// Its window's id: the tab's, for as long as the tab lives.
    id: u64,
    from: From,
    query: String,
    /// The field grouped (empty: `payload` if there is one, else the line).
    field: String,
    sort: Sort,
    job: Option<(Job<Patterns>, Key, String)>,
    run: Option<Run>,
    error: Option<String>,
    /// Patterns unfolded (their index in the result), with how many of
    /// their lines show.
    unfolded: HashMap<usize, usize>,
    /// Patterns listed.
    shown: usize,
    /// The configuration file's stamp, and when it was looked at.
    config: (Option<Instant>, Option<(SystemTime, u64)>),
}

impl Default for PatternsUi {
    fn default() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        PatternsUi {
            open: false,
            mode: Mode::Patterns,
            compare: CompareUi::default(),
            id: NEXT.fetch_add(1, Ordering::Relaxed),
            from: From::All,
            query: String::new(),
            field: String::new(),
            sort: Sort::Most,
            job: None,
            run: None,
            error: None,
            unfolded: HashMap::new(),
            shown: PAGE,
            config: (None, None),
        }
    }
}

/// A tab as the window sees it.
pub struct TabRef {
    /// Its Patterns window's id, which names the tab.
    pub id: u64,
    pub title: String,
    pub file: Arc<dyn LogSource>,
    /// The filter result (none: no filter).
    pub kept: Option<Arc<SearchResult>>,
    /// Its signals and bookmarks, to compile filters with.
    pub cx: QueryContext,
    /// Its lifecycles found.
    pub lifecycles: Vec<LifeRef>,
}

/// The lifecycles of a `@name` found in a tab.
pub struct LifeRef {
    pub name: String,
    /// Its lines rule as NFD, if it has one.
    pub lines: Option<String>,
    pub found: Arc<Lifecycles>,
}

impl TabRef {
    /// An NFD filter, compiled with the tab's signals and bookmarks.
    pub fn compile(&self, nfd: &str) -> Result<Filter, String> {
        self.cx.compile(nfd, &*self.file).map(Filter::Query).map_err(|e| e.to_string())
    }

    /// NFD of the lines of lifecycles `numbers` of `l` (its own lines, by
    /// its lines rule, as the Signals panel filters to one).
    pub fn lifecycle_nfd(&self, l: &LifeRef, numbers: &[u32]) -> String {
        let one: Vec<String> = numbers.iter().map(|n| format!("@{} == {n}", l.name)).collect();
        let mut nfd = if one.len() == 1 { one[0].clone() } else { format!("({})", one.join(" OR ")) };
        if let Some(lines) = &l.lines {
            nfd += &format!(" AND ({lines})");
        }
        nfd
    }
}

/// What the window needs: its tab, every tab (Compare reads others), and
/// how to read the configuration.
pub struct Input<'a> {
    pub tab: &'a TabRef,
    pub tabs: &'a [TabRef],
    pub signals_gen: u64,
    /// The window's title.
    pub title: String,
    /// The pattern configuration to run with (the saved one).
    pub config: fn() -> Result<PatternConfig, String>,
}

/// What the window asks of its tab.
#[derive(Debug, Default, PartialEq)]
pub struct Output {
    /// A line of a tab (by its id) to go to.
    pub goto: Option<(u64, LineNo)>,
    /// An NFD query for the Filter box, to run.
    pub filter: Option<String>,
    /// An NFD query for the Search box, to run.
    pub search: Option<String>,
}

impl PatternsUi {
    /// Stop a run (the tab closes or reads its files again).
    pub fn cancel(&mut self) {
        if let Some((j, _, _)) = self.job.take() {
            j.cancel();
        }
        self.compare.cancel();
    }

    pub fn is_busy(&self) -> bool {
        self.job.is_some() || self.compare.is_busy()
    }

    /// Its id: the tab's in [`TabRef::id`].
    pub fn id(&self) -> u64 {
        self.id
    }

    /// The configuration file's stamp, looked at once a second.
    fn config_stamp(&mut self) -> Option<(SystemTime, u64)> {
        let (at, stamp) = &mut self.config;
        if at.is_none_or(|t| t.elapsed() >= Duration::from_secs(1)) {
            *at = Some(Instant::now());
            *stamp = PatternConfig::saved_path()
                .and_then(|p| std::fs::metadata(p).ok())
                .map(|m| (m.modified().unwrap_or(SystemTime::UNIX_EPOCH), m.len()));
        }
        *stamp
    }

    fn key(&mut self, input: &Input) -> Key {
        Key {
            file: Arc::as_ptr(&input.tab.file) as *const u8 as usize,
            from: self.from.clone(),
            query: self.query.trim().to_owned(),
            kept: input.tab.kept.as_ref().map_or(0, |r| Arc::as_ptr(r) as usize),
            field: self.field.clone(),
            config: self.config_stamp(),
            signals: input.signals_gen,
        }
    }

    /// ▶ Run: group the lines chosen.
    fn start(&mut self, ctx: &egui::Context, input: &Input) {
        self.cancel();
        self.error = None;
        let config = match (input.config)() {
            Ok(c) => c,
            Err(e) => {
                self.error = Some(format!("Pattern configuration: {e}"));
                return;
            }
        };
        enum Lines {
            All,
            Kept(Arc<SearchResult>),
            Search(Filter),
        }
        let (lines, what) = match &self.from {
            From::All => (Lines::All, "all lines".to_owned()),
            From::Filter => match &input.tab.kept {
                Some(r) => (Lines::Kept(r.clone()), "the filter result".to_owned()),
                None => (Lines::All, "all lines (no filter)".to_owned()),
            },
            From::Lifecycle(name, n) => match input
                .tab
                .lifecycles
                .iter()
                .find(|l| l.name == *name)
                .ok_or_else(|| "not found (yet)".to_owned())
                .and_then(|l| input.tab.compile(&input.tab.lifecycle_nfd(l, &[*n as u32])))
            {
                Ok(f) => (Lines::Search(f), format!("lifecycle @{name} #{n}")),
                Err(e) => {
                    self.error = Some(format!("Lifecycle @{name}: {e}"));
                    return;
                }
            },
            From::Query => {
                let q = self.query.trim();
                if q.is_empty() {
                    self.error = Some("Type a filter for the lines to group, e.g. @ecu1 == 2".into());
                    return;
                }
                match input.tab.compile(q) {
                    Ok(f) => (Lines::Search(f), q.to_owned()),
                    Err(e) => {
                        self.error = Some(e);
                        return;
                    }
                }
            }
        };
        let field = (!self.field.is_empty()).then(|| self.field.clone());
        let options = PatternOptions { field, lines: true, config };
        let file = input.tab.file.clone();
        let key = self.key(input);
        let job = Job::spawn(ctx, move |progress| {
            let found;
            let lines: Option<&[LineNo]> = match &lines {
                Lines::All => None,
                Lines::Kept(r) => Some(&r.lines),
                Lines::Search(f) => {
                    found = nerka_core::search(&*file, f, progress)?;
                    Some(&found.lines)
                }
            };
            pattern::detect(&*file, lines, &options, progress)
        });
        self.job = Some((job, key, what));
    }

    fn poll(&mut self) {
        let Some((job, _, _)) = &self.job else { return };
        let Poll::Done(r, took) = job.poll() else { return };
        let (_, key, what) = self.job.take().expect("polled");
        match r {
            Ok(patterns) => {
                self.unfolded.clear();
                self.shown = PAGE;
                self.run = Some(Run { patterns, key, took, what });
            }
            Err(nerka_core::Error::Cancelled) => {}
            Err(e) => self.error = Some(e.to_string()),
        }
    }

    /// Show the window (if open), each frame.
    pub fn window(&mut self, ctx: &egui::Context, input: &Input) -> Output {
        let mut out = Output::default();
        if !self.open {
            return out;
        }
        self.poll();
        self.compare.poll();
        if self.is_busy() {
            ctx.request_repaint_after(Duration::from_millis(100));
        }
        let id = egui::ViewportId::from_hash_of(("nerka-patterns", self.id));
        let builder = egui::ViewportBuilder::default().with_title(input.title.clone()).with_inner_size([980.0, 600.0]);
        let mut closed = false;
        ctx.show_viewport_immediate(id, builder, |ui, _| {
            closed = ui.input(|i| i.viewport().close_requested());
            egui::CentralPanel::default().show(ui, |ui| self.contents(ui, input, &mut out));
        });
        if closed {
            self.open = false;
        }
        out
    }

    fn contents(&mut self, ui: &mut egui::Ui, input: &Input, out: &mut Output) {
        ui.horizontal(|ui| {
            ui.selectable_value(&mut self.mode, Mode::Patterns, RichText::new("🧩 Patterns").strong())
                .on_hover_text("This tab's lines grouped into patterns");
            ui.selectable_value(&mut self.mode, Mode::Compare, RichText::new("⚖ Compare").strong())
                .on_hover_text("What differs between two sets of lines: lifecycles, filters, tabs");
        });
        ui.separator();
        if self.mode == Mode::Compare || self.compare.pending {
            self.mode = Mode::Compare;
            let config = self.config_stamp();
            self.compare.ui(ui, input, config, out);
            return;
        }
        self.choices(ui, input);
        ui.horizontal(|ui| {
            if let Some((job, _, what)) = &self.job {
                if ui.button("■ Cancel").clicked() {
                    job.cancel();
                }
                ui.add(egui::ProgressBar::new(job.progress.fraction()).desired_width(160.0).show_percentage());
                ui.weak(format!("grouping {what}…"));
            } else if ui
                .button("▶ Run")
                .on_hover_text("Group the lines chosen into patterns, with the saved pattern configuration")
                .clicked()
            {
                self.start(ui.ctx(), input);
            }
            if let Some(r) = &self.run {
                let p = &r.patterns;
                ui.label(format!("{} patterns in {} lines ({}), {:.1?}", p.patterns.len(), p.total(), r.what, r.took))
                    .on_hover_text(stats_text(p));
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                egui::ComboBox::from_id_salt("patterns-sort").selected_text(self.sort.label()).show_ui(ui, |ui| {
                    for s in [Sort::Most, Sort::Rarest, Sort::First] {
                        ui.selectable_value(&mut self.sort, s, s.label());
                    }
                });
            });
        });
        let now = self.key(input);
        if let Some(why) = self.run.as_ref().and_then(|r| r.key.why_stale(&now)) {
            ui.colored_label(ui.visuals().warn_fg_color, format!("Out of date: {why}. ▶ Run groups them again."));
        }
        if let Some(e) = &self.error {
            ui.colored_label(ui.visuals().error_fg_color, e);
        }
        ui.separator();
        match &self.run {
            None if self.job.is_none() => {
                ui.weak(
                    "Patterns group lines by the message they print: values masked, the parts that differ \
                     as slots, states with their values. Nothing is grouped until ▶ Run.",
                );
            }
            None => {}
            Some(_) => self.list(ui, input.tab, out),
        }
    }

    /// Which lines, which field.
    fn choices(&mut self, ui: &mut egui::Ui, input: &Input) {
        ui.horizontal(|ui| {
            ui.label("Lines:");
            let shown = match &self.from {
                From::All => "All lines".to_owned(),
                From::Filter => match &input.tab.kept {
                    Some(r) => format!("Filter result ({} lines)", r.lines.len()),
                    None => "Filter result (no filter: all lines)".to_owned(),
                },
                From::Lifecycle(n, _) => format!("Lifecycle @{n}"),
                From::Query => "Lines of a filter".to_owned(),
            };
            egui::ComboBox::from_id_salt("patterns-from").selected_text(shown).show_ui(ui, |ui| {
                ui.selectable_value(&mut self.from, From::All, "All lines");
                ui.selectable_value(&mut self.from, From::Filter, "Filter result");
                for l in &input.tab.lifecycles {
                    let here = matches!(&self.from, From::Lifecycle(n, _) if *n == l.name);
                    if ui.selectable_label(here, format!("Lifecycle @{}", l.name)).clicked() && !here {
                        self.from = From::Lifecycle(l.name.clone(), 1);
                    }
                }
                ui.selectable_value(&mut self.from, From::Query, "Lines of a filter…");
            });
            match &mut self.from {
                From::Lifecycle(name, n) => {
                    let count =
                        input.tab.lifecycles.iter().find(|l| l.name == *name).map_or(0, |l| l.found.lives.len());
                    ui.label("#");
                    ui.add(egui::DragValue::new(n).range(1..=count.max(1)));
                    ui.weak(format!("of {count}"));
                }
                From::Query => {
                    ui.add(
                        egui::TextEdit::singleline(&mut self.query)
                            .hint_text("NFD, e.g. @ecu1 == 2 AND level >= WARN")
                            .desired_width(280.0),
                    );
                }
                _ => {}
            }
            ui.separator();
            ui.label("Field:");
            let schema = input.tab.file.schema();
            let shown = if self.field.is_empty() { "Default".to_owned() } else { self.field.clone() };
            egui::ComboBox::from_id_salt("patterns-field").selected_text(shown).show_ui(ui, |ui| {
                ui.selectable_value(&mut self.field, String::new(), "Default")
                    .on_hover_text("payload if the file has it, else the line");
                ui.selectable_value(&mut self.field, "raw".to_owned(), "raw (the line)");
                for d in schema.iter().filter(|d| d.kind == nerka_core::FieldKind::Str) {
                    ui.selectable_value(&mut self.field, d.name.to_string(), &*d.name);
                }
            });
        });
    }

    /// The patterns, sorted, the unfolded ones with their slots and lines.
    fn list(&mut self, ui: &mut egui::Ui, tab: &TabRef, out: &mut Output) {
        let Some(run) = &self.run else { return };
        let ps = &run.patterns;
        let mut order: Vec<usize> = (0..ps.patterns.len()).collect();
        match self.sort {
            Sort::Most => {}
            Sort::Rarest => order.sort_by_key(|&k| (ps.patterns[k].count(), ps.patterns[k].first())),
            Sort::First => order.sort_by_key(|&k| ps.patterns[k].first()),
        }
        let total = ps.total().max(1);
        let state = ui.visuals().hyperlink_color;
        let weak = ui.visuals().weak_text_color();
        let mut toggle = None;
        let mut more_lines = None;
        let mut show_more = false;
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            for &k in order.iter().take(self.shown) {
                let p = &ps.patterns[k];
                let open = self.unfolded.get(&k).copied();
                ui.horizontal(|ui| {
                    if ui
                        .small_button(if open.is_some() { "▾" } else { "▸" })
                        .on_hover_text("Its slots and lines")
                        .clicked()
                    {
                        toggle = Some(k);
                    }
                    // The buttons first, at the right: a long template is
                    // cut, not drawn under them.
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let q = ps.query(p);
                        if ui.small_button("📋").on_hover_text(format!("Copy its query:\n{q}")).clicked() {
                            ui.ctx().copy_text(q.clone());
                        }
                        if ui
                            .small_button("🔍")
                            .on_hover_text("Search for its lines (among the filtered ones)")
                            .clicked()
                        {
                            out.search = Some(q.clone());
                        }
                        if ui.small_button("▾ Filter").on_hover_text(format!("Filter to its lines:\n{q}")).clicked() {
                            out.filter = Some(q);
                        }
                        ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                            let share = 100.0 * p.count() as f64 / total as f64;
                            ui.monospace(format!("{share:>5.1}% {:>9}", p.count()));
                            for (name, value) in &p.fields {
                                ui.label(RichText::new(format!("{name}={value}")).color(weak));
                            }
                            let mut tip = format!(
                                "{}\nFirst line {}, last {}. Click: go to the first.",
                                p.template(),
                                p.first() + 1,
                                p.last() + 1
                            );
                            if p.unsorted {
                                tip += "\nNot sorted: lines that fit no pattern (too many kinds of lines).";
                            }
                            let job = template_job(p, ui.visuals().text_color(), weak, state);
                            let label = egui::Label::new(job).sense(egui::Sense::click()).truncate();
                            if ui.add(label).on_hover_text(tip).clicked() {
                                out.goto = Some((tab.id, p.first()));
                            }
                        });
                    });
                });
                if let Some(n) = open {
                    ui.indent(("pattern", k), |ui| {
                        if let Some(more) = unfolded(ui, p, n, tab, out) {
                            more_lines = Some((k, more));
                        }
                    });
                }
            }
            if order.len() > self.shown {
                ui.horizontal(|ui| {
                    ui.weak(format!("{} more", order.len() - self.shown));
                    show_more = ui.button("Show more").clicked();
                });
            }
        });
        if let Some(k) = toggle
            && self.unfolded.remove(&k).is_none()
        {
            self.unfolded.insert(k, LINES);
        }
        if let Some((k, n)) = more_lines {
            self.unfolded.insert(k, n);
        }
        if show_more {
            self.shown += PAGE;
        }
    }
}

/// An unfolded pattern: its slots' values and its first `lines` lines.
/// Returns how many lines to show next, if "more" was clicked.
fn unfolded(ui: &mut egui::Ui, p: &Pattern, lines: usize, tab: &TabRef, out: &mut Output) -> Option<usize> {
    let file = &*tab.file;
    for (i, s) in p.slots().enumerate() {
        let kind = if s.is_state() { "state" } else { "values" };
        let more = if s.overflow { ", more not kept" } else { "" };
        ui.label(RichText::new(format!("slot {} ({kind}{more})", i + 1)).strong());
        egui::Grid::new(("slot", p.first(), i)).striped(true).show(ui, |ui| {
            for v in &s.values {
                ui.monospace(format!("{:>9}", v.count()));
                ui.label(v.value());
                if ui.link(format!("first {}", v.first() + 1)).clicked() {
                    out.goto = Some((tab.id, v.first()));
                }
                if v.last() != v.first() && ui.link(format!("last {}", v.last() + 1)).clicked() {
                    out.goto = Some((tab.id, v.last()));
                }
                ui.end_row();
            }
        });
    }
    let all: Vec<LineNo> = p.lines.iter().flatten().copied().collect();
    ui.label(RichText::new(format!("Lines ({})", all.len())).strong());
    for &l in all.iter().take(lines) {
        ui.horizontal(|ui| {
            if ui.link(format!("{:>8}", l + 1)).clicked() {
                out.goto = Some((tab.id, l));
            }
            let text = file.line_text(l).unwrap_or_default();
            let text: String = text.chars().take(300).collect();
            ui.add(egui::Label::new(RichText::new(text).monospace()).truncate());
        });
    }
    if all.len() > lines {
        let mut next = None;
        ui.horizontal(|ui| {
            ui.weak(format!("… {} more", all.len() - lines));
            if ui.small_button("more").clicked() {
                next = Some(lines * 10);
            }
        });
        return next;
    }
    None
}

/// A pattern's template: text as text (masked values weak), `<*>` weak, a
/// state's values in `state`'s colour.
pub fn template_job(p: &Pattern, text: Color32, weak: Color32, state: Color32) -> egui::text::LayoutJob {
    let mut job = egui::text::LayoutJob::default();
    let font = egui::FontId::monospace(13.0);
    let add = |job: &mut egui::text::LayoutJob, s: &str, c: Color32| {
        job.append(s, 0.0, egui::TextFormat::simple(font.clone(), c));
    };
    for piece in p.pieces() {
        match piece {
            Piece::Text(t) => {
                for (i, part) in t.split("<*>").enumerate() {
                    if i > 0 {
                        add(&mut job, "<*>", weak);
                    }
                    if !part.is_empty() {
                        add(&mut job, part, text);
                    }
                }
            }
            Piece::Any => add(&mut job, "<*>", weak),
            Piece::State(vs) => {
                add(&mut job, "{", weak);
                for (i, v) in vs.iter().enumerate() {
                    if i > 0 {
                        add(&mut job, "|", weak);
                    }
                    add(&mut job, v, state);
                }
                add(&mut job, "}", weak);
            }
        }
    }
    job
}

/// A run's stages, for the hover on its summary.
fn stats_text(p: &Patterns) -> String {
    let s = &p.stats;
    let mut t = format!(
        "Stage 1: {} templates, {:.1?}\nStage 2: {:.1?}; kept apart: {} two new slots, {} side too long, {} by rules",
        s.templates, s.stage1, s.stage2, s.apart, s.side, s.rules
    );
    if s.ignored != [0, 0] {
        t += &format!("\nIgnored by rules: {} lines", s.ignored[0] + s.ignored[1]);
    }
    t += "\nSettings and rules: the pattern configuration (nerka patterns --help; docs: cli.md)";
    t
}

#[cfg(test)]
mod tests {
    use super::*;
    use nerka_core::TextFile;

    fn file() -> Arc<dyn LogSource> {
        let lines = ["gear D", "gear P", "gear D", "door open", "door closed", "door open", "gear N"];
        Arc::new(TextFile::from_bytes("t", lines.join("\n").into_bytes()))
    }

    fn tab_of(id: u64, f: Arc<dyn LogSource>) -> TabRef {
        TabRef { id, title: format!("t{id}"), file: f, kept: None, cx: QueryContext::default(), lifecycles: vec![] }
    }

    fn input<'a>(tab: &'a TabRef, tabs: &'a [TabRef]) -> Input<'a> {
        let config = || Ok(PatternConfig::default());
        Input { tab, tabs, signals_gen: 0, title: "Patterns".into(), config }
    }

    /// Frames of the window until its run is done.
    fn run_until_done(ui: &mut PatternsUi, ctx: &egui::Context, input: &Input) -> Vec<Output> {
        let mut outs = Vec::new();
        let until = Instant::now() + Duration::from_secs(10);
        loop {
            let mut o = None;
            let mut full = ctx.run_ui(egui::RawInput::default(), |c| o = Some(ui.window(c.ctx(), input)));
            full.textures_delta.clear();
            outs.extend(o);
            if !ui.is_busy() {
                return outs;
            }
            assert!(Instant::now() < until, "the run never finished");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn nothing_runs_until_asked_then_the_result_stays() {
        let t = [tab_of(1, file())];
        let ctx = egui::Context::default();
        let mut p = PatternsUi { open: true, ..Default::default() };
        let i = input(&t[0], &t);
        run_until_done(&mut p, &ctx, &i);
        assert!(p.run.is_none() && p.job.is_none());
        p.start(&ctx, &i);
        run_until_done(&mut p, &ctx, &i);
        let g = [tab_of(1, file())];
        let now = p.key(&input(&g[0], &g));
        let run = p.run.as_ref().expect("a result");
        let ts: Vec<String> = run.patterns.patterns.iter().map(Pattern::template).collect();
        assert_eq!(ts, ["gear {D|P|N}", "door {open|closed}"]);
        assert_eq!(run.patterns.patterns[0].lines[0], [0, 1, 2, 6]);
        // Another file: out of date, the result kept.
        assert_eq!(run.key.why_stale(&now), Some("the file was read again"));
        assert!(p.run.is_some());
    }

    #[test]
    fn a_typed_filter_that_doesnt_compile_says_why() {
        let t = [tab_of(1, file())];
        let ctx = egui::Context::default();
        let mut p = PatternsUi { open: true, from: From::Query, ..Default::default() };
        p.start(&ctx, &input(&t[0], &t));
        assert!(p.error.as_deref().is_some_and(|e| e.contains("e.g. @ecu1")), "{:?}", p.error);
        p.query = "@x == 1".into();
        p.start(&ctx, &input(&t[0], &t));
        assert!(p.error.as_deref().is_some_and(|e| e.contains('x')), "{:?}", p.error);
        assert!(p.job.is_none());
    }

    /// Compare (α2): another tab against this one's lines of a filter, the
    /// new message and the new value listed; an ignored row leaves the
    /// list, and a click on a row's line names its side's tab.
    #[test]
    fn compare_two_tabs() {
        let a = file();
        let b: Arc<dyn LogSource> = Arc::new(nerka_core::TextFile::from_bytes(
            "b",
            [
                "gear D",
                "gear P",
                "gear D",
                "door open",
                "door closed",
                "door open",
                "gear N",
                "gear R",
                "gear R",
                "panic in task",
            ]
            .join("\n")
            .into_bytes(),
        ));
        let tabs = [tab_of(1, b), tab_of(2, a)];
        let ctx = egui::Context::default();
        let mut p = PatternsUi { open: true, mode: Mode::Compare, ..Default::default() };
        p.compare.sides[0] = crate::compare::Side { from: crate::compare::SideFrom::Tab, tab: 2, ..Default::default() };
        p.compare.sides[1] =
            crate::compare::Side { from: crate::compare::SideFrom::Query, query: "/./".into(), ..Default::default() };
        p.compare.pending = true;
        let i = input(&tabs[0], &tabs);
        run_until_done(&mut p, &ctx, &i);
        let rows = p.compare.rows_for_test();
        assert!(rows.contains(&"new panic in task".to_owned()), "{rows:?}");
        assert!(rows.iter().any(|r| r.starts_with("new gear {") && r.ends_with("slot 1: R")), "{rows:?}");
        // Ignored, it leaves the list.
        p.compare.ignore_for_test("panic in task");
        assert!(!p.compare.rows_for_test().contains(&"new panic in task".to_owned()));
    }

    #[test]
    fn stale_reasons() {
        let k = Key {
            file: 1,
            from: From::Filter,
            query: String::new(),
            kept: 5,
            field: String::new(),
            config: None,
            signals: 0,
        };
        assert_eq!(k.why_stale(&k.clone()), None);
        assert_eq!(k.why_stale(&Key { kept: 6, ..k.clone() }), Some("the filter changed"));
        assert_eq!(k.why_stale(&Key { field: "raw".into(), ..k.clone() }), Some("another field is chosen"));
        let stamp = Some((SystemTime::UNIX_EPOCH, 3));
        assert_eq!(k.why_stale(&Key { config: stamp, ..k.clone() }), Some("the pattern configuration changed"));
        // Signals matter to lifecycles and typed filters only.
        assert_eq!(k.why_stale(&Key { signals: 1, ..k.clone() }), None);
        let q = Key { from: From::Query, query: "@a == 1".into(), ..k.clone() };
        assert_eq!(q.why_stale(&Key { signals: 1, ..q.clone() }), Some("the signals changed"));
        assert_eq!(q.why_stale(&Key { query: "@a == 2".into(), ..q.clone() }), Some("other lines are chosen"));
    }

    #[test]
    fn the_template_colours_states() {
        let f = file();
        let ps = pattern::detect(&*f, None, &PatternOptions::default(), &nerka_core::Progress::new()).unwrap();
        let job = template_job(&ps.patterns[0], Color32::WHITE, Color32::GRAY, Color32::BLUE);
        assert_eq!(job.text, "gear {D|P|N}");
        let colors: Vec<Color32> = job.sections.iter().map(|s| s.format.color).collect();
        let (w, g, b) = (Color32::WHITE, Color32::GRAY, Color32::BLUE);
        assert_eq!(colors, [w, g, b, g, b, g, b, g]);
    }
}
