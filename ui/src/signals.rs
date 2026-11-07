//! The Signals panel: the tab's signals (`doc/sources-and-signals.md` →
//! "What a signal is"), each with its state on the current line. They come
//! from the file (`@time`), from the user's saved definitions (edited
//! here) and from the named groups of the selection's queries (kind and
//! scope chosen here). The FIBEX files that describe DLT non-verbose
//! messages are chosen here too (owner, 2026-10-24). Core does the
//! capturing and decoding; the panel only edits definitions and shows
//! values.
//!
//! Lifecycles (`doc/lifecycles.md`) are defined here too (a lifecycle is a
//! signal: `@boot`), signals are bound to one (`per`) with a default, and
//! the lifecycles found are listed under the signals, with their findings.

use crate::plot::PlotState;
use eframe::egui::{self, RichText};
use nerka_core::lifecycle::{self, LifecycleDef};
use nerka_core::{Library, LineNo, LogSource, SearchResult, SignalDef, SignalKind, SignalScope, SignalSeries, Signals};
use std::collections::HashMap;
use std::sync::Arc;

/// The panel's state in one tab.
#[derive(Default)]
pub struct SignalsUi {
    draft: Draft,
    /// The saved signal the form edits (its name), or none for a new one.
    editing: Option<String>,
    /// The definitions bound to the tab's source, by generation.
    bound: Vec<(String, Result<SignalDef, String>)>,
    bound_gen: Option<u64>,
    /// The FIBEX catalog's arguments, worked out once per catalog (its
    /// address), and the tree's filter text.
    args: Option<(usize, Vec<nerka_core::Argument>)>,
    arg_filter: String,
    /// The tree's folding: Collapse all / Expand all and a new filter start
    /// it again (`tree.0` is in the groups' ids) with every group closed or
    /// open (`tree.1`); after that each group folds on its own.
    tree: (u64, Option<bool>),
    /// The filter the tree was last opened for.
    tree_filter: String,
}

struct Draft {
    /// A lifecycle, not a signal.
    life: bool,
    name: String,
    kind: SignalKind,
    group: String,
    query: String,
    /// The lifecycle it resets with (empty: none) and its default.
    per: String,
    default: String,
    /// A lifecycle's rules (`doc/lifecycles.md`): its lines, what starts
    /// one, how one ends. Queries are NFD or `{pack.query}`.
    life_form: LifeForm,
    error: Option<String>,
}

/// The lifecycle form's fields.
struct LifeForm {
    lines: String,
    on_start: bool,
    start: String,
    on_reset: bool,
    /// `uptime` or `@signal`.
    reset: String,
    tolerance: String,
    on_gap: bool,
    gap: String,
    planned: String,
    crash: String,
    late: String,
    /// What the last preview found (or why it couldn't).
    preview: Option<Result<String, String>>,
}

impl Default for LifeForm {
    fn default() -> Self {
        let d = |v: f64| lifecycle::format_duration(v);
        LifeForm {
            lines: String::new(),
            on_start: false,
            start: String::new(),
            on_reset: true,
            reset: "uptime".into(),
            tolerance: d(lifecycle::DEFAULT_TOLERANCE),
            on_gap: false,
            gap: "30s".into(),
            planned: String::new(),
            crash: String::new(),
            late: d(lifecycle::DEFAULT_LATE),
            preview: None,
        }
    }
}

impl LifeForm {
    fn of(l: &LifecycleDef) -> LifeForm {
        let text = |r: Option<&lifecycle::Rule>| r.map(|r| r.text()).unwrap_or_default();
        let mut f = LifeForm {
            lines: text(l.lines()),
            start: text(l.start()),
            planned: text(l.planned()),
            crash: text(l.crash()),
            late: lifecycle::format_duration(l.late()),
            ..LifeForm::default()
        };
        f.on_start = l.start().is_some();
        f.on_reset = l.reset().is_some();
        if let Some(r) = l.reset() {
            f.reset = match &r.value {
                lifecycle::ResetValue::Uptime => "uptime".into(),
                lifecycle::ResetValue::Signal(s) => format!("@{s}"),
            };
            f.tolerance = lifecycle::format_duration(r.tolerance);
        }
        f.on_gap = l.gap().is_some();
        if let Some(g) = l.gap() {
            f.gap = lifecycle::format_duration(g);
        }
        f
    }

    /// The definition the fields describe; an error names the field.
    fn build(&self, name: &str) -> Result<LifecycleDef, String> {
        let q = |what: &str, e: nerka_core::DslError| format!("{what}: {} (at column {})", e.msg, e.pos + 1);
        let dur = |what: &str, t: &str| {
            lifecycle::parse_duration(t).ok_or_else(|| format!("{what}: '{t}' isn't a duration (30s, 500ms, 2m)"))
        };
        let mut d = LifecycleDef::new(name)?
            .with_lines(&self.lines)
            .map_err(|e| q("Its lines", e))?
            .with_start(if self.on_start { &self.start } else { "" })
            .map_err(|e| q("Starts on a line", e))?
            .with_planned(&self.planned)
            .map_err(|e| q("Planned", e))?
            .with_crash(&self.crash)
            .map_err(|e| q("Crash", e))?
            .with_late(dur("Late start above", &self.late)?);
        if self.on_start && self.start.trim().is_empty() {
            return Err("Starts on a line: which line? Give a query".into());
        }
        if self.on_reset {
            let r = self.reset.trim();
            let value = if r.eq_ignore_ascii_case("uptime") {
                lifecycle::ResetValue::Uptime
            } else if let Some(s) = r.strip_prefix('@').filter(|s| !s.is_empty()) {
                lifecycle::ResetValue::Signal(s.to_ascii_lowercase())
            } else {
                return Err(format!("Uptime goes back: '{r}' isn't uptime or a signal (@name)"));
            };
            let tolerance = dur("Uptime goes back", &self.tolerance)?;
            d = d.with_reset(Some(lifecycle::Reset { value, tolerance }));
        }
        if self.on_gap {
            d = d.with_gap(Some(dur("Its lines stop", &self.gap)?));
        }
        d.check()?;
        Ok(d)
    }
}

impl Default for Draft {
    fn default() -> Self {
        Draft {
            life: false,
            name: String::new(),
            kind: SignalKind::Num,
            group: String::new(),
            query: String::new(),
            per: String::new(),
            default: String::new(),
            life_form: LifeForm::default(),
            error: None,
        }
    }
}

impl Draft {
    /// The form filled with `d`.
    fn of(d: &SignalDef) -> Draft {
        if let Some(l) = d.as_lifecycle() {
            return Draft { life: true, name: l.name().to_owned(), life_form: LifeForm::of(l), ..Draft::default() };
        }
        Draft {
            name: d.name().to_owned(),
            kind: d.kind(),
            group: if d.group() == d.name() { String::new() } else { d.group().to_owned() },
            query: d.query().source().to_owned(),
            per: d.per().unwrap_or_default().to_owned(),
            default: d.default_value().map(|v| v.to_string()).unwrap_or_default(),
            ..Draft::default()
        }
    }

    /// The definition the form describes.
    fn build(&self) -> Result<SignalDef, String> {
        if self.life {
            return self.life_form.build(&self.name).map(SignalDef::lifecycle);
        }
        let group = (!self.group.trim().is_empty()).then_some(self.group.as_str());
        let per = (!self.per.trim().is_empty()).then_some(self.per.as_str());
        let default = (!self.default.trim().is_empty()).then_some(self.default.as_str());
        SignalDef::new(&self.name, self.kind, &self.query, group)
            .and_then(|d| d.with_per(per).with_default(default))
            .map_err(|e| e.to_string())
    }
}

/// What happened in the panel this frame.
#[derive(Default)]
pub struct Output {
    pub close: bool,
    /// A saved signal was added, changed or removed.
    pub saved_changed: bool,
    /// The signal the form added or updated (by name).
    pub adopted: Option<String>,
    /// Show this check in the Checks pane (a check's signal was clicked).
    pub open_check: Option<String>,
    /// A kind chosen for a query signal.
    pub kind: Option<(String, SignalKind)>,
    /// A scope chosen for a signal.
    pub scope: Option<(String, SignalScope)>,
    /// Go to this line (the event that set a state was clicked).
    pub goto: Option<LineNo>,
    /// Filter by this NFD (a lifecycle's "filter to it").
    pub filter: Option<String>,
    /// Open the Checks pane on this lifecycle's results (name, number).
    pub checks: Option<(String, u32)>,
    pub fibex: Option<FibexAction>,
    /// The picked FIBEX arguments changed.
    pub args_changed: bool,
}

/// What to do with the FIBEX files.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FibexAction {
    /// Pick more files.
    Add,
    /// Use none.
    Forget,
}

/// The events each signal shows: all of them, or for a
/// [`SignalScope::Filtered`] one those on the lines the filter keeps
/// (worked out once per filter result).
#[derive(Default)]
pub struct Views {
    key: (u64, usize),
    filtered: HashMap<String, Arc<SignalSeries>>,
}

impl Views {
    /// `generation` changes with the signals; `kept` is the filter result.
    pub fn series(
        &mut self,
        def: &SignalDef,
        file: &dyn LogSource,
        kept: Option<&Arc<SearchResult>>,
        generation: u64,
    ) -> Option<Arc<SignalSeries>> {
        let all = def.cached_series(file)?;
        let Some(kept) = kept.filter(|_| def.scope() == SignalScope::Filtered) else { return Some(Arc::new(all)) };
        let key = (generation, Arc::as_ptr(kept) as usize);
        if self.key != key {
            self.filtered.clear();
            self.key = key;
        }
        Some(self.filtered.entry(def.name().to_owned()).or_insert_with(|| Arc::new(all.within(&kept.lines))).clone())
    }
}

/// What the panel shows and edits.
pub struct Input<'a> {
    /// The tab's signals (generation: changes whenever they do).
    pub signals: &'a Signals,
    pub generation: u64,
    /// The user's saved signals (every tab has them).
    pub saved: &'a mut Signals,
    pub file: &'a dyn LogSource,
    /// The current line.
    pub line: LineNo,
    pub plot: &'a mut PlotState,
    pub views: &'a mut Views,
    /// The filter result, if a filter is applied.
    pub kept: Option<&'a Arc<SearchResult>>,
    /// The FIBEX files in use (every tab, this session).
    pub fibex: Option<&'a nerka_core::Fibex>,
    /// The FIBEX arguments this tab uses as signals.
    pub arg_picks: &'a mut Vec<nerka_core::Argument>,
    /// The query library: lifecycles reuse its queries.
    pub library: &'a Library,
    /// The checks' results on this tab's file: shown per lifecycle.
    pub check_results: &'a [(String, crate::checks::Outcome)],
}

pub fn signals_panel(ui: &mut egui::Ui, st: &mut SignalsUi, input: Input<'_>) -> Output {
    let Input { signals, generation, saved, file, line, plot, views, kept, fibex, arg_picks, library, check_results } =
        input;
    let mut out = Output::default();
    ui.horizontal(|ui| {
        ui.heading("Signals");
        ui.weak("ℹ").on_hover_text(
            "Values captured by a regex group. An event is a line that carries one; the state holds it until the \
             next. Filter with @name (the state), @name.event or @name.changed. Named groups of the selection's \
             included and signal (~) queries are signals too.",
        );
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            out.close = crate::close_button(ui).on_hover_text("Close panel").clicked();
            // The plot can be closed and shown again with its lanes.
            let tip = if plot.shown { "Close the plot (its lanes are kept)" } else { "Show the plot" };
            if ui.selectable_label(plot.shown, "📈 Plot").on_hover_text(tip).clicked() {
                plot.shown = !plot.shown;
            }
        });
    });
    // The list (and the form) above, the FIBEX browser below, with a divider
    // to drag between them (owner, 2026-10-28: many signals left no room for
    // the browser). Each part scrolls both ways: content wider than the panel
    // would otherwise be clipped, with no way to reach it (issue #4).
    let n_signals = signals.len();
    let list = |ui: &mut egui::Ui| {
        egui::ScrollArea::both().id_salt("signals_panel").auto_shrink([false, false]).show(ui, |ui| {
            if st.bound_gen != Some(generation) {
                st.bound = signals
                    .iter()
                    .map(|d| (d.name().to_owned(), d.bind(file.schema()).map_err(|e| e.to_string())))
                    .collect();
                st.bound_gen = Some(generation);
            }

            if signals.is_empty() {
                ui.weak("No signals in this tab yet.");
            }
            // The value column gets the room the other columns leave
            // (owner, 2026-10-28: a fixed cut wasted a wide panel).
            let fit = value_room(ui, signals);
            let mut remove = None;
            let mut switch: Option<(String, bool)> = None;
            let mut remove_arg: Option<String> = None;
            let mut edit = None;
            egui::Grid::new("signals").num_columns(6).striped(true).show(ui, |ui| {
                for d in signals.iter() {
                    // Plotting a time against time says nothing.
                    if d.kind() == SignalKind::Time {
                        ui.label("");
                    } else {
                        let mut on = plot.is_plotted(d.name());
                        let tip = if d.as_lifecycle().is_some() {
                            "Plot it: a line where each lifecycle starts (red after one that ended badly)"
                        } else {
                            "Plot it (its state over time)"
                        };
                        if ui.checkbox(&mut on, "").on_hover_text(tip).changed() {
                            plot.toggle(d.name());
                        }
                    }
                    let hover = if d.kind() == SignalKind::Time && d.is_from_source() {
                        "time\nThe file's own time of each line (DLT: when the logger received it)".to_owned()
                    } else if let Some(l) = d.as_lifecycle() {
                        format!(
                            "lifecycle: {}\nThe number of the lifecycle each line is in; an event where one starts",
                            l.spec()
                        )
                    } else if let Some(c) = d.check_name() {
                        check_hover(c, d)
                    } else {
                        let mut h = format!("group: {}\n{}", d.group(), d.query().source());
                        if let Some(p) = d.per() {
                            h += &format!("\nResets at each new lifecycle '{p}'");
                            if !d.is_linked() {
                                h += " (not defined!)";
                            }
                        }
                        if let Some(v) = d.default_value() {
                            h += &format!("\nDefault: {v} (where no event set it)");
                        }
                        h
                    };
                    // Long names keep their end, where they differ.
                    let name = crate::plot::name_tail(&format!("@{}", d.name()), 32);
                    ui.label(RichText::new(name).monospace().strong()).on_hover_text(format!("@{}\n{hover}", d.name()));
                    kind_cell(ui, d, &mut out);
                    scope_cell(ui, d, &mut out);
                    if d.is_from_source() {
                        let v = d.source_value(file, line);
                        let (shown, t) = match v {
                            Some(t) if d.kind() == SignalKind::Time => {
                                (fit_time(t, fit + SUFFIX), nerka_core::time::format_time(t, 6))
                            }
                            Some(u) => (format!("{u:.4} s"), format!("{u} s")),
                            None => ("—".to_owned(), "—".to_owned()),
                        };
                        ui.label(RichText::new(shown).monospace())
                            .on_hover_text(format!("{t}\nEvery line is an event"));
                        ui.weak("built in");
                        ui.end_row();
                        continue;
                    }
                    let bound = st.bound.iter().find(|(n, _)| n == d.name()).map(|(_, b)| b);
                    match bound {
                        Some(Ok(_)) => match views.series(d, file, kept, generation) {
                            // The state on the cursor line, and the event that set it.
                            Some(series) => match series.state_at(line) {
                                Some((None, v)) => {
                                    let full = d.format(v);
                                    ui.label(
                                        RichText::new(format!("{}  (default)", short_value(&full, fit)))
                                            .monospace()
                                            .italics()
                                            .weak(),
                                    )
                                    .on_hover_text(format!(
                                        "{full}: the default, assumed. No event set it yet{}",
                                        d.per().map(|p| format!(" in this lifecycle ({p})")).unwrap_or_default()
                                    ));
                                }
                                Some((Some(row), v)) => {
                                    // A lifecycle's number: "#2".
                                    let full = if d.as_lifecycle().is_some() {
                                        format!("#{}", d.format(v))
                                    } else {
                                        d.format(v)
                                    };
                                    let v = match v {
                                        nerka_core::ValueRef::Num(t) if d.kind() == SignalKind::Time => {
                                            fit_time(t, fit)
                                        }
                                        _ => short_value(&full, fit),
                                    };
                                    let events = series.len();
                                    if row == line {
                                        ui.label(RichText::new(format!("{v}  • event")).monospace()).on_hover_text(
                                            format!("{full}\nReceived on line {} ({events} events)", line + 1),
                                        );
                                    } else {
                                        let r = ui.link(RichText::new(format!("{v}  ↑ {}", row + 1)).monospace());
                                        if r.on_hover_text(format!(
                                            "{full}\nState on line {}, set by the event on line {} ({events} events). \
                                     Click to go there.",
                                            line + 1,
                                            row + 1
                                        ))
                                        .clicked()
                                        {
                                            out.goto = Some(row);
                                        }
                                    }
                                }
                                None => {
                                    ui.label(RichText::new("—").monospace())
                                        .on_hover_text(format!("No event before line {}", line + 1));
                                }
                            },
                            None => {
                                ui.weak("…").on_hover_text("Collecting events");
                            }
                        },
                        Some(Err(e)) => {
                            ui.colored_label(ui.visuals().error_fg_color, "error").on_hover_text(e);
                        }
                        None => {
                            ui.label("");
                        }
                    }
                    if let Some(c) = d.check_name() {
                        // Read-only: the check is edited in the Checks pane.
                        let tip = format!("{}\nClick: the check in the Checks pane", check_hover(c, d));
                        if ui.link(RichText::new("check").weak()).on_hover_text(tip).clicked() {
                            out.open_check = Some(c.to_owned());
                        }
                    } else if d.queries().is_empty() {
                        ui.horizontal(|ui| {
                            let mut on = true;
                            if ui
                                .checkbox(&mut on, "")
                                .on_hover_text("On: computed and usable in queries. Off: kept, not computed")
                                .changed()
                            {
                                switch = Some((d.name().to_owned(), on));
                            }
                            if ui.small_button("✎").on_hover_text("Edit").clicked() {
                                edit = Some((*d).clone());
                            }
                            if ui.small_button("−").on_hover_text("Remove (from every tab)").clicked() {
                                remove = Some(d.name().to_owned());
                            }
                        });
                    } else if d.argument_index().is_some() {
                        ui.horizontal(|ui| {
                            ui.weak("FIBEX").on_hover_text(d.queries()[0].as_str());
                            if ui.small_button("−").on_hover_text("Remove it from this tab's signals").clicked() {
                                remove_arg = Some(d.name().to_owned());
                            }
                        });
                    } else {
                        ui.weak("query").on_hover_text(format!(
                        "From the named group (?<{}>…) of: {}\nIt goes away with the query; pin its kind in the pack \
                     with signals: {}:{}",
                        d.group(),
                        d.queries().join(", "),
                        d.group(),
                        d.kind().as_str()
                    ));
                    }
                    ui.end_row();
                }
                // The saved ones switched off: listed, to switch on again.
                for d in saved.iter().filter(|d| !d.is_on()) {
                    ui.label("");
                    let what = if d.as_lifecycle().is_some() { "lifecycle" } else { d.kind().as_str() };
                    ui.label(RichText::new(format!("@{}", d.name())).monospace().weak()).on_hover_text(format!(
                        "Off: kept, not computed
{}",
                        d.spec()
                    ));
                    ui.weak(what);
                    ui.label("");
                    ui.weak("off");
                    ui.horizontal(|ui| {
                        let mut on = false;
                        if ui.checkbox(&mut on, "").on_hover_text("Switch it on").changed() {
                            switch = Some((d.name().to_owned(), on));
                        }
                        if ui.small_button("✎").on_hover_text("Edit").clicked() {
                            edit = Some((*d).clone());
                        }
                        if ui.small_button("−").on_hover_text("Remove (from every tab)").clicked() {
                            remove = Some(d.name().to_owned());
                        }
                    });
                    ui.end_row();
                }
            });
            if let Some((name, on)) = switch
                && let Some(d) = saved.get(&name)
            {
                let d = (**d).clone().with_on(on);
                saved.set(d);
                out.saved_changed = true;
            }
            if let Some(d) = edit {
                st.draft = Draft::of(&d);
                st.editing = Some(d.name().to_owned());
            }
            if let Some(name) = remove_arg {
                arg_picks.retain(|a| a.name != name);
                if plot.is_plotted(&name) {
                    plot.toggle(&name);
                }
                out.args_changed = true;
            }
            if let Some(name) = remove {
                saved.remove(&name);
                if st.editing.as_deref() == Some(name.as_str()) {
                    st.editing = None;
                }
                out.saved_changed = true;
            }

            lifecycles_list(ui, signals, file, check_results, &mut out);
            // A lifecycle edited: its form's fields.

            ui.separator();
            // Folded until wanted (an edit opens it): the list stays short.
            let title = match (st.editing.is_some(), st.draft.life) {
                (true, true) => "Edit lifecycle",
                (true, false) => "Edit signal",
                (false, _) => "New signal or lifecycle",
            };
            egui::CollapsingHeader::new(RichText::new(title).strong())
                .id_salt("signal_form")
                .default_open(false)
                .open(st.editing.is_some().then_some(true))
                .show(ui, |ui| {
                    ui.weak("Saved for every tab and the next start");
                    let lifecycle_names: Vec<String> = signals.lifecycles().map(|l| l.name().to_owned()).collect();
                    let d = &mut st.draft;
                    if st.editing.is_none() {
                        ui.horizontal(|ui| {
                            ui.radio_value(&mut d.life, false, "Signal");
                            ui.radio_value(&mut d.life, true, "Lifecycle").on_hover_text(
                                "When one entity (an ECU, an application) starts and how it ends, numbered. \
                                 A signal too (@name: its number); signals can reset with it.",
                            );
                        });
                    }
                    if d.life {
                        lifecycle_form(ui, d, library, signals, file);
                    } else {
                        egui::Grid::new("signal_form").num_columns(2).show(ui, |ui| {
                            ui.label("Name");
                            ui.add(
                                egui::TextEdit::singleline(&mut d.name).hint_text("speed").desired_width(f32::INFINITY),
                            );
                            ui.end_row();
                            name_taken_row(ui, signals, &d.name);
                            ui.label("Kind");
                            ui.horizontal(|ui| {
                                ui.radio_value(&mut d.kind, SignalKind::Num, "num")
                                    .on_hover_text("The capture read as a number");
                                ui.radio_value(&mut d.kind, SignalKind::Enum, "enum")
                                    .on_hover_text("The captured text");
                                ui.radio_value(&mut d.kind, SignalKind::Time, "time").on_hover_text(
                    "The capture read as a time: 2026-10-17 10:00:01.5, 10:00:01.5 (a time of day) or seconds. \
                 The first time signal is the tab's clock for plots.",
                );
                            });
                            ui.end_row();
                            ui.label("Group");
                            ui.add(
                                egui::TextEdit::singleline(&mut d.group)
                                    .hint_text("same as the name")
                                    .desired_width(f32::INFINITY),
                            )
                            .on_hover_text("The named regex group (?<group>…) the value is taken from");
                            ui.end_row();
                            ui.label("Per");
                            let shown = if d.per.is_empty() { "(no lifecycle)".to_owned() } else { d.per.clone() };
                            let before = d.per.clone();
                            egui::ComboBox::from_id_salt("signal_per")
                                .selected_text(shown)
                                .show_ui(ui, |ui| {
                                    ui.selectable_value(&mut d.per, String::new(), "(no lifecycle)");
                                    for n in &lifecycle_names {
                                        ui.selectable_value(&mut d.per, n.clone(), n);
                                    }
                                })
                                .response
                                .on_hover_text(
                                    "Reset its state each time this lifecycle starts. Its query still decides its \
                                     lines: picking a lifecycle fills an empty query with the lifecycle's lines.",
                                );
                            // An empty query starts with the lifecycle's lines (owner, 2026-11-07).
                            if d.per != before
                                && d.query.trim().is_empty()
                                && let Some(lines) = signals
                                    .lifecycles()
                                    .find(|l| l.name() == d.per)
                                    .and_then(|l| l.lines())
                                    .and_then(|r| r.query())
                            {
                                d.query = format!("{} AND ", lines.source());
                            }
                            ui.end_row();
                            ui.label("Default");
                            ui.add(
                                egui::TextEdit::singleline(&mut d.default)
                                    .hint_text("none (unset)")
                                    .desired_width(f32::INFINITY),
                            )
                            .on_hover_text(
                                "Its state where no event set it (in its lifecycle, else its file), shown as assumed. \
                             Not an event.",
                            );
                            ui.end_row();
                        });
                        ui.label("Query (NFD); a row carries the value when it matches:");
                        ui.add(
                            egui::TextEdit::multiline(&mut d.query)
                                .code_editor()
                                .desired_rows(2)
                                .desired_width(f32::INFINITY)
                                .hint_text(r"payload ~ /speed=(?<speed>\d+)/"),
                        );
                    }
                    ui.horizontal(|ui| {
                        let label = if st.editing.is_some() { "Update" } else { "Add" };
                        if ui.button(label).clicked() {
                            match st.draft.build() {
                                Ok(def) => {
                                    // An edit keeps the signal's scope, and its being off.
                                    let old = st.editing.as_deref().and_then(|n| saved.get(n));
                                    let scope = old.map(|o| o.scope()).unwrap_or_default();
                                    let on = old.is_none_or(|o| o.is_on());
                                    let def = def.with_on(on);
                                    if let Some(old) = st.editing.take()
                                        && old != def.name()
                                    {
                                        saved.remove(&old);
                                    }
                                    out.adopted = Some(def.name().to_owned());
                                    saved.set(def.with_scope(scope));
                                    st.draft = Draft::default();
                                    out.saved_changed = true;
                                }
                                Err(e) => st.draft.error = Some(e),
                            }
                        }
                        if st.editing.is_some() && ui.button("Cancel").clicked() {
                            st.editing = None;
                            st.draft = Draft::default();
                        }
                    });
                    if let Some(e) = &st.draft.error {
                        ui.colored_label(ui.visuals().error_fg_color, e);
                    }
                });
        });
    };
    match fibex {
        Some(f) => {
            // Room for the list as it is, at most half the panel at first.
            let want = (n_signals as f32 * 24.0 + 60.0).clamp(100.0, (ui.available_height() * 0.5).max(100.0));
            egui::Panel::top("signals_list")
                .resizable(true)
                .default_size(want)
                .min_size(60.0)
                .frame(egui::Frame::NONE)
                .show(ui, list);
            out.fibex = fibex_ui(ui, fibex);
            // `|=`: a − in the list above may have changed the picks too.
            out.args_changed |= arg_tree(ui, st, f, arg_picks, plot);
        }
        None => {
            let h = ui.available_height() - 60.0;
            ui.allocate_ui(egui::vec2(ui.available_width(), h.max(60.0)), list);
            ui.separator();
            out.fibex = fibex_ui(ui, fibex);
        }
    }
    out
}

/// The lifecycle form: its name, its lines, what starts one, how one ends;
/// each query is NFD or a library query (📚, or dropped from the Library);
/// Preview tries it on the file.
fn lifecycle_form(ui: &mut egui::Ui, d: &mut Draft, lib: &Library, signals: &Signals, file: &dyn LogSource) {
    let f = &mut d.life_form;
    egui::Grid::new("lifecycle_form").num_columns(2).spacing([8.0, 6.0]).show(ui, |ui| {
        ui.label("Name");
        ui.add(egui::TextEdit::singleline(&mut d.name).hint_text("ecu1").desired_width(f32::INFINITY));
        ui.end_row();
        name_taken_row(ui, signals, &d.name);
        ui.label("Its lines").on_hover_text("The lines of the entity (empty: every line), e.g. ecu == ECU1");
        rule_field(ui, "life_lines", &mut f.lines, "ecu == ECU1  (empty: every line)", lib);
        ui.end_row();
        ui.label("Starts when");
        ui.vertical(|ui| {
            ui.horizontal(|ui| {
                ui.checkbox(&mut f.on_reset, "its uptime goes back")
                    .on_hover_text("The uptime its lines carry (DLT: tmsp), or a numeric signal (@name)");
                ui.add_enabled(f.on_reset, egui::TextEdit::singleline(&mut f.reset).desired_width(70.0))
                    .on_hover_text("uptime: what its lines carry (DLT: tmsp, in seconds); or @signal");
                ui.label("by more than");
                ui.add_enabled(f.on_reset, egui::TextEdit::singleline(&mut f.tolerance).desired_width(48.0))
                    .on_hover_text("Lines of different writers interleave: smaller steps back aren't a reboot");
            });
            ui.horizontal(|ui| {
                ui.checkbox(&mut f.on_start, "one of its lines matches");
            });
            if f.on_start {
                rule_field(ui, "life_start", &mut f.start, "payload ~ /booting/", lib);
            }
            ui.horizontal(|ui| {
                ui.checkbox(&mut f.on_gap, "its lines stop for");
                ui.add_enabled(f.on_gap, egui::TextEdit::singleline(&mut f.gap).desired_width(48.0));
                ui.weak("(ends one as lost)");
            });
        });
        ui.end_row();
        ui.label("Ended as planned by");
        rule_field(ui, "life_planned", &mut f.planned, "payload ~ /shutdown/", lib);
        ui.end_row();
        ui.label("Ended as a crash by");
        rule_field(ui, "life_crash", &mut f.crash, "payload ~ /watchdog/", lib);
        ui.end_row();
        ui.label("Late start above").on_hover_text("A lifecycle whose first line has a higher uptime started late");
        ui.add(egui::TextEdit::singleline(&mut f.late).desired_width(48.0));
        ui.end_row();
    });
    ui.horizontal(|ui| {
        if ui.button("👁 Preview").on_hover_text("Find its lifecycles in this file").clicked() {
            f.preview = Some(preview(f, &d.name, lib, signals, file));
        }
        match &f.preview {
            Some(Ok(s)) => {
                ui.label(s);
            }
            Some(Err(e)) => {
                ui.colored_label(ui.visuals().error_fg_color, e);
            }
            None => {}
        }
    });
}

/// What the form's lifecycle finds in `file` (a summary), or why it can't.
fn preview(f: &LifeForm, name: &str, lib: &Library, signals: &Signals, file: &dyn LogSource) -> Result<String, String> {
    let name = if name.trim().is_empty() { "preview" } else { name };
    let def = f.build(name)?.resolved(lib)?.linked(signals);
    let l = def.lifecycles(file, &nerka_core::Progress::new()).map_err(|e| e.to_string())?;
    let first = l.lives.first().map(|x| format!(" (first on line {})", x.first + 1)).unwrap_or_default();
    Ok(format!("{}{first}", l.summary()))
}

/// A query field of a lifecycle: NFD, or a library query `{pack.query}`
/// picked with 📚 or dropped from the Library.
pub(crate) fn rule_field(ui: &mut egui::Ui, id: &str, text: &mut String, hint: &str, lib: &Library) {
    ui.horizontal(|ui| {
        let w = (ui.available_width() - 34.0).max(80.0);
        let r = ui.add(egui::TextEdit::singleline(text).code_editor().hint_text(hint).desired_width(w));
        if crate::library::hovering_query(&r) {
            ui.painter().rect_stroke(
                r.rect,
                2.0,
                egui::Stroke::new(1.5, ui.visuals().selection.stroke.color),
                egui::StrokeKind::Outside,
            );
        }
        if let Some(q) = crate::library::dropped_query(&r) {
            *text = library_rule(lib, &q);
        }
        r.on_hover_text("NFD, or a library query: {pack.query name=value} (📚, or drag one from the Library)");
        ui.menu_button("📚", |ui| {
            ui.set_max_height(320.0);
            egui::ScrollArea::vertical().id_salt(("rule_pick", id)).show(ui, |ui| {
                if lib.packs().is_empty() {
                    ui.weak("No query packs loaded");
                }
                for p in lib.packs() {
                    ui.label(RichText::new(&p.name).strong());
                    for q in &p.queries {
                        let full = format!("{}.{}", p.name, q.id);
                        let r = ui.button(&full);
                        let r = if q.note.is_empty() { r } else { r.on_hover_text(&q.note) };
                        if r.clicked() {
                            *text = library_rule(lib, &full);
                            ui.close();
                        }
                    }
                }
            });
        })
        .response
        .on_hover_text("Use a query from the library (kept by its name: a fix in its pack reaches here)");
    });
}

/// `{pack.query}`, with its parameters to fill in (`{vision.drops cam=}`).
fn library_rule(lib: &Library, id: &str) -> String {
    let params = lib.get(id).map(|(_, q)| q.params.clone()).unwrap_or_default();
    lifecycle::Rule::library(id, params.into_iter().map(|p| (p, String::new())).collect()).text()
}

/// The lifecycles found, per definition: a row each (click: go to its first
/// line; ▾ filter: filter to it), and the findings.
fn lifecycles_list(
    ui: &mut egui::Ui,
    signals: &Signals,
    file: &dyn LogSource,
    checks: &[(String, crate::checks::Outcome)],
    out: &mut Output,
) {
    for def in signals.lifecycles() {
        ui.separator();
        if let Some(q) = def.unresolved().first() {
            ui.colored_label(
                ui.visuals().error_fg_color,
                format!("Lifecycle @{}: library query {q} isn't loaded", def.name()),
            );
            continue;
        }
        let l = match def.done(file) {
            None => {
                ui.weak(format!("Lifecycles @{}: finding them…", def.name()));
                continue;
            }
            // Why none were found (owner, 2026-11-07: it said "finding them…" for good).
            Some(Err(e)) => {
                ui.colored_label(ui.visuals().error_fg_color, format!("Lifecycles @{}: {e}", def.name()))
                    .on_hover_text("Edit it (✎), or switch it off");
                continue;
            }
            Some(Ok(l)) => l,
        };
        let header = format!("Lifecycles @{}: {}", def.name(), l.summary());
        egui::CollapsingHeader::new(RichText::new(header).strong())
            .id_salt(("lifecycles", def.name()))
            .default_open(true)
            .show(ui, |ui| {
                egui::Grid::new(("lifecycles_grid", def.name())).num_columns(5).striped(true).show(ui, |ui| {
                    for h in ["#", "line", "duration", "outcome", ""] {
                        ui.weak(h);
                    }
                    ui.end_row();
                    for (i, x) in l.lives.iter().enumerate() {
                        ui.label(x.number.to_string());
                        let start = x.first_time.map(|t| nerka_core::time::format_time(t, 3)).unwrap_or_default();
                        if ui
                            .link(RichText::new((x.first + 1).to_string()).monospace())
                            .on_hover_text(format!(
                                "Started by {} on line {}{}\nClick to go there",
                                x.started_by.as_str(),
                                x.first + 1,
                                if start.is_empty() { String::new() } else { format!(" at {start}") }
                            ))
                            .clicked()
                        {
                            out.goto = Some(x.first);
                        }
                        ui.label(x.duration().map(|d| format!("{d:.1} s")).unwrap_or_default());
                        let color =
                            if x.outcome.is_bad() { ui.visuals().error_fg_color } else { ui.visuals().text_color() };
                        let r = ui.colored_label(color, x.outcome.as_str());
                        if let Some(m) = x.marker {
                            r.on_hover_text(format!("Its {} marker is on line {}", x.outcome.as_str(), m + 1));
                        }
                        ui.horizontal(|ui| {
                            if ui
                                .small_button("▾ filter")
                                .on_hover_text("Filter to its lines in this lifecycle")
                                .clicked()
                            {
                                let mut nfd = format!("@{} == {}", def.name(), x.number);
                                if let Some(lines) = def.lines().and_then(|r| r.query()) {
                                    nfd += &format!(" AND ({})", lines.source());
                                }
                                out.filter = Some(nfd);
                            }
                            // What the checks found in it (owner, 2026-11-07).
                            let (issues, facts, lines) = crate::checks::of_lifecycle(checks, def.name(), x.number);
                            if !lines.is_empty() {
                                let text = match (issues, facts) {
                                    (0, f) => RichText::new(format!("✔ ℹ{f}")).color(ui.visuals().weak_text_color()),
                                    (n, f) => RichText::new(format!("⚠{n} ℹ{f}")).color(ui.visuals().error_fg_color),
                                };
                                let tip = format!(
                                    "{}
Click: the Checks pane",
                                    lines.join(
                                        "
"
                                    )
                                );
                                if ui.link(text).on_hover_text(tip).clicked() {
                                    out.checks = Some((def.name().to_owned(), x.number));
                                }
                            }
                            for f in l.findings_of(i) {
                                let times = if f.count > 1 { format!(" ×{}", f.count) } else { String::new() };
                                let r = ui.link(
                                    RichText::new(format!("⚠ {}{times}", f.kind.as_str()))
                                        .color(ui.visuals().warn_fg_color),
                                );
                                if r.on_hover_text(format!("Line {}: {}\nClick to go there", f.row + 1, f.detail))
                                    .clicked()
                                {
                                    out.goto = Some(f.row);
                                }
                            }
                        });
                        ui.end_row();
                    }
                });
            });
    }
}

/// What follows a value in its cell (`  • event`, `  ↑ 12345`), in characters.
const SUFFIX: usize = 10;

/// How many characters of a value fit in the list's value column: the
/// panel's width less the other columns (the names as shown, at most 32
/// characters), at least 8.
fn value_room(ui: &egui::Ui, signals: &Signals) -> usize {
    let font = egui::TextStyle::Monospace.resolve(ui.style());
    let cw = ui.painter().layout_no_wrap("0".into(), font, egui::Color32::WHITE).size().x.max(1.0);
    let names = signals.iter().map(|d| d.name().chars().count() + 1).max().unwrap_or(0).min(32);
    // Plot box, kind, scope, the last column and the spaces between.
    const OTHERS: f32 = 250.0;
    let room = (ui.available_width() - OTHERS - names as f32 * cw) / cw;
    (room.max(0.0) as usize).saturating_sub(SUFFIX).max(8)
}

/// A value cut to `fit` characters (the hover has all of it).
fn short_value(v: &str, fit: usize) -> String {
    match v.char_indices().nth(fit.saturating_sub(1)) {
        Some((i, _)) if v.chars().count() > fit => format!("{}…", &v[..i]),
        _ => v.to_owned(),
    }
}

/// A time as long as fits in `fit` characters: the date and time to the µs,
/// then to the ms, then the time of day to the µs, to the ms, to the second.
fn fit_time(t: f64, fit: usize) -> String {
    let full = nerka_core::time::format_time(t, 6);
    let ms = nerka_core::time::format_time(t, 3);
    let day = |s: &str| s.rsplit(' ').next().unwrap_or(s).to_owned();
    let secs = nerka_core::time::format_time(t, 0);
    [full.clone(), ms.clone(), day(&full), day(&ms), day(&secs)]
        .into_iter()
        .find(|c| c.chars().count() <= fit)
        .unwrap_or_else(|| day(&secs))
}

/// The FIBEX files: what they describe, and buttons to add or forget them.
fn fibex_ui(ui: &mut egui::Ui, fibex: Option<&nerka_core::Fibex>) -> Option<FibexAction> {
    let mut action = None;
    ui.horizontal(|ui| {
        ui.label(RichText::new("FIBEX").strong()).on_hover_text(
            "Descriptions of DLT non-verbose messages: their payload is shown as text, and their APID, CTID and \
             level come from it (so filters and signals work on them). For every tab, this session.",
        );
        if ui.small_button("Add files…").clicked() {
            action = Some(FibexAction::Add);
        }
        if fibex.is_some() && ui.small_button("Forget").on_hover_text("Decode with no FIBEX").clicked() {
            action = Some(FibexAction::Forget);
        }
    });
    match fibex {
        Some(f) => {
            ui.weak(format!("{} messages described", f.len()));
            for file in &f.files {
                let short = file.rsplit(['/', '\\']).next().unwrap_or(file);
                ui.monospace(short).on_hover_text(file);
            }
        }
        None => {
            ui.weak("None: DLT non-verbose messages show as [id] hex.");
        }
    }
    action
}

/// The arguments FIBEX describes, as a tree: ECU, APID / CTID, message,
/// argument. Ticking an argument makes it a signal of the tab; plotting
/// one ticks it too. Returns whether the picks changed.
fn arg_tree(
    ui: &mut egui::Ui,
    st: &mut SignalsUi,
    fibex: &nerka_core::Fibex,
    picks: &mut Vec<nerka_core::Argument>,
    plot: &mut PlotState,
) -> bool {
    let key = fibex as *const nerka_core::Fibex as usize;
    if st.args.as_ref().is_none_or(|(k, _)| *k != key) {
        st.args = Some((key, fibex.arguments()));
    }
    let Some((_, args)) = &st.args else { return false };
    if args.is_empty() {
        ui.weak("No message has typed arguments.");
        return false;
    }
    ui.horizontal(|ui| {
        ui.label("Arguments");
        ui.add(
            egui::TextEdit::singleline(&mut st.arg_filter).hint_text("filter: name, text, APID…").desired_width(160.0),
        );
        if ui.small_button("Collapse all").clicked() {
            st.tree = (st.tree.0 + 1, Some(false));
        }
        if ui.small_button("Expand all").clicked() {
            st.tree = (st.tree.0 + 1, Some(true));
        }
    });
    let filter = st.arg_filter.trim().to_lowercase();
    // A new filter opens the groups that hold its matches (the others are
    // hidden); they can be folded again.
    if filter != st.tree_filter {
        st.tree = (st.tree.0 + 1, (!filter.is_empty()).then_some(true));
        st.tree_filter = filter.clone();
    }
    let shown = |a: &nerka_core::Argument| {
        filter.is_empty()
            || [&a.name, &a.label, &a.message, a.apid.as_deref().unwrap_or(""), a.ctid.as_deref().unwrap_or("")]
                .iter()
                .any(|t| t.to_lowercase().contains(&filter))
    };
    let mut changed = false;
    // A small catalog starts unfolded; a big one waits for the filter.
    let open = st.tree.1.unwrap_or(args.len() <= 50);
    let tree = st.tree.0;
    // The rest of the panel (drag the divider above for more).
    let h = ui.available_height().max(120.0);
    egui::ScrollArea::both().id_salt("fibex_args").max_height(h).auto_shrink([false, false]).show(ui, |ui| {
        // Group consecutive arguments (the list is sorted) by ECU, then
        // APID / CTID, then message.
        let mut i = 0;
        while i < args.len() {
            let ecu = &args[i].ecu;
            let end = i + args[i..].iter().take_while(|a| &a.ecu == ecu).count();
            let group: Vec<&nerka_core::Argument> = args[i..end].iter().filter(|a| shown(a)).collect();
            i = end;
            if group.is_empty() {
                continue;
            }
            let title = RichText::new(ecu.clone().unwrap_or_else(|| "(any ECU)".into())).strong();
            changed |= arg_group(ui, ("ecu", ecu, tree), open, title, &group, picks, plot, |ui, picks, plot| {
                let mut changed = false;
                let mut j = 0;
                while j < group.len() {
                    let app = (&group[j].apid, &group[j].ctid);
                    let end = j + group[j..].iter().take_while(|a| (&a.apid, &a.ctid) == app).count();
                    let apps = &group[j..end];
                    j = end;
                    let title = RichText::new(format!(
                        "{} / {}",
                        app.0.as_deref().unwrap_or("-"),
                        app.1.as_deref().unwrap_or("-")
                    ));
                    changed |=
                        arg_group(ui, ("app", ecu, app, tree), open, title, apps, picks, plot, |ui, picks, plot| {
                            let mut changed = false;
                            let mut k = 0;
                            while k < apps.len() {
                                let mid = apps[k].mid;
                                let end = k + apps[k..].iter().take_while(|a| a.mid == mid).count();
                                let msg = &apps[k..end];
                                k = end;
                                // Long templates are cut: the panel stays narrow.
                                let text = &msg[0].message;
                                let title = match text.char_indices().nth(44) {
                                    Some((i, _)) => format!("[{mid}] {}…", &text[..i]),
                                    None => format!("[{mid}] {text}"),
                                };
                                let title = RichText::new(title).monospace();
                                changed |= arg_group(
                                    ui,
                                    ("msg", ecu, mid, tree),
                                    open,
                                    title,
                                    msg,
                                    picks,
                                    plot,
                                    |ui, picks, plot| {
                                        let mut changed = false;
                                        for a in msg {
                                            changed |= arg_row(ui, a, picks, plot);
                                        }
                                        changed
                                    },
                                );
                            }
                            changed
                        });
                }
                changed
            });
        }
    });
    changed
}

/// A group of the tree (an ECU, an APID / CTID, a message) that folds, with
/// a checkbox for all its (shown) arguments: ticked when all are signals,
/// a dash when some are. Ticking it adds the others; unticking removes all
/// (and their lanes). Its 📈 plots them all (or, all plotted, none).
/// Returns whether the picks changed.
#[allow(clippy::too_many_arguments)]
fn arg_group(
    ui: &mut egui::Ui,
    salt: impl std::hash::Hash + std::fmt::Debug,
    open: bool,
    title: RichText,
    members: &[&nerka_core::Argument],
    picks: &mut Vec<nerka_core::Argument>,
    plot: &mut PlotState,
    body: impl FnOnce(&mut egui::Ui, &mut Vec<nerka_core::Argument>, &mut PlotState) -> bool,
) -> bool {
    let id = ui.make_persistent_id(salt);
    let mut changed = false;
    let state = egui::collapsing_header::CollapsingState::load_with_default_open(ui.ctx(), id, open);
    let mut title_clicked = false;
    let mut header = state.show_header(ui, |ui| {
        let picked = members.iter().filter(|a| picks.iter().any(|p| p.name == a.name)).count();
        let mut all = picked == members.len();
        let some = picked > 0 && !all;
        let r = ui.add(egui::Checkbox::new(&mut all, "").indeterminate(some));
        let tip = format!("All {} arguments as signals of this tab ({picked} are)", members.len());
        if r.on_hover_text(tip).changed() {
            pick_all(members, all, picks, plot);
            changed = true;
        }
        let plotted = members.iter().filter(|a| plot.is_plotted(&a.name)).count();
        let every = plotted == members.len();
        let tip = format!("Plot all {} arguments ({plotted} are plotted); again: none", members.len());
        if ui.small_button(if every { "📈✓" } else { "📈" }).on_hover_text(tip).clicked() {
            changed |= plot_all(members, !every, picks, plot);
        }
        // A click on the title folds it, as on a plain header.
        title_clicked = ui.add(egui::Label::new(title).sense(egui::Sense::click())).clicked();
    });
    if title_clicked {
        header.toggle();
    }
    header.body(|ui| changed |= body(ui, picks, plot));
    changed
}

/// Make all of `members` signals of the tab (`on`), or none (their lanes go
/// too).
fn pick_all(members: &[&nerka_core::Argument], on: bool, picks: &mut Vec<nerka_core::Argument>, plot: &mut PlotState) {
    for a in members {
        let picked = picks.iter().any(|p| p.name == a.name);
        if on && !picked {
            picks.push((*a).clone());
        } else if !on {
            picks.retain(|p| p.name != a.name);
            if plot.is_plotted(&a.name) {
                plot.toggle(&a.name);
            }
        }
    }
}

/// Plot all of `members` (`on`; they become signals too), or none (they stay
/// signals). Returns whether the picks changed.
fn plot_all(
    members: &[&nerka_core::Argument],
    on: bool,
    picks: &mut Vec<nerka_core::Argument>,
    plot: &mut PlotState,
) -> bool {
    let mut changed = false;
    for a in members {
        if plot.is_plotted(&a.name) != on {
            plot.toggle(&a.name);
        }
        if on && !picks.iter().any(|p| p.name == a.name) {
            picks.push((*a).clone());
            changed = true;
        }
    }
    changed
}

/// One argument: whether it is a signal, whether it is plotted.
fn arg_row(
    ui: &mut egui::Ui,
    a: &nerka_core::Argument,
    picks: &mut Vec<nerka_core::Argument>,
    plot: &mut PlotState,
) -> bool {
    let mut changed = false;
    ui.horizontal(|ui| {
        let picked = picks.iter().any(|p| p.name == a.name);
        let mut on = picked;
        if ui.checkbox(&mut on, "").on_hover_text("Use it as a signal of this tab").changed() {
            if on {
                picks.push(a.clone());
            } else {
                picks.retain(|p| p.name != a.name);
                if plot.is_plotted(&a.name) {
                    plot.toggle(&a.name);
                }
            }
            changed = true;
        }
        let mut plotted = plot.is_plotted(&a.name);
        if ui.small_button(if plotted { "📈✓" } else { "📈" }).on_hover_text("Plot it").clicked() {
            plotted = !plotted;
            plot.toggle(&a.name);
            if plotted && !picked {
                picks.push(a.clone());
                changed = true;
            }
        }
        ui.label(RichText::new(format!("@{}", a.name)).monospace());
        ui.weak(a.ty.name());
        if !a.label.is_empty() {
            ui.weak(format!("\"{}\"", a.label));
        }
    });
    changed
}

/// The kind: a choice for a query signal (inferred from its values until
/// chosen), else a label.
fn kind_cell(ui: &mut egui::Ui, d: &SignalDef, out: &mut Output) {
    if d.as_lifecycle().is_some() {
        ui.weak("lifecycle");
        return;
    }
    if d.queries().is_empty() {
        ui.weak(d.kind().as_str());
        return;
    }
    let mut kind = d.kind();
    egui::ComboBox::from_id_salt(("kind", d.name())).width(48.0).selected_text(kind.as_str()).show_ui(ui, |ui| {
        for k in [SignalKind::Num, SignalKind::Enum, SignalKind::Time] {
            ui.selectable_value(&mut kind, k, k.as_str());
        }
    });
    if kind != d.kind() {
        out.kind = Some((d.name().to_owned(), kind));
    }
}

/// A grid row saying that a check makes a signal of this name: the new
/// signal takes it, and the check's isn't made (owner, 2026-11-07: that
/// was silent).
fn name_taken_row(ui: &mut egui::Ui, signals: &Signals, name: &str) {
    let Some(check) = signals.get(name.trim()).and_then(|d| d.check_name()) else { return };
    ui.label("");
    ui.colored_label(
        ui.visuals().warn_fg_color,
        format!("⚠ Check '{check}' makes @{}: this one takes the name, and the check's signal isn't made", name.trim()),
    );
    ui.end_row();
}

/// What a check's signal is (owner, 2026-11-07: listed with the others,
/// to try).
fn check_hover(check: &str, d: &SignalDef) -> String {
    let per = d.per().map(|p| format!(", from 0 again in each lifecycle of {p}")).unwrap_or_default();
    let value = if d.default_value().is_some() {
        format!("its value counts them{per}")
    } else {
        "its value is what the fact measures there".to_owned()
    };
    format!(
        "From check '{check}': an event where it finds something; {value}.\nUse it in queries and other checks \
         (@{0}.event, @{0} > 0). Edit or switch it off in the Checks pane.",
        d.name()
    )
}

/// The scope: all lines, or only the lines the filter keeps.
fn scope_cell(ui: &mut egui::Ui, d: &SignalDef, out: &mut Output) {
    if d.is_builtin() {
        ui.label("");
        return;
    }
    let filtered = d.scope() == SignalScope::Filtered;
    let r = ui.selectable_label(filtered, if filtered { "filtered" } else { "all" }).on_hover_text(
        "all: events from every line (a filter can use it)\nfiltered: only from the lines the filter keeps \
         (panel and plot only). Click to switch.",
    );
    if r.clicked() {
        let to = if filtered { SignalScope::All } else { SignalScope::Filtered };
        out.scope = Some((d.name().to_owned(), to));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The form makes what it is filled with, lifecycles and bound
    /// signals too, and says what is wrong.
    #[test]
    fn the_form_round_trips_lifecycles_and_bound_signals() {
        for spec in [
            "lifecycle ecu1: lines ecu == ECU1; reset uptime; planned {sys.shutdown}",
            "lifecycle run: start raw ~ /boot/; gap 30s; crash raw ~ /panic/; late 20s",
            "lifecycle up: reset @up tolerance 500ms",
            r"ota(s):enum:per(ecu1):default(idle)=/OTA state=(?<s>\w+)/",
            r"n:num:default(0)=/n=(?<n>\d+)/",
        ] {
            let d = SignalDef::parse_spec(spec).unwrap();
            assert_eq!(Draft::of(&d).build().unwrap().spec(), d.spec(), "{spec}");
        }
        // Each field's error names the field.
        let life = |f: LifeForm| Draft { life: true, name: "ecu1".into(), life_form: f, ..Draft::default() }.build();
        let e = life(LifeForm { lines: "ecu ==".into(), ..LifeForm::default() }).unwrap_err();
        assert!(e.starts_with("Its lines: "), "{e}");
        let e = life(LifeForm { on_gap: true, gap: "soon".into(), ..LifeForm::default() }).unwrap_err();
        assert_eq!(e, "Its lines stop: 'soon' isn't a duration (30s, 500ms, 2m)");
        let e = life(LifeForm { on_reset: false, ..LifeForm::default() }).unwrap_err();
        assert_eq!(e, "a lifecycle needs a start, reset or gap clause");
        let e = life(LifeForm { reset: "tmsp".into(), ..LifeForm::default() }).unwrap_err();
        assert!(e.contains("isn't uptime or a signal"), "{e}");
        // A library query from the picker comes with its parameters to fill.
        let dir = std::env::temp_dir().join(format!("nerka-gui-rule-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let pack = dir.join("v.nerka-pack");
        std::fs::write(
            &pack,
            "pack v

query drops(cam)
  raw ~ /$cam drop/
",
        )
        .unwrap();
        let mut lib = Library::new();
        lib.load(&pack).unwrap();
        assert_eq!(library_rule(&lib, "v.drops"), "{v.drops cam=}");
        std::fs::remove_dir_all(&dir).ok();
        let draft =
            Draft { name: "n".into(), query: r"/n=(?<n>\d+)/".into(), default: "many".into(), ..Draft::default() };
        assert_eq!(draft.build().unwrap_err(), "default 'many' isn't a number");
    }

    fn fibex() -> nerka_core::Fibex {
        // A folder per call: tests run in parallel, and each removes its own.
        static CALLS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let n = CALLS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("nerka-gui-tree-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let xml = dir.join("q.xml");
        std::fs::write(&xml, crate::tests::QUEUE_FIBEX).unwrap();
        let f = nerka_core::Fibex::load(&[&xml]).unwrap();
        std::fs::remove_dir_all(&dir).ok();
        f
    }

    /// The texts the tree draws.
    fn tree(
        ctx: &egui::Context,
        st: &mut SignalsUi,
        f: &nerka_core::Fibex,
        picks: &mut Vec<nerka_core::Argument>,
    ) -> Vec<String> {
        let mut plot = PlotState::default();
        let mut texts = Vec::new();
        for _ in 0..2 {
            // Forward in time, so a group's opening animation has ended.
            let t = ctx.input(|i| i.time) + 1.0;
            let input = egui::RawInput { time: Some(t), ..Default::default() };
            let mut out = ctx.run_ui(input, |ui| {
                egui::CentralPanel::default().show(ui, |ui| arg_tree(ui, st, f, picks, &mut plot));
            });
            out.textures_delta.clear();
            texts = out
                .shapes
                .iter()
                .filter_map(|s| match &s.shape {
                    egui::Shape::Text(t) => Some(t.galley.text().to_owned()),
                    _ => None,
                })
                .collect();
        }
        texts
    }

    /// Owner, 2026-10-28: the tree folds all at once, and a filter opens
    /// the groups with matches, which fold again by themselves.
    #[test]
    fn the_fibex_tree_folds_and_its_filter_opens_matches() {
        let f = fibex();
        let ctx = egui::Context::default();
        let mut st = SignalsUi::default();
        let mut picks = Vec::new();
        assert!(tree(&ctx, &mut st, &f, &mut picks).contains(&"@free_slots".to_owned()), "a small catalog is open");
        st.tree = (st.tree.0 + 1, Some(false));
        let shown = tree(&ctx, &mut st, &f, &mut picks);
        assert!(shown.contains(&"ECU1".to_owned()) && !shown.contains(&"@free_slots".to_owned()), "{shown:?}");
        st.arg_filter = "free".into();
        assert!(tree(&ctx, &mut st, &f, &mut picks).contains(&"@free_slots".to_owned()), "the filter opens it");
        st.arg_filter = "nothing like it".into();
        assert!(!tree(&ctx, &mut st, &f, &mut picks).contains(&"ECU1".to_owned()), "no match: hidden");
    }

    /// Owner, 2026-10-28: values use the room the panel has; a time is shown
    /// whole when it fits, else shorter.
    #[test]
    fn values_and_times_fit_the_room() {
        nerka_core::time::set_local(false);
        let t = 1_760_000_000.123_456; // 2025-10-09 08:53:20.123456 UTC
        assert_eq!(fit_time(t, 40), "2025-10-09 08:53:20.123456");
        assert_eq!(fit_time(t, 23), "2025-10-09 08:53:20.123");
        assert_eq!(fit_time(t, 20), "08:53:20.123456");
        assert_eq!(fit_time(t, 12), "08:53:20.123");
        assert_eq!(fit_time(t, 8), "08:53:20");
        assert_eq!(fit_time(t, 3), "08:53:20", "never less than the second");
        assert_eq!(short_value("short", 8), "short");
        assert_eq!(short_value("a long enum value", 8), "a long …");
        assert_eq!(short_value("żółć żółć", 5), "żółć…");

        // A wide panel has room for the whole date and time; a narrow one
        // for less.
        let mut sig = Signals::default();
        sig.set(SignalDef::parse_spec(r"speed=/v=(?<speed>\d+)/").unwrap());
        let room = |w: f32| {
            let ctx = egui::Context::default();
            let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(w, 400.0));
            let mut fit = 0;
            let mut out = ctx.run_ui(egui::RawInput { screen_rect: Some(screen), ..Default::default() }, |ui| {
                egui::CentralPanel::default().show(ui, |ui| fit = value_room(ui, &sig));
            });
            out.textures_delta.clear();
            fit
        };
        assert!(room(900.0) >= 26, "{}", room(900.0));
        assert!(room(460.0) < 26, "{}", room(460.0));
        assert_eq!(room(100.0), 8, "at least 8");
    }

    /// Owner, 2026-10-28: a whole group in or out at once.
    #[test]
    fn a_group_is_picked_and_unpicked_at_once() {
        let f = fibex();
        let args = f.arguments();
        let members: Vec<&nerka_core::Argument> = args.iter().collect();
        let (mut picks, mut plot) = (Vec::new(), PlotState::default());
        pick_all(&members, true, &mut picks, &mut plot);
        pick_all(&members, true, &mut picks, &mut plot);
        assert_eq!(picks.len(), args.len(), "once each");
        plot.toggle(&args[0].name);
        pick_all(&members, false, &mut picks, &mut plot);
        assert!(picks.is_empty() && plot.plotted.is_empty(), "their lanes go too");

        // 📈 on a group: all plotted (and picked), then none (still picked).
        assert!(plot_all(&members, true, &mut picks, &mut plot));
        assert_eq!((picks.len(), plot.plotted.len()), (args.len(), args.len()));
        assert!(!plot_all(&members, false, &mut picks, &mut plot));
        assert_eq!((picks.len(), plot.plotted.len()), (args.len(), 0));
    }
}
