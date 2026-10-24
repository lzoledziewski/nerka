//! The Signals panel: the tab's signal definitions (`doc/sources-and-signals.md`
//! → "What a signal is"), each with its value on the current line. Core
//! does the capturing; the panel only edits definitions and shows values.

use eframe::egui::{self, RichText};
use nerka_core::{LineNo, LogSource, SignalDef, SignalKind, Signals};

/// The panel's state in one tab.
pub struct SignalsUi {
    draft: Draft,
    /// The signal the form edits (its name), or none for a new one.
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

impl Default for SignalsUi {
    fn default() -> Self {
        Self {
            draft: Draft {
                name: String::new(),
                kind: SignalKind::Num,
                group: String::new(),
                query: String::new(),
                error: None,
            },
            editing: None,
            bound: Vec::new(),
            bound_gen: None,
        }
    }
}

/// What happened in the panel this frame.
#[derive(Default)]
pub struct Output {
    pub close: bool,
    /// A definition was added, changed or removed.
    pub changed: bool,
    /// Go to this line (the event that set a state was clicked).
    pub goto: Option<LineNo>,
}

/// `generation` changes whenever `signals` does, so bound definitions are made
/// again only then.
pub fn signals_panel(
    ui: &mut egui::Ui,
    st: &mut SignalsUi,
    signals: &mut Signals,
    generation: u64,
    file: &dyn LogSource,
    line: LineNo,
) -> Output {
    let mut out = Output::default();
    ui.horizontal(|ui| {
        ui.heading("Signals");
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            out.close = crate::close_button(ui).on_hover_text("Close panel").clicked();
        });
    });
    ui.weak(
        "Values captured by a regex group. An event is a line that carries one; the state holds it until the \
         next. Filter with @name (the state), @name.event or @name.changed.",
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
    egui::Grid::new("signals").num_columns(4).striped(true).show(ui, |ui| {
        for d in signals.iter() {
            ui.label(RichText::new(format!("@{}", d.name())).monospace().strong()).on_hover_text(format!(
                "{}\ngroup: {}\n{}",
                d.kind().as_str(),
                d.group(),
                d.query().source()
            ));
            ui.weak(d.kind().as_str());
            let bound = st.bound.iter().find(|(n, _)| n == d.name()).map(|(_, b)| b);
            match bound {
                Some(Ok(_)) => match d.cached_series(file) {
                    // The state on the cursor line, and the event that set it.
                    Some(series) => match series.state(line) {
                        Some((row, v)) => {
                            let v = v.to_value().to_string();
                            let events = series.len();
                            if row == line {
                                ui.label(RichText::new(format!("{v}  • event")).monospace())
                                    .on_hover_text(format!("Received on line {} ({events} events in all)", line + 1));
                            } else {
                                let r = ui.link(RichText::new(format!("{v}  ↑ {}", row + 1)).monospace());
                                if r.on_hover_text(format!(
                                    "State on line {}, set by the event on line {} ({events} events in all). \
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
            ui.horizontal(|ui| {
                if ui.small_button("✎").on_hover_text("Edit").clicked() {
                    edit = Some((*d).clone());
                }
                if ui.small_button("−").on_hover_text("Remove").clicked() {
                    remove = Some(d.name().to_owned());
                }
            });
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
        signals.remove(&name);
        if st.editing.as_deref() == Some(name.as_str()) {
            st.editing = None;
        }
        out.changed = true;
    }

    ui.separator();
    ui.label(RichText::new(if st.editing.is_some() { "Edit signal" } else { "New signal" }).strong());
    let d = &mut st.draft;
    egui::Grid::new("signal_form").num_columns(2).show(ui, |ui| {
        ui.label("Name");
        ui.add(egui::TextEdit::singleline(&mut d.name).hint_text("speed").desired_width(f32::INFINITY));
        ui.end_row();
        ui.label("Kind");
        ui.horizontal(|ui| {
            ui.radio_value(&mut d.kind, SignalKind::Num, "num").on_hover_text("The capture read as a number");
            ui.radio_value(&mut d.kind, SignalKind::Enum, "enum").on_hover_text("The captured text");
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
                    if let Some(old) = st.editing.take()
                        && old != def.name()
                    {
                        signals.remove(&old);
                    }
                    signals.set(def);
                    st.draft = SignalsUi::default().draft;
                    out.changed = true;
                }
                Err(e) => st.draft.error = Some(e.to_string()),
            }
        }
        if st.editing.is_some() && ui.button("Cancel").clicked() {
            st.editing = None;
            st.draft = SignalsUi::default().draft;
        }
    });
    if let Some(e) = &st.draft.error {
        ui.colored_label(ui.visuals().error_fg_color, e);
    }
    out
}
