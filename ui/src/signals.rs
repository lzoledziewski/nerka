//! The Signals panel: the tab's signals (`doc/sources-and-signals.md` →
//! "What a signal is"), each with its state on the current line. They come
//! from the file (`@time`), from the user's saved definitions (edited
//! here) and from the named groups of the selection's queries (kind and
//! scope chosen here). The FIBEX files that describe DLT non-verbose
//! messages are chosen here too (owner, 2026-10-24). Core does the
//! capturing and decoding; the panel only edits definitions and shows
//! values.

use crate::plot::PlotState;
use eframe::egui::{self, RichText};
use nerka_core::{LineNo, LogSource, SearchResult, SignalDef, SignalKind, SignalScope, SignalSeries, Signals};
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
    name: String,
    kind: SignalKind,
    group: String,
    query: String,
    error: Option<String>,
}

impl Default for Draft {
    fn default() -> Self {
        Draft { name: String::new(), kind: SignalKind::Num, group: String::new(), query: String::new(), error: None }
    }
}

/// What happened in the panel this frame.
#[derive(Default)]
pub struct Output {
    pub close: bool,
    /// A saved signal was added, changed or removed.
    pub saved_changed: bool,
    /// A kind chosen for a query signal.
    pub kind: Option<(String, SignalKind)>,
    /// A scope chosen for a signal.
    pub scope: Option<(String, SignalScope)>,
    /// Go to this line (the event that set a state was clicked).
    pub goto: Option<LineNo>,
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
}

pub fn signals_panel(ui: &mut egui::Ui, st: &mut SignalsUi, input: Input<'_>) -> Output {
    let Input { signals, generation, saved, file, line, plot, views, kept, fibex, arg_picks } = input;
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
            let mut remove_arg: Option<String> = None;
            let mut edit = None;
            egui::Grid::new("signals").num_columns(6).striped(true).show(ui, |ui| {
                for d in signals.iter() {
                    // Plotting a time against time says nothing.
                    if d.kind() == SignalKind::Time {
                        ui.label("");
                    } else {
                        let mut on = plot.is_plotted(d.name());
                        if ui.checkbox(&mut on, "").on_hover_text("Plot it (its state over time)").changed() {
                            plot.toggle(d.name());
                        }
                    }
                    let hover = if d.is_from_source() {
                        "time\nThe file's own time of each line (DLT: when the logger received it)".to_owned()
                    } else {
                        format!("group: {}\n{}", d.group(), d.query().source())
                    };
                    // Long names keep their end, where they differ.
                    let name = crate::plot::name_tail(&format!("@{}", d.name()), 32);
                    ui.label(RichText::new(name).monospace().strong()).on_hover_text(format!("@{}\n{hover}", d.name()));
                    kind_cell(ui, d, &mut out);
                    scope_cell(ui, d, &mut out);
                    if d.is_from_source() {
                        let (shown, t) = match file.time(line) {
                            Some(t) => (fit_time(t, fit + SUFFIX), nerka_core::time::format_time(t, 6)),
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
                            Some(series) => match series.state(line) {
                                Some((row, v)) => {
                                    let full = d.format(v);
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
                    if d.queries().is_empty() {
                        ui.horizontal(|ui| {
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
            });
            if let Some(d) = edit {
                st.draft = Draft {
                    name: d.name().to_owned(),
                    kind: d.kind(),
                    group: if d.group() == d.name() { String::new() } else { d.group().to_owned() },
                    query: d.query().source().to_owned(),
                    error: None,
                };
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

            ui.separator();
            // Folded until wanted (an edit opens it): the list stays short.
            let title = if st.editing.is_some() { "Edit signal" } else { "New signal" };
            egui::CollapsingHeader::new(RichText::new(title).strong())
                .id_salt("signal_form")
                .default_open(false)
                .open(st.editing.is_some().then_some(true))
                .show(ui, |ui| {
                    ui.weak("Saved for every tab and the next start");
                    let d = &mut st.draft;
                    egui::Grid::new("signal_form").num_columns(2).show(ui, |ui| {
                        ui.label("Name");
                        ui.add(egui::TextEdit::singleline(&mut d.name).hint_text("speed").desired_width(f32::INFINITY));
                        ui.end_row();
                        ui.label("Kind");
                        ui.horizontal(|ui| {
                            ui.radio_value(&mut d.kind, SignalKind::Num, "num")
                                .on_hover_text("The capture read as a number");
                            ui.radio_value(&mut d.kind, SignalKind::Enum, "enum").on_hover_text("The captured text");
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
                    });
                    ui.label("Query (NFD); a row carries the value when it matches:");
                    ui.add(
                        egui::TextEdit::multiline(&mut d.query)
                            .code_editor()
                            .desired_rows(2)
                            .desired_width(f32::INFINITY)
                            .hint_text(r"payload ~ /speed=(?<speed>\d+)/"),
                    );
                    ui.horizontal(|ui| {
                        let label = if st.editing.is_some() { "Update" } else { "Add" };
                        if ui.button(label).clicked() {
                            let d = &st.draft;
                            let group = (!d.group.trim().is_empty()).then_some(d.group.as_str());
                            match SignalDef::new(&d.name, d.kind, &d.query, group) {
                                Ok(def) => {
                                    // An edit keeps the signal's scope.
                                    let scope = st
                                        .editing
                                        .as_deref()
                                        .and_then(|n| saved.get(n))
                                        .map(|o| o.scope())
                                        .unwrap_or_default();
                                    if let Some(old) = st.editing.take()
                                        && old != def.name()
                                    {
                                        saved.remove(&old);
                                    }
                                    saved.set(def.with_scope(scope));
                                    st.draft = Draft::default();
                                    out.saved_changed = true;
                                }
                                Err(e) => st.draft.error = Some(e.to_string()),
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

/// The scope: all lines, or only the lines the filter keeps.
fn scope_cell(ui: &mut egui::Ui, d: &SignalDef, out: &mut Output) {
    if d.is_from_source() {
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

    fn fibex() -> nerka_core::Fibex {
        let dir = std::env::temp_dir().join(format!("nerka-gui-tree-{}", std::process::id()));
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
