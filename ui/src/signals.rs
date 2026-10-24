//! The Signals panel: the tab's signals (`doc/sources-and-signals.md` →
//! "What a signal is"), each with its state on the current line. They come
//! from the file (`@time`), from the user's saved definitions (edited
//! here) and from the named groups of the selection's queries (kind and
//! scope chosen here). Core does the capturing; the panel only edits
//! definitions and shows values.

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
}

pub fn signals_panel(ui: &mut egui::Ui, st: &mut SignalsUi, input: Input<'_>) -> Output {
    let Input { signals, generation, saved, file, line, plot, views, kept } = input;
    let mut out = Output::default();
    ui.horizontal(|ui| {
        ui.heading("Signals");
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            out.close = crate::close_button(ui).on_hover_text("Close panel").clicked();
        });
    });
    ui.weak(
        "Values captured by a regex group. An event is a line that carries one; the state holds it until the \
         next. Filter with @name (the state), @name.event or @name.changed. Named groups of the selection's \
         included and signal (~) queries are signals too.",
    );
    ui.separator();

    if st.bound_gen != Some(generation) {
        st.bound =
            signals.iter().map(|d| (d.name().to_owned(), d.bind(file.schema()).map_err(|e| e.to_string()))).collect();
        st.bound_gen = Some(generation);
    }

    if signals.is_empty() {
        ui.weak("No signals in this tab yet.");
    }
    let mut remove = None;
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
            ui.label(RichText::new(format!("@{}", d.name())).monospace().strong()).on_hover_text(hover);
            kind_cell(ui, d, &mut out);
            scope_cell(ui, d, &mut out);
            if d.is_from_source() {
                let t = file.time(line).map_or_else(|| "—".to_owned(), |t| nerka_core::time::format_time(t, 6));
                ui.label(RichText::new(t).monospace()).on_hover_text("Every line is an event");
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
                            let v = d.format(v);
                            let events = series.len();
                            if row == line {
                                ui.label(RichText::new(format!("{v}  • event")).monospace())
                                    .on_hover_text(format!("Received on line {} ({events} events)", line + 1));
                            } else {
                                let r = ui.link(RichText::new(format!("{v}  ↑ {}", row + 1)).monospace());
                                if r.on_hover_text(format!(
                                    "State on line {}, set by the event on line {} ({events} events). \
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
    if let Some(name) = remove {
        saved.remove(&name);
        if st.editing.as_deref() == Some(name.as_str()) {
            st.editing = None;
        }
        out.saved_changed = true;
    }

    ui.separator();
    ui.label(RichText::new(if st.editing.is_some() { "Edit signal" } else { "New signal" }).strong())
        .on_hover_text("Saved for every tab and the next start");
    let d = &mut st.draft;
    egui::Grid::new("signal_form").num_columns(2).show(ui, |ui| {
        ui.label("Name");
        ui.add(egui::TextEdit::singleline(&mut d.name).hint_text("speed").desired_width(f32::INFINITY));
        ui.end_row();
        ui.label("Kind");
        ui.horizontal(|ui| {
            ui.radio_value(&mut d.kind, SignalKind::Num, "num").on_hover_text("The capture read as a number");
            ui.radio_value(&mut d.kind, SignalKind::Enum, "enum").on_hover_text("The captured text");
            ui.radio_value(&mut d.kind, SignalKind::Time, "time").on_hover_text(
                "The capture read as a time: 2026-10-17 10:00:01.5, 10:00:01.5 (a time of day) or seconds. \
                 The first time signal is the tab's clock for plots.",
            );
        });
        ui.end_row();
        ui.label("Group");
        ui.add(egui::TextEdit::singleline(&mut d.group).hint_text("same as the name").desired_width(f32::INFINITY))
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
                    let scope = st.editing.as_deref().and_then(|n| saved.get(n)).map(|o| o.scope()).unwrap_or_default();
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
    out
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
