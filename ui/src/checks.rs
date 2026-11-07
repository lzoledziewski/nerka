//! The Checks pane (`doc/checks.md`): the user's checks grouped by label,
//! their results on the tab's file (overall and per lifecycle) and the
//! evidence, and a form to define one. Core runs them
//! ([`nerka_core::check`]); the pane shows results and edits definitions
//! (owner, 2026-11-07: built in small steps with feedback).

use eframe::egui::{self, RichText};
use nerka_core::check::{CheckDef, CheckResult, Checks, Kind, Op, Part, Test, Verdict, signal_name};
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
    /// Its tests (owner, 2026-11-07: several in one check); one at first.
    tests: Vec<TestForm>,
    /// Several tests: an issue when any finds one (else all).
    any: bool,
    kind: Kind,
    per: String,
    labels: String,
    error: Option<String>,
    preview: Option<Result<String, String>>,
}

/// One test's fields.
#[derive(Clone)]
struct TestForm {
    test: TestChoice,
    /// A condition on a count or a value: an operator (empty: none) and a number.
    op: String,
    number: String,
    /// `rate`'s count, and the duration of `rate` and `gap`.
    max: String,
    window: String,
    pick: String,
    /// A sequence's steps.
    steps: Vec<StepForm>,
}

/// One step of a sequence: followed by (`then`) or not (`missing`).
#[derive(Clone)]
struct StepForm {
    missing: bool,
    query: String,
    within: String,
}

impl Default for StepForm {
    fn default() -> Self {
        StepForm { missing: false, query: String::new(), within: "10s".into() }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum TestChoice {
    Exists,
    Count,
    Rate,
    Gap,
    Steps,
    Value,
}

impl TestChoice {
    const ALL: [TestChoice; 6] = [
        TestChoice::Exists,
        TestChoice::Count,
        TestChoice::Rate,
        TestChoice::Gap,
        TestChoice::Steps,
        TestChoice::Value,
    ];

    fn label(self) -> &'static str {
        match self {
            TestChoice::Exists => "shows up",
            TestChoice::Count => "how many",
            TestChoice::Rate => "too many at once",
            TestChoice::Gap => "stops for too long",
            TestChoice::Steps => "followed (or not) by",
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
            TestChoice::Steps => (
                "each of its lines whose 'followed by' steps happen, and a 'not followed by' step doesn't",
                "how often",
            ),
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

impl Default for TestForm {
    fn default() -> Self {
        TestForm {
            test: TestChoice::Exists,
            op: String::new(),
            number: "1".into(),
            max: "10".into(),
            window: "10s".into(),
            pick: "max".into(),
            steps: vec![StepForm::default()],
        }
    }
}

impl TestForm {
    /// Its clauses.
    fn spec(&self) -> String {
        let cond = if self.test.has_condition() && !self.op.is_empty() {
            format!(" {} {}", self.op, self.number.trim())
        } else {
            String::new()
        };
        match self.test {
            TestChoice::Exists => "exists".to_owned(),
            TestChoice::Count => format!("count{cond}"),
            TestChoice::Rate => format!("rate {} per {}", self.max.trim(), self.window.trim()),
            TestChoice::Gap => format!("gap {}", self.window.trim()),
            TestChoice::Steps => self
                .steps
                .iter()
                .map(|s| {
                    let word = if s.missing { "missing" } else { "then" };
                    format!("{word} {} within {}", s.query.trim(), s.within.trim())
                })
                .collect::<Vec<_>>()
                .join("; "),
            TestChoice::Value => format!("value {}{cond}", self.pick),
        }
    }

    fn of(p: &Part) -> TestForm {
        let d = nerka_core::lifecycle::format_duration;
        let mut f = TestForm::default();
        if let Some((op, n)) = p.cond {
            (f.op, f.number) = (op_text(op).into(), n.to_string());
        }
        match &p.test {
            Test::Exists => f.test = TestChoice::Exists,
            Test::Count => f.test = TestChoice::Count,
            Test::Rate { max, window } => (f.test, f.max, f.window) = (TestChoice::Rate, max.to_string(), d(*window)),
            Test::Gap(g) => (f.test, f.window) = (TestChoice::Gap, d(*g)),
            Test::Steps(steps) => {
                f.test = TestChoice::Steps;
                f.steps = steps
                    .iter()
                    .map(|s| StepForm { missing: s.missing, query: s.rule.text(), within: d(s.within) })
                    .collect();
            }
            Test::Value(pick) => {
                f.test = TestChoice::Value;
                f.pick = format!("{pick:?}").to_ascii_lowercase();
            }
        }
        f
    }
}

fn op_text(op: Op) -> &'static str {
    match op {
        Op::Gt => ">",
        Op::Ge => ">=",
        Op::Lt => "<",
        Op::Le => "<=",
        Op::Eq => "==",
        Op::Ne => "!=",
    }
}

impl Default for Form {
    fn default() -> Self {
        Form {
            name: String::new(),
            lines: String::new(),
            tests: vec![TestForm::default()],
            any: false,
            kind: Kind::Issue,
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
        let tests: Vec<String> = self.tests.iter().map(TestForm::spec).collect();
        let mut spec = format!("{}: lines {}; {}", self.name.trim(), self.lines.trim(), tests.join("; "));
        if self.tests.len() > 1 && self.any {
            spec += "; any";
        }
        spec += &format!("; {}", self.kind.as_str());
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
        Form {
            name: c.name().to_owned(),
            lines: c.lines_text(),
            tests: c.parts().iter().map(TestForm::of).collect(),
            any: c.is_any(),
            kind: c.kind(),
            per: c.per().unwrap_or_default().to_owned(),
            labels: c.labels().join(", "),
            ..Form::default()
        }
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
    /// Plotting a check plots its signal.
    pub plot: &'a mut crate::plot::PlotState,
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
    let Input { saved, results, running, file, signals, library, plot } = input;
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
                ui.indent((label, c.name()), |ui| check_row(ui, st, c, results, running, plot, &mut act));
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
    plot: &mut crate::plot::PlotState,
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
        // Its signal: plotted only when asked (owner, 2026-11-07: a check
        // on, not plotted; low-priority ones would clutter the plot).
        let sig = signal_name(c.name());
        let plotted = plot.is_plotted(&sig);
        let tip = format!(
            "Plot @{sig}: an event where it finds something, its value counting them{}. Other checks and \
             filters use it too (@{sig}.event, @{sig} > 0).",
            if c.per().is_some() { " per lifecycle" } else { "" }
        );
        if ui.add_enabled(c.is_on(), egui::Button::selectable(plotted, "📈")).on_hover_text(tip).clicked() {
            plot.toggle(&sig);
            plot.shown = true;
        }
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
        ui.label("Its lines").on_hover_text(
            "The lines it looks at: NFD or a library query. Other checks' signals work too: @name.event for \
             where another check found something",
        );
        crate::signals::rule_field(ui, "check_lines", &mut f.lines, "payload ~ /net down/", lib);
        ui.end_row();
    });
    // Its tests, each in a frame: the test, its fields, − to drop it.
    let several = f.tests.len() > 1;
    let mut drop_test = None;
    for (i, t) in f.tests.iter_mut().enumerate() {
        egui::Frame::group(ui.style()).show(ui, |ui| {
            egui::Grid::new(("check_test_grid", i)).num_columns(2).spacing([8.0, 6.0]).show(ui, |ui| {
                ui.label(if several { format!("Test {}", i + 1) } else { "Test".into() });
                ui.horizontal(|ui| {
                    let before = t.test;
                    egui::ComboBox::from_id_salt(("check_test", i)).selected_text(t.test.label()).show_ui(ui, |ui| {
                        for c in TestChoice::ALL {
                            let (issue, fact) = c.help();
                            ui.selectable_value(&mut t.test, c, c.label())
                                .on_hover_text(format!("As an issue: {issue}\nAs a fact: {fact}"));
                        }
                    });
                    // A count or a value is a fact unless it has a condition.
                    if t.test != before && !several {
                        f.kind = if t.test.has_condition() { Kind::Fact } else { Kind::Issue };
                    }
                    if several && ui.small_button("−").on_hover_text("Remove this test").clicked() {
                        drop_test = Some(i);
                    }
                });
                ui.end_row();
                test_fields(ui, i, t, f.kind, lib);
            });
        });
    }
    if let Some(i) = drop_test {
        f.tests.remove(i);
    }
    ui.horizontal(|ui| {
        if ui
            .button("➕ Another test")
            .on_hover_text("Several tests in one check: an issue when all of them (or any) find one")
            .clicked()
        {
            f.tests.push(TestForm::default());
            f.kind = Kind::Issue;
        }
        if f.tests.len() > 1 {
            ui.label("An issue when");
            ui.radio_value(&mut f.any, false, "all find one");
            ui.radio_value(&mut f.any, true, "any finds one");
        }
    });
    egui::Grid::new("check_form_grid_2").num_columns(2).spacing([8.0, 6.0]).show(ui, |ui| {
        ui.label("It is");
        ui.horizontal(|ui| {
            let (issue, fact) = f.tests[0].test.help();
            ui.radio_value(&mut f.kind, Kind::Issue, "an issue").on_hover_text(format!("Shows {issue}"));
            ui.add_enabled_ui(f.tests.len() == 1, |ui| {
                ui.radio_value(&mut f.kind, Kind::Fact, "a fact")
                    .on_hover_text(format!("Shows {fact}"))
                    .on_disabled_hover_text("A fact has one test");
            });
        });
        ui.end_row();
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
    if f.tests.len() == 1 {
        let (issue, fact) = f.tests[0].test.help();
        ui.weak(format!("Shows {}", if f.kind == Kind::Issue { issue } else { fact }));
    }
    if !f.name.trim().is_empty() {
        ui.weak(format!("Its signal: @{} (an event where it finds something)", signal_name(&f.name)));
    }
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

/// One test's own fields (grid rows).
fn test_fields(ui: &mut egui::Ui, i: usize, t: &mut TestForm, kind: Kind, lib: &Library) {
    match t.test {
        TestChoice::Exists | TestChoice::Count => {}
        TestChoice::Rate => {
            ui.label("More than");
            ui.horizontal(|ui| {
                ui.add(egui::TextEdit::singleline(&mut t.max).desired_width(50.0));
                ui.label("within");
                ui.add(egui::TextEdit::singleline(&mut t.window).desired_width(60.0));
            });
            ui.end_row();
        }
        TestChoice::Gap => {
            ui.label("For longer than");
            ui.add(egui::TextEdit::singleline(&mut t.window).desired_width(60.0));
            ui.end_row();
        }
        TestChoice::Steps => {
            // Each step after the one before (the first: after its line).
            let mut drop_step = None;
            let n = t.steps.len();
            for (k, s) in t.steps.iter_mut().enumerate() {
                ui.horizontal(|ui| {
                    egui::ComboBox::from_id_salt(("check_step_kind", i, k))
                        .width(130.0)
                        .selected_text(if s.missing { "not followed by" } else { "followed by" })
                        .show_ui(ui, |ui| {
                            ui.selectable_value(&mut s.missing, false, "followed by")
                                .on_hover_text("Must happen for the sequence to go on");
                            ui.selectable_value(&mut s.missing, true, "not followed by")
                                .on_hover_text("Expected: the issue is that it doesn't happen");
                        });
                    if n > 1 && ui.small_button("−").on_hover_text("Remove this step").clicked() {
                        drop_step = Some(k);
                    }
                });
                ui.vertical(|ui| {
                    let id = format!("check_step_{i}_{k}");
                    crate::signals::rule_field(ui, &id, &mut s.query, "payload ~ /ota progress/", lib);
                    ui.horizontal(|ui| {
                        ui.label("within");
                        ui.add(egui::TextEdit::singleline(&mut s.within).desired_width(60.0));
                        ui.weak(if k == 0 { "of its line" } else { "of the step before" });
                    });
                });
                ui.end_row();
            }
            if let Some(k) = drop_step {
                t.steps.remove(k);
            }
            ui.label("");
            if ui
                .small_button("➕ Step")
                .on_hover_text("Then another line within a time of this one: 'followed by' steps come first")
                .clicked()
            {
                let missing = t.steps.last().is_some_and(|s| s.missing);
                t.steps.push(StepForm { missing, ..StepForm::default() });
            }
            ui.end_row();
        }
        TestChoice::Value => {
            ui.label("Value");
            ui.horizontal(|ui| {
                for p in ["first", "last", "min", "max"] {
                    ui.radio_value(&mut t.pick, p.to_owned(), p);
                }
            })
            .response
            .on_hover_text("Of what its lines' query's first named group (?<name>…) captures");
            ui.end_row();
        }
    }
    if t.test.has_condition() && kind == Kind::Issue {
        ui.label("When");
        ui.horizontal(|ui| {
            let shown = if t.op.is_empty() { "(any)".to_owned() } else { t.op.clone() };
            egui::ComboBox::from_id_salt(("check_op", i)).width(60.0).selected_text(shown).show_ui(ui, |ui| {
                if t.test == TestChoice::Count {
                    ui.selectable_value(&mut t.op, String::new(), "(any)");
                }
                for op in [">", ">=", "<", "<=", "==", "!="] {
                    ui.selectable_value(&mut t.op, op.to_owned(), op);
                }
            });
            if !t.op.is_empty() {
                ui.add(egui::TextEdit::singleline(&mut t.number).desired_width(60.0));
            }
        });
        ui.end_row();
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
            "k: lines raw == a; then raw == b within 1s; then raw == c within 2s; missing raw == d within 3s",
            "l: lines raw == y; count >= 3; gap 30s; per a",
            "m: lines raw == y; rate 2 per 1s; gap 30s; any",
            r"n: lines @flow.event; value max > 3; count; then {net.down} within 500ms",
        ] {
            let d = CheckDef::parse_spec(spec).unwrap();
            let back = CheckDef::parse_spec(&Form::of(&d).spec()).unwrap_or_else(|e| panic!("{spec}: {}", e.msg));
            assert_eq!(back.spec(), d.spec(), "{spec}");
        }
    }
}
