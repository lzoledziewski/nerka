//! The Checks pane (`doc/checks.md`): the user's checks grouped by label,
//! their results on the tab's file (overall and per lifecycle) and the
//! evidence, and a form to define one. Core runs them
//! ([`nerka_core::check`]); the pane shows results and edits definitions
//! (owner, 2026-11-07: built in small steps with feedback).

use eframe::egui::{self, RichText};
use nerka_core::check::{CheckDef, CheckResult, Checks, Kind, Verdict};
use nerka_core::{Library, LineNo, LogSource, Signals};

/// A check's result on the tab's file, or why it couldn't run.
pub type Outcome = Result<CheckResult, String>;

/// The pane's state in one tab.
#[derive(Default)]
pub struct ChecksUi {
    form: Form,
    /// The form is open (a new check, or `editing` one).
    form_open: bool,
    /// The saved check the form edits, or none for a new one.
    editing: Option<String>,
    /// The result whose evidence shows: (check, scope index).
    pub selected: Option<(String, usize)>,
}

/// The form's fields.
struct Form {
    name: String,
    lines: String,
    test: TestChoice,
    kind: Kind,
    /// A condition on a count or a value: an operator (empty: none) and a number.
    op: String,
    number: String,
    /// `rate`'s count, and the duration of `rate`, `gap`, `then`, `missing`.
    max: String,
    window: String,
    other: String,
    pick: String,
    per: String,
    labels: String,
    error: Option<String>,
    preview: Option<Result<String, String>>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum TestChoice {
    Exists,
    Count,
    Rate,
    Gap,
    Then,
    Missing,
    Value,
}

impl TestChoice {
    const ALL: [TestChoice; 7] = [
        TestChoice::Exists,
        TestChoice::Count,
        TestChoice::Rate,
        TestChoice::Gap,
        TestChoice::Then,
        TestChoice::Missing,
        TestChoice::Value,
    ];

    fn label(self) -> &'static str {
        match self {
            TestChoice::Exists => "shows up",
            TestChoice::Count => "how many",
            TestChoice::Rate => "too many at once",
            TestChoice::Gap => "stops for too long",
            TestChoice::Then => "followed by",
            TestChoice::Missing => "not followed by",
            TestChoice::Value => "a value",
        }
    }

    /// What it shows as an issue, and as a fact.
    fn help(self) -> (&'static str, &'static str) {
        match self {
            TestChoice::Exists => ("each of its lines", "how many lines"),
            TestChoice::Count => ("the count compares so (none: any line)", "how many lines"),
            TestChoice::Rate => ("each burst of more than N within the window", "the most lines in one window"),
            TestChoice::Gap => ("each stop longer than this", "the longest stop"),
            TestChoice::Then => ("each line followed by the other within the time", "how often"),
            TestChoice::Missing => ("each line not followed by the other within the time", "how often"),
            TestChoice::Value => {
                ("the value compares so", "the value its query's first named group (?<name>…) captures")
            }
        }
    }

    /// A count or a value: what a condition applies to.
    fn has_condition(self) -> bool {
        matches!(self, TestChoice::Count | TestChoice::Value)
    }
}

impl Default for Form {
    fn default() -> Self {
        Form {
            name: String::new(),
            lines: String::new(),
            test: TestChoice::Exists,
            kind: Kind::Issue,
            op: String::new(),
            number: "1".into(),
            max: "10".into(),
            window: "10s".into(),
            other: String::new(),
            pick: "max".into(),
            per: String::new(),
            labels: String::new(),
            error: None,
            preview: None,
        }
    }
}

impl Form {
    /// The spec line the form describes.
    fn spec(&self) -> String {
        let cond = if self.test.has_condition() && !self.op.is_empty() {
            format!(" {} {}", self.op, self.number.trim())
        } else {
            String::new()
        };
        let test = match self.test {
            TestChoice::Exists => "exists".to_owned(),
            TestChoice::Count => format!("count{cond}"),
            TestChoice::Rate => format!("rate {} per {}", self.max.trim(), self.window.trim()),
            TestChoice::Gap => format!("gap {}", self.window.trim()),
            TestChoice::Then => format!("then {} within {}", self.other.trim(), self.window.trim()),
            TestChoice::Missing => format!("missing {} within {}", self.other.trim(), self.window.trim()),
            TestChoice::Value => format!("value {}{cond}", self.pick),
        };
        let mut spec = format!("{}: lines {}; {test}; {}", self.name.trim(), self.lines.trim(), self.kind.as_str());
        if !self.per.is_empty() {
            spec += &format!("; per {}", self.per);
        }
        if !self.labels.trim().is_empty() {
            spec += &format!("; labels {}", self.labels.trim());
        }
        spec
    }

    /// The form filled with a saved check.
    fn of(c: &CheckDef) -> Form {
        let mut f = Form { kind: c.kind(), labels: c.labels().join(", "), ..Form::default() };
        let spec = c.spec();
        let spec = spec.strip_prefix("off ").unwrap_or(&spec);
        let Some((name, rest)) = spec.split_once(':') else { return f };
        f.name = name.trim().into();
        let cond = |f: &mut Form, r: &str| {
            if let Some((op, n)) = r.split_once(' ') {
                (f.op, f.number) = (op.into(), n.into());
            }
        };
        for clause in rest.split(';').map(str::trim) {
            let (w, r) = clause.split_once(' ').unwrap_or((clause, ""));
            let within = |r: &str| match r.rsplit_once(" within ") {
                Some((q, d)) => (q.to_owned(), d.to_owned()),
                None => (r.to_owned(), "10s".into()),
            };
            match w {
                "lines" => f.lines = r.into(),
                "per" => f.per = r.into(),
                "exists" => f.test = TestChoice::Exists,
                "count" => {
                    f.test = TestChoice::Count;
                    cond(&mut f, r);
                }
                "rate" => {
                    f.test = TestChoice::Rate;
                    if let Some((n, d)) = r.split_once(" per ") {
                        (f.max, f.window) = (n.into(), d.into());
                    }
                }
                "gap" => (f.test, f.window) = (TestChoice::Gap, r.into()),
                "then" => (f.test, (f.other, f.window)) = (TestChoice::Then, within(r)),
                "missing" => (f.test, (f.other, f.window)) = (TestChoice::Missing, within(r)),
                "value" => {
                    f.test = TestChoice::Value;
                    let (pick, more) = r.split_once(' ').unwrap_or((r, ""));
                    f.pick = pick.into();
                    cond(&mut f, more);
                }
                _ => {}
            }
        }
        f
    }
}

/// What the pane shows and edits.
pub struct Input<'a> {
    /// The user's checks (every tab has them).
    pub saved: &'a mut Checks,
    /// Their results on this tab's file (by name), and whether they run.
    pub results: &'a [(String, Outcome)],
    pub running: bool,
    pub file: &'a dyn LogSource,
    pub signals: &'a Signals,
    pub library: &'a Library,
}

#[derive(Default)]
pub struct Output {
    pub close: bool,
    /// A check was added, changed, switched or removed.
    pub changed: bool,
    pub goto: Option<LineNo>,
}

/// What a check's checkbox, ✎ or − asked for.
enum Act {
    Switch(String, bool),
    SwitchLabel(String, bool),
    Edit(String),
    Remove(String),
}

pub fn checks_panel(ui: &mut egui::Ui, st: &mut ChecksUi, input: Input<'_>) -> Output {
    let Input { saved, results, running, file, signals, library } = input;
    let mut out = Output::default();
    ui.horizontal(|ui| {
        ui.heading("Checks");
        ui.weak("ℹ").on_hover_text(
            "Issues (a known problem shows up) and facts (a count, a value) found in the file, overall and per \
             lifecycle. Click a result for the lines that show it. Labels group checks: switch a group on or off \
             at once.",
        );
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            out.close = crate::close_button(ui).on_hover_text("Close panel").clicked();
            if running {
                ui.spinner();
            }
            if ui.button("➕ New check").clicked() {
                st.form = Form::default();
                st.editing = None;
                st.form_open = true;
            }
        });
    });
    let issues = results
        .iter()
        .filter(|(_, r)| r.as_ref().is_ok_and(|r| matches!(r.overall().verdict, Verdict::Issue(_))))
        .count();
    let facts = results.iter().filter(|(_, r)| r.as_ref().is_ok_and(|r| r.kind == Kind::Fact)).count();
    if !saved.is_empty() {
        ui.label(format!(
            "{issues} issue{} found · {facts} fact{}",
            if issues == 1 { "" } else { "s" },
            if facts == 1 { "" } else { "s" }
        ));
    }

    // The form, at the top while open.
    if st.form_open {
        egui::Frame::group(ui.style()).show(ui, |ui| {
            let title = if st.editing.is_some() { "Edit check" } else { "New check" };
            ui.label(RichText::new(title).strong());
            form_ui(ui, st, saved, &mut out, file, signals, library);
        });
    }

    // The checks, by label (a check with several shows under each).
    let mut act = None;
    egui::ScrollArea::vertical().id_salt("checks_list").max_height(ui.available_height() * 0.6).show(ui, |ui| {
        if saved.is_empty() {
            ui.weak("No checks yet: ➕ New check.");
        }
        let mut groups: Vec<(Option<String>, Vec<&CheckDef>)> =
            saved.labels().into_iter().map(|l| (Some(l), Vec::new())).collect();
        groups.push((None, Vec::new()));
        for c in saved.iter() {
            for (label, members) in groups.iter_mut() {
                let mine = match label {
                    Some(l) => c.labels().contains(l),
                    None => c.labels().is_empty(),
                };
                if mine {
                    members.push(c);
                }
            }
        }
        let labelled = groups.len() > 1;
        for (label, members) in groups.iter().filter(|(_, m)| !m.is_empty()) {
            if labelled {
                ui.horizontal(|ui| {
                    match label {
                        Some(l) => {
                            let mut on = members.iter().all(|c| c.is_on());
                            let tip = "Switch every check of this label on or off";
                            if ui
                                .checkbox(&mut on, RichText::new(format!("🏷 {l}")).strong())
                                .on_hover_text(tip)
                                .changed()
                            {
                                act = Some(Act::SwitchLabel(l.clone(), on));
                            }
                        }
                        None => {
                            ui.weak("no label");
                        }
                    }
                    ui.weak(format!("{} check{}", members.len(), if members.len() == 1 { "" } else { "s" }));
                });
            }
            for c in members {
                ui.indent((label, c.name()), |ui| check_row(ui, st, c, results, running, &mut act));
            }
        }
    });
    match act {
        Some(Act::Switch(name, on)) => {
            if let Some(c) = saved.get(&name) {
                let c = c.clone().with_on(on);
                saved.set(c);
                out.changed = true;
            }
        }
        Some(Act::SwitchLabel(label, on)) => {
            saved.set_label_on(&label, on);
            out.changed = true;
        }
        Some(Act::Remove(name)) => {
            saved.remove(&name);
            out.changed = true;
        }
        Some(Act::Edit(name)) => {
            if let Some(c) = saved.get(&name) {
                st.form = Form::of(c);
                st.editing = Some(name);
                st.form_open = true;
            }
        }
        None => {}
    }

    // The evidence of the chosen result.
    if let Some((name, i)) = &st.selected
        && let Some((_, Ok(r))) = results.iter().find(|(n, _)| n == name)
        && let Some(s) = r.scopes.get(*i)
    {
        ui.separator();
        ui.label(RichText::new(format!("{} · {}", r.name, s.scope.label)).strong());
        if s.evidence.is_empty() {
            ui.weak("Nothing to show here.");
        }
        egui::ScrollArea::vertical().id_salt("check_evidence").show(ui, |ui| {
            for e in s.evidence.iter().take(300) {
                ui.horizontal(|ui| {
                    let lines: Vec<String> = e.rows.iter().map(|r| (r + 1).to_string()).collect();
                    if ui.link(RichText::new(format!("line {}", lines.join(" → "))).monospace()).clicked() {
                        out.goto = Some(e.rows[0]);
                    }
                    if !e.note.is_empty() {
                        ui.weak(&e.note);
                    }
                });
                let text = file.line_text(e.rows[0]).unwrap_or_default();
                let short: String = text.chars().take(160).collect();
                ui.label(RichText::new(short).monospace().small().weak());
            }
            if s.evidence.len() > 300 {
                ui.weak(format!("… {} more", s.evidence.len() - 300));
            }
        });
    }
    out
}

/// One check: on/off, its name and kind, ✎, −; then its results, overall │
/// per lifecycle.
fn check_row(
    ui: &mut egui::Ui,
    st: &mut ChecksUi,
    c: &CheckDef,
    results: &[(String, Outcome)],
    running: bool,
    act: &mut Option<Act>,
) {
    ui.horizontal(|ui| {
        let mut on = c.is_on();
        if ui.checkbox(&mut on, "").on_hover_text("On: run on every tab. Off: kept, not run").changed() {
            *act = Some(Act::Switch(c.name().to_owned(), on));
        }
        let icon = if c.kind() == Kind::Issue { "⚠" } else { "ℹ" };
        let name = RichText::new(format!("{icon} {}", c.name())).strong();
        ui.label(if c.is_on() { name } else { name.weak() }).on_hover_text(c.spec());
        ui.weak(c.kind().as_str());
        if ui.small_button("✎").on_hover_text("Edit").clicked() {
            *act = Some(Act::Edit(c.name().to_owned()));
        }
        if ui.small_button("−").on_hover_text("Remove").clicked() {
            *act = Some(Act::Remove(c.name().to_owned()));
        }
    });
    let result = results.iter().find(|(n, _)| n == c.name()).map(|(_, r)| r);
    ui.horizontal_wrapped(|ui| {
        ui.add_space(22.0);
        match (c.is_on(), result) {
            (false, _) => {
                ui.weak("off");
            }
            (true, None) => {
                ui.weak(if running { "…" } else { "—" });
            }
            (true, Some(Err(e))) => {
                ui.colored_label(ui.visuals().error_fg_color, e);
            }
            (true, Some(Ok(res))) => {
                let several = res.scopes.len() > 1;
                for (i, s) in res.scopes.iter().enumerate() {
                    let label = if i == 0 { String::new() } else { format!("#{}: ", s.scope.life.unwrap_or(0)) };
                    let (text, color) = match &s.verdict {
                        Verdict::Ok => ("ok".to_owned(), ui.visuals().weak_text_color()),
                        Verdict::Issue(n) => (format!("{n}×"), ui.visuals().error_fg_color),
                        Verdict::Fact(Some(v)) => (v.clone(), ui.visuals().text_color()),
                        Verdict::Fact(None) => ("—".to_owned(), ui.visuals().weak_text_color()),
                    };
                    let chosen = st.selected.as_ref().is_some_and(|(n, k)| n == c.name() && *k == i);
                    let resp = ui.selectable_label(chosen, RichText::new(format!("{label}{text}")).color(color));
                    if resp.on_hover_text(format!("{}: click for its lines", s.scope.label)).clicked() {
                        st.selected = Some((c.name().to_owned(), i));
                    }
                    // The overall result, then each lifecycle's.
                    if i == 0 && several {
                        ui.weak("│");
                    }
                }
            }
        }
    });
    ui.add_space(4.0);
}

fn form_ui(
    ui: &mut egui::Ui,
    st: &mut ChecksUi,
    saved: &mut Checks,
    out: &mut Output,
    file: &dyn LogSource,
    signals: &Signals,
    lib: &Library,
) {
    let f = &mut st.form;
    egui::Grid::new("check_form_grid").num_columns(2).spacing([8.0, 6.0]).show(ui, |ui| {
        ui.label("Name");
        ui.add(egui::TextEdit::singleline(&mut f.name).hint_text("ota_stall").desired_width(f32::INFINITY));
        ui.end_row();
        ui.label("Its lines").on_hover_text("The lines it looks at: NFD or a library query");
        crate::signals::rule_field(ui, "check_lines", &mut f.lines, "payload ~ /net down/", lib);
        ui.end_row();
        ui.label("Test");
        let before = f.test;
        egui::ComboBox::from_id_salt("check_test").selected_text(f.test.label()).show_ui(ui, |ui| {
            for t in TestChoice::ALL {
                let (issue, fact) = t.help();
                ui.selectable_value(&mut f.test, t, t.label())
                    .on_hover_text(format!("As an issue: {issue}\nAs a fact: {fact}"));
            }
        });
        // A count or a value is a fact unless it has a condition.
        if f.test != before {
            f.kind = if f.test.has_condition() { Kind::Fact } else { Kind::Issue };
        }
        ui.end_row();
        match f.test {
            TestChoice::Exists | TestChoice::Count => {}
            TestChoice::Rate => {
                ui.label("More than");
                ui.horizontal(|ui| {
                    ui.add(egui::TextEdit::singleline(&mut f.max).desired_width(50.0));
                    ui.label("within");
                    ui.add(egui::TextEdit::singleline(&mut f.window).desired_width(60.0));
                });
                ui.end_row();
            }
            TestChoice::Gap => {
                ui.label("For longer than");
                ui.add(egui::TextEdit::singleline(&mut f.window).desired_width(60.0));
                ui.end_row();
            }
            TestChoice::Then | TestChoice::Missing => {
                ui.label(if f.test == TestChoice::Then { "Followed by" } else { "Not followed by" });
                crate::signals::rule_field(ui, "check_other", &mut f.other, "payload ~ /ota progress/", lib);
                ui.end_row();
                ui.label("Within");
                ui.add(egui::TextEdit::singleline(&mut f.window).desired_width(60.0));
                ui.end_row();
            }
            TestChoice::Value => {
                ui.label("Value");
                ui.horizontal(|ui| {
                    for p in ["first", "last", "min", "max"] {
                        ui.radio_value(&mut f.pick, p.to_owned(), p);
                    }
                })
                .response
                .on_hover_text("Of what its lines' query's first named group (?<name>…) captures");
                ui.end_row();
            }
        }
        ui.label("It is");
        ui.horizontal(|ui| {
            let (issue, fact) = f.test.help();
            ui.radio_value(&mut f.kind, Kind::Issue, "an issue").on_hover_text(format!("Shows {issue}"));
            ui.radio_value(&mut f.kind, Kind::Fact, "a fact").on_hover_text(format!("Shows {fact}"));
        });
        ui.end_row();
        if f.test.has_condition() && f.kind == Kind::Issue {
            ui.label("When");
            ui.horizontal(|ui| {
                let shown = if f.op.is_empty() { "(any)".to_owned() } else { f.op.clone() };
                egui::ComboBox::from_id_salt("check_op").width(60.0).selected_text(shown).show_ui(ui, |ui| {
                    if f.test == TestChoice::Count {
                        ui.selectable_value(&mut f.op, String::new(), "(any)");
                    }
                    for op in [">", ">=", "<", "<=", "==", "!="] {
                        ui.selectable_value(&mut f.op, op.to_owned(), op);
                    }
                });
                if !f.op.is_empty() {
                    ui.add(egui::TextEdit::singleline(&mut f.number).desired_width(60.0));
                }
            });
            ui.end_row();
        }
        ui.label("For");
        let lifecycles: Vec<String> = signals.lifecycles().map(|l| l.name().to_owned()).collect();
        let shown = if f.per.is_empty() { "the whole log".to_owned() } else { format!("each lifecycle of {}", f.per) };
        egui::ComboBox::from_id_salt("check_per").selected_text(shown).show_ui(ui, |ui| {
            ui.selectable_value(&mut f.per, String::new(), "the whole log");
            for n in lifecycles {
                let label = format!("each lifecycle of {n}");
                ui.selectable_value(&mut f.per, n, label);
            }
        });
        ui.end_row();
        ui.label("Labels").on_hover_text("Groups: switch every check of a label on or off at once");
        ui.add(egui::TextEdit::singleline(&mut f.labels).hint_text("ota, network").desired_width(f32::INFINITY));
        ui.end_row();
    });
    let (issue, fact) = f.test.help();
    ui.weak(format!("Shows {}", if f.kind == Kind::Issue { issue } else { fact }));
    ui.horizontal(|ui| {
        let label = if st.editing.is_some() { "Update" } else { "Add" };
        if ui.button(label).clicked() {
            match CheckDef::parse_spec(&f.spec()) {
                Ok(c) => {
                    let on = st.editing.as_deref().and_then(|n| saved.get(n)).is_none_or(|o| o.is_on());
                    if let Some(old) = st.editing.take()
                        && old != c.name()
                    {
                        saved.remove(&old);
                    }
                    saved.set(c.with_on(on));
                    *f = Form::default();
                    st.form_open = false;
                    out.changed = true;
                }
                Err(e) => f.error = Some(e.msg),
            }
        }
        if ui.button("👁 Preview").on_hover_text("Run it on this file").clicked() {
            f.preview = Some(preview(&f.spec(), file, signals, lib));
        }
        if ui.button("Cancel").clicked() {
            st.editing = None;
            st.form_open = false;
            *f = Form::default();
        }
    });
    if let Some(e) = &f.error {
        ui.colored_label(ui.visuals().error_fg_color, e);
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
}

/// What the form's check finds on `file`, in one line.
fn preview(spec: &str, file: &dyn LogSource, signals: &Signals, lib: &Library) -> Result<String, String> {
    let c = CheckDef::parse_spec(spec).map_err(|e| e.msg)?.resolved(lib)?;
    let r = c.run(file, signals, &nerka_core::Progress::new()).map_err(|e| e.to_string())?;
    Ok(r.scopes
        .iter()
        .map(|s| {
            let v = match &s.verdict {
                Verdict::Ok => "ok".to_owned(),
                Verdict::Issue(n) => format!("{n}×"),
                Verdict::Fact(v) => v.clone().unwrap_or_else(|| "—".into()),
            };
            format!("{}: {v}", s.scope.label)
        })
        .collect::<Vec<_>>()
        .join(" · "))
}

/// Run every check that is on, on `file`: their results by name.
pub fn run_all(
    checks: &Checks,
    file: &dyn LogSource,
    signals: &Signals,
    lib: &Library,
    progress: &nerka_core::Progress,
) -> nerka_core::Result<Vec<(String, Outcome)>> {
    let mut out = Vec::new();
    for c in checks.iter().filter(|c| c.is_on()) {
        let r = match c.resolved(lib) {
            Ok(c) => match c.run(file, signals, progress) {
                Err(nerka_core::Error::Cancelled) => return Err(nerka_core::Error::Cancelled),
                r => r.map_err(|e| e.to_string()),
            },
            Err(e) => Err(format!("check '{}': {e}", c.name())),
        };
        out.push((c.name().to_owned(), r));
    }
    Ok(out)
}

/// What the checks found in one lifecycle (`lifecycle #number`): how many
/// issues and facts, and a line per check, for the lifecycle list.
pub fn of_lifecycle(results: &[(String, Outcome)], lifecycle: &str, number: u32) -> (usize, usize, Vec<String>) {
    let label = format!("{lifecycle} #{number}");
    let (mut issues, mut facts, mut lines) = (0, 0, Vec::new());
    for (name, r) in results {
        let Ok(r) = r else { continue };
        let Some(s) = r.scopes.iter().find(|s| s.scope.label == label) else { continue };
        match &s.verdict {
            Verdict::Issue(n) => {
                issues += 1;
                lines.push(format!("⚠ {name}: {n}×"));
            }
            Verdict::Fact(v) => {
                facts += 1;
                lines.push(format!("ℹ {name}: {}", v.as_deref().unwrap_or("—")));
            }
            Verdict::Ok => lines.push(format!("✔ {name}: ok")),
        }
    }
    (issues, facts, lines)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_form_round_trips_specs() {
        for spec in [
            "a: lines raw ~ /x/; exists",
            "b: lines {sys.retry}; count",
            "c: lines raw == y; count >= 3; per ecu1",
            "d: lines raw == y; rate 20 per 10s; labels net, ota",
            "e: lines apid == NAV; gap 5s; fact; per gw",
            "f: lines {net.down}; then {ota.progress} within 30s",
            "g: lines {net.down}; missing raw ~ /ota/ within 1m; fact",
            r"h: lines /t=(?<t>\d+)/; value max; per ecu1",
            r"i: lines /t=(?<t>\d+)/; value max > 90",
            "j: lines raw == y; count; issue",
            "Fusa Faults from aurix: lines {Aurix_logs.Fusa Fault}; count; per perf",
        ] {
            let d = CheckDef::parse_spec(spec).unwrap();
            let back = CheckDef::parse_spec(&Form::of(&d).spec()).unwrap_or_else(|e| panic!("{spec}: {}", e.msg));
            assert_eq!(back.spec(), d.spec(), "{spec}");
        }
    }
}
