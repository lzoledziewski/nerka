//! The pattern tuning pane (🛠; experimental, `docs/dev/patterns.md`, round
//! α2), a mode of the Patterns window: the pattern configuration's settings
//! and rules edited on the trace at hand, each change re-running the
//! patterns (debounced, on the same lines), what the change did, and why
//! two patterns were kept apart or joined. Shown in every build (owner,
//! 2026-11-25: it stays in the Patterns window for 0.3.0). Nothing is
//! written until Save; Export and Load move a
//! configuration between testers.

use eframe::egui::{self, RichText};
use nerka_core::pattern::{PatternConfig, Patterns, Setting, Settings};
use std::time::{Duration, Instant};

/// A setting's change waits this long for the next before the run.
const DEBOUNCE: Duration = Duration::from_millis(600);

/// What a setting does (its name's hover) and, in short, what raising or
/// lowering it does (beside its value).
struct Help {
    what: &'static str,
    effect: &'static str,
}

const STAGE1: &str = "Stage 1: within each chunk of lines, messages of the same length and first word (and \
                      fields) that are alike enough join a template; the words where they differ become <*>.";
const STAGE2: &str = "Stage 2: the templates of all chunks are aligned word by word and character by \
                      character and joined into patterns, so a varying part may sit inside a word (gear=D), \
                      come first, or have different lengths ({engaged at <*> C|released}).";

const SIMILARITY: Help = Help {
    what: "How alike a message must be to join a template: the share of its words that are the same. \
           0.5: `user anna logged in` joins `user bob logged in` (3 of 4 the same).\n\n\
           Higher: fewer messages join, so more and narrower patterns; a message differing in one word of \
           a few stays apart. Lower: more join, fewer and more general patterns with more <*>.",
    effect: "↑ more, narrower patterns · ↓ fewer, more <*>",
};
const NEW_WILDCARDS: Help = Help {
    what: "How many words of a template may become <*> at once when a message joins it. 1: `door open by \
           driver` and `door closed by passenger`, two words apart, start two templates (stage 2 may still \
           join them).\n\n\
           Higher: they join here, fewer templates (two messages sharing only a few words may become one). \
           0: a template gets no new <*>; only messages of the same words (values masked) group.",
    effect: "↑ joins messages differing in more places at once · 0 stricter",
};
const HEADER: Help = Help {
    what: "Header and footer detection. Words most lines of a context (an apid, a file's profile) carry \
           at the same place from the start or end, such as `LOG:` … `Elapsed[us]: 7; #3`, don't count as \
           alike: two short messages between them stay two patterns.\n\n\
           sample: lines of a context held back per chunk to look at; min: the fewest lines to look at \
           (fewer: no header there); share: a word on more than this share of them is header or footer; \
           places: how many tokens from the start and the end it may be at.\n\n\
           Higher share or fewer places: fewer words taken for a frame, so messages sharing one join more \
           easily. Lower share: more words passed over (a common real word may be, splitting its \
           messages).",
    effect: "↑ share, ↓ places: fewer frame words, more joining",
};
const MERGE: Help = Help {
    what: "Stage 2 at all. Off: only messages of the same length and first word group (stage 1), so \
           `thermal throttling engaged at 92 C` and `thermal throttling released` stay apart, and there \
           are many more, narrower patterns.",
    effect: "off: stage 1 only, more patterns",
};
const NEW_SLOTS: Help = Help {
    what: "How many new slots (places neither template has a slot yet) one merge may make. 1: two places \
           that would become slots at once keep the templates apart (they change together, as far as \
           anyone can tell), until another pattern shows them varying on their own.\n\n\
           Higher: such templates join at once, fewer patterns (different messages with the same frame \
           may join). 0: a merge only fills slots that exist.",
    effect: "↑ fewer patterns, riskier joins · ↓ more patterns",
};
const SIDE: Help = Help {
    what: "The most tokens (words, values, slots) the longer side of a place two templates differ in may \
           hold. 4: `job done` and `job done fast` join as `job done {∅|fast}`; `job finished in 5 ms` and \
           `job of the big nightly batch finished in 5 ms` don't.\n\n\
           Higher: longer phrases fit one slot, fewer patterns, slots spanning phrases. Lower: only short \
           variations join.",
    effect: "↑ longer phrases in one slot · ↓ only short ones",
};
const STATE_VALUES: Help = Help {
    what: "A slot of at most this many values, which repeat and hold a letter, shows as a state, \
           gear={D|P|N}, and ➕ To library makes it an enum signal. With more values it shows as <*> \
           (users, addresses). 16 at most: the values a slot keeps while grouping.\n\n\
           Lower: fewer states, more <*>. Higher: slots of more values listed as states.",
    effect: "↑ more {a|b|c} states · ↓ more <*>",
};
const LONE_WORD: Help = Help {
    what: "A message's only word is never a slot. On: a level near the start (`INFO`, `587~DEBUG`) is \
           header, not a sign of one message, and two messages whose words in common are only header \
           and footer stay two patterns: `INFO dlt_message_init()` and `INFO dlt_message_free()`, or \
           `LOG: radar: OK` and `LOG: imu: OFFLINE`; a state needs a word of the message in common \
           (`door {open|closed}`).\n\n\
           Off: such messages join into one pattern, the word a slot ({dlt_message_init()|…}), fewer \
           patterns.",
    effect: "on: one-word messages apart · off: fewer patterns",
};

/// The rule form's actions.
const ACTIONS: [(&str, &str); 10] = [
    ("keep", "WORD… never in a slot"),
    ("apart", "WORD WORD… never in one slot"),
    ("max-slots", "N: slots a pattern has at most"),
    ("value", "/REGEX/: text that is a value"),
    ("literal", "/REGEX/: text that is never a value"),
    ("header", "WORD…: header or footer tokens"),
    ("ignore", "[/REGEX/]: lines left out"),
    ("no-merge", "stage 1 only"),
    ("set", "SETTING VALUE for these lines"),
    ("prefix", "/REGEX/: not the message, at its start; (?<name>…): a field"),
];

/// What a change of settings did to the patterns.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Change {
    /// Patterns (fields and template) only the new run has, only the old.
    pub new: Vec<String>,
    pub gone: Vec<String>,
    /// New patterns holding lines of several old ones (how many), and old
    /// ones whose lines went into several new ones.
    pub joined: Vec<(String, usize)>,
    pub split: Vec<(String, usize)>,
    /// Too many lines to follow (only new and gone are known).
    pub partial: bool,
}

impl Change {
    fn summary(&self) -> String {
        if *self == Change::default() {
            return "no change".into();
        }
        format!(
            "+{} patterns, −{}, {} joined, {} split{}",
            self.new.len(),
            self.gone.len(),
            self.joined.len(),
            self.split.len(),
            if self.partial { " (joins and splits not followed: too many lines)" } else { "" }
        )
    }
}

/// A pattern's name in a change: its fields and template.
pub fn name_of(p: &nerka_core::pattern::Pattern) -> String {
    let mut s: String = p.fields.iter().map(|(n, v)| format!("{n}={v} ")).collect();
    s += &p.template();
    s
}

/// What changed from run `old` to run `new` (of the same lines).
pub fn change(old: &Patterns, new: &Patterns) -> Change {
    use std::collections::HashSet;
    let names = |ps: &Patterns| ps.patterns.iter().map(name_of).collect::<Vec<_>>();
    let (on, nn) = (names(old), names(new));
    let (os, ns): (HashSet<&String>, HashSet<&String>) = (on.iter().collect(), nn.iter().collect());
    let mut c = Change {
        new: nn.iter().filter(|n| !os.contains(n)).cloned().collect(),
        gone: on.iter().filter(|n| !ns.contains(n)).cloned().collect(),
        ..Change::default()
    };
    // Each line's pattern in both runs, by line: who went where.
    let lines: u64 = old.patterns.iter().map(|p| p.lines.iter().map(Vec::len).sum::<usize>() as u64).sum();
    if lines > 4_000_000 || old.total() != new.total() {
        c.partial = true;
        return c;
    }
    let of = |ps: &Patterns| {
        let mut v: Vec<(usize, u64, u32)> = Vec::new();
        for (k, p) in ps.patterns.iter().enumerate() {
            for (side, ls) in p.lines.iter().enumerate() {
                v.extend(ls.iter().map(|&l| (side, l, k as u32)));
            }
        }
        v.sort_unstable();
        v
    };
    let (a, b) = (of(old), of(new));
    if a.len() != b.len() || a.iter().zip(&b).any(|(x, y)| (x.0, x.1) != (y.0, y.1)) {
        c.partial = true;
        return c;
    }
    let mut pairs: Vec<(u32, u32)> = a.iter().zip(&b).map(|(x, y)| (x.2, y.2)).collect();
    pairs.sort_unstable();
    pairs.dedup();
    let count = |key: &dyn Fn(&(u32, u32)) -> u32, n: usize| {
        let mut per = vec![0usize; n];
        for p in &pairs {
            per[key(p) as usize] += 1;
        }
        per
    };
    let into = count(&|p| p.0, old.patterns.len());
    let from = count(&|p| p.1, new.patterns.len());
    c.split = into.iter().enumerate().filter(|(_, n)| **n > 1).map(|(k, n)| (on[k].clone(), *n)).collect();
    c.joined = from.iter().enumerate().filter(|(_, n)| **n > 1).map(|(k, n)| (nn[k].clone(), *n)).collect();
    c
}

/// The new rule form.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Form {
    /// Per scope field: (name, value; empty: any).
    pub scope: Vec<(String, String)>,
    pub action: usize,
    pub arg: String,
    /// The tuned tab's kind (`profile == sail_log`, `proto == DLT`:
    /// [`nerka_core::pattern::kind_scope`]): what the scope starts with, and keeps where
    /// a pattern gives no value, so a rule made on one kind of file isn't
    /// applied to others unless widened.
    pub kind: Vec<(String, String)>,
}

impl Form {
    /// The rule it makes: `apid == NAV: keep Elapsed`.
    pub fn rule(&self) -> String {
        let conds: Vec<String> = self
            .scope
            .iter()
            .filter(|(_, v)| !v.trim().is_empty())
            .map(|(n, v)| {
                let v = v.trim();
                if v.chars().all(|c| c.is_alphanumeric() || "_*.-".contains(c)) {
                    format!("{n} == {v}")
                } else {
                    format!("{n} == \"{}\"", v.replace('\\', "\\\\").replace('"', "\\\""))
                }
            })
            .collect();
        let scope = if conds.is_empty() { "*".to_owned() } else { conds.join(" AND ") };
        format!("{scope}: {} {}", ACTIONS[self.action].0, self.arg.trim()).trim_end().to_owned()
    }

    /// The scope's fields `fields`, for a tab of `kind`: on another tab's
    /// fields or kind, the scope starts again from the kind.
    pub fn scope_for(&mut self, fields: &[String], kind: &[(String, String)]) {
        if self.scope.iter().map(|(n, _)| n).ne(fields.iter()) || self.kind != kind {
            self.kind = kind.to_vec();
            self.scope = fields
                .iter()
                .map(|f| (f.clone(), kind.iter().find(|(n, _)| n == f).map(|(_, v)| v.clone()).unwrap_or_default()))
                .collect();
        }
    }

    /// Filled for `action` with `arg`, scoped by `fields` (of a pattern),
    /// else by the tab's kind.
    pub fn fill(&mut self, fields: &[(String, String)], action: &str, arg: String) {
        for (n, v) in &mut self.scope {
            *v = fields.iter().chain(&self.kind).find(|(m, _)| m == n).map(|(_, w)| w.clone()).unwrap_or_default();
        }
        self.action = ACTIONS.iter().position(|(a, _)| *a == action).unwrap_or(0);
        self.arg = arg;
    }
}

/// The tuning pane's state.
#[derive(Default)]
pub struct TuningUi {
    /// The configuration tuned: loaded from the saved one when first shown.
    config: Option<PatternConfig>,
    /// The rules as typed, each with why it doesn't read.
    rules: Vec<(String, Option<String>)>,
    /// Edited since saved or loaded.
    changed: bool,
    /// Bumps with each edit: a run with another is out of date.
    pub generation: u64,
    edited: Option<Instant>,
    note: Option<String>,
    pub form: Form,
    /// What the last change did, and whether its list is shown.
    pub change: Option<Change>,
    show_change: bool,
}

impl TuningUi {
    /// The configuration to run with, once the pane is in use.
    pub fn active(&self) -> Option<&PatternConfig> {
        self.config.as_ref()
    }

    /// Load the saved configuration (the pane's first showing, Revert).
    fn load(&mut self, config: Result<PatternConfig, String>) {
        match config {
            Ok(c) => self.set(c, false),
            Err(e) => {
                self.note = Some(format!("The saved configuration doesn't read: {e}"));
                self.set(PatternConfig::default(), false);
            }
        }
    }

    fn set(&mut self, c: PatternConfig, changed: bool) {
        self.rules = c.rules.iter().map(|r| (r.text.clone(), None)).collect();
        self.config = Some(c);
        self.changed = changed;
        self.touch();
    }

    fn touch(&mut self) {
        self.generation += 1;
        self.edited = Some(Instant::now());
    }

    /// An edit waited long enough: run again (once).
    pub fn due(&mut self) -> bool {
        if self.edited.is_some_and(|t| t.elapsed() >= DEBOUNCE) {
            self.edited = None;
            return true;
        }
        false
    }

    /// Waiting to run again.
    pub fn waiting(&self) -> bool {
        self.edited.is_some()
    }

    /// The rules typed, read again into the configuration.
    fn rules_changed(&mut self) {
        let Some(c) = &mut self.config else { return };
        c.rules.clear();
        for (text, error) in &mut self.rules {
            *error = None;
            if text.trim().is_empty() {
                continue;
            }
            if let Err(e) = c.add_rule(text) {
                *error = Some(e);
            }
        }
        self.changed = true;
        self.touch();
    }

    /// Add a rule from the form (or a pattern's ⋮ Rule…).
    pub fn add_rule(&mut self, rule: String) {
        self.rules.push((rule, None));
        self.rules_changed();
    }

    /// The pane. `fields`: the context fields rules are scoped by; `kind`:
    /// the tab's kind ([`Form::kind`]); `last`: the last run.
    pub fn ui(
        &mut self,
        ui: &mut egui::Ui,
        fields: &[String],
        kind: &[(String, String)],
        last: Option<&Patterns>,
        load: fn() -> Result<PatternConfig, String>,
    ) {
        if self.config.is_none() {
            self.load(load());
            self.edited = None;
        }
        self.form.scope_for(fields, kind);
        ui.horizontal(|ui| {
            let path = PatternConfig::saved_path().map(|p| p.display().to_string()).unwrap_or_default();
            let state = if self.changed { "changed, not saved" } else { "as saved" };
            ui.label(RichText::new(format!("Configuration: {state}")).strong()).on_hover_text(path);
            if ui.add_enabled(self.changed, egui::Button::new("💾 Save")).clicked() {
                self.save();
            }
            if ui
                .add_enabled(self.changed, egui::Button::new("↺ Revert"))
                .on_hover_text("Back to the saved configuration")
                .clicked()
            {
                self.load(load());
            }
            if ui.button("Load…").on_hover_text("Read a configuration file (not saved until Save)").clicked() {
                self.load_file();
            }
            if ui.button("Export…").on_hover_text("Write this configuration to a file, e.g. for a bug report").clicked()
            {
                self.export();
            }
            if self.waiting() {
                ui.weak("running again…");
            }
        });
        if let Some(n) = &self.note {
            ui.label(n);
        }
        ui.separator();
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            self.settings_ui(ui);
            ui.separator();
            self.rules_ui(ui);
            ui.separator();
            if let Some(p) = last {
                let s = &p.stats;
                ui.label(RichText::new("Last run").strong());
                ui.label(format!(
                    "stage 1: {} templates, {:.1?} · stage 2: {} patterns, {:.1?}",
                    s.templates,
                    s.stage1,
                    p.patterns.len(),
                    s.stage2
                ));
                ui.label(format!(
                    "kept apart: {} by two new slots · {} by a long side · {} by rules{}",
                    s.apart,
                    s.side,
                    s.rules,
                    if s.ignored != [0, 0] {
                        format!(" · {} lines ignored", s.ignored[0] + s.ignored[1])
                    } else {
                        String::new()
                    }
                ));
            }
            if let Some(c) = &self.change {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Change").strong());
                    ui.label(format!("{} (since the run before)", c.summary()));
                    if *c != Change::default() {
                        ui.toggle_value(&mut self.show_change, "Show");
                    }
                });
                if self.show_change {
                    let list = |ui: &mut egui::Ui, title: &str, items: &[String]| {
                        if !items.is_empty() {
                            ui.label(RichText::new(title).strong());
                            for i in items {
                                ui.monospace(i);
                            }
                        }
                    };
                    list(ui, "New", &c.new);
                    list(ui, "Gone", &c.gone);
                    let counted = |v: &[(String, usize)], what: &str| -> Vec<String> {
                        v.iter().map(|(n, k)| format!("{n}   ({what} {k})")).collect()
                    };
                    list(ui, "Joined", &counted(&c.joined, "from"));
                    list(ui, "Split", &counted(&c.split, "into"));
                }
            }
            ui.add_space(4.0);
            ui.weak(
                "Why two patterns are apart or one: tick ? on two rows in 🧩 Patterns. ⋮ on a row makes a \
                 rule from it here.",
            );
        });
    }

    fn settings_ui(&mut self, ui: &mut egui::Ui) {
        let Some(c) = &mut self.config else { return };
        let d = Settings::default();
        let s = c.settings.clone();
        let mut set: Vec<Setting> = Vec::new();
        /// A setting's reset dot: ● while it isn't the default.
        fn dot(ui: &mut egui::Ui, changed: bool, default: String) -> bool {
            if changed {
                ui.small_button("●").on_hover_text(format!("Changed: back to the default, {default}")).clicked()
            } else {
                ui.weak("·").on_hover_text(format!("The default, {default}"));
                false
            }
        }
        /// A setting's name, its help on hover.
        fn name(ui: &mut egui::Ui, text: &str, help: &Help) {
            ui.label(text).on_hover_text(help.what);
        }
        /// What raising or lowering it does, in short.
        fn effect(ui: &mut egui::Ui, help: &Help) {
            ui.add_space(8.0);
            ui.weak(help.effect).on_hover_text(help.what);
        }
        ui.weak(
            "Each change runs the patterns again on the same lines; Change below says what joined, split, \
             came or went. Hover a setting's name for what it does.",
        );
        egui::Grid::new("tuning-settings").num_columns(3).spacing([10.0, 4.0]).show(ui, |ui| {
            ui.label(RichText::new("Stage 1").strong()).on_hover_text(STAGE1);
            name(ui, "similarity", &SIMILARITY);
            ui.horizontal(|ui| {
                let mut v = s.similarity;
                if ui.add(egui::DragValue::new(&mut v).range(0.0..=1.0).speed(0.01).max_decimals(2)).changed() {
                    set.push(Setting::Similarity(v));
                }
                if dot(ui, s.similarity != d.similarity, d.similarity.to_string()) {
                    set.push(Setting::Similarity(d.similarity));
                }
                effect(ui, &SIMILARITY);
            });
            ui.end_row();
            ui.label("");
            name(ui, "new wildcards", &NEW_WILDCARDS);
            ui.horizontal(|ui| {
                let mut v = s.new_wildcards;
                if ui.add(egui::DragValue::new(&mut v).range(0..=8)).changed() {
                    set.push(Setting::NewWildcards(v));
                }
                if dot(ui, s.new_wildcards != d.new_wildcards, d.new_wildcards.to_string()) {
                    set.push(Setting::NewWildcards(d.new_wildcards));
                }
                effect(ui, &NEW_WILDCARDS);
            });
            ui.end_row();
            ui.label("");
            name(ui, "header", &HEADER);
            ui.horizontal(|ui| {
                let (mut a, mut b, mut sh, mut p) = (s.header_sample, s.header_min, s.header_share, s.header_places);
                ui.label("sample").on_hover_text("Lines of a context held back per chunk to look at");
                if ui.add(egui::DragValue::new(&mut a).range(1..=1024)).changed() {
                    set.push(Setting::HeaderSample(a));
                }
                ui.label("min").on_hover_text("The fewest lines to look at: fewer, no header there");
                if ui.add(egui::DragValue::new(&mut b).range(0..=1024)).changed() {
                    set.push(Setting::HeaderMin(b));
                }
                ui.label("share").on_hover_text("A word on more than this share of them is header or footer");
                if ui.add(egui::DragValue::new(&mut sh).range(0.0..=1.0).speed(0.01).max_decimals(2)).changed() {
                    set.push(Setting::HeaderShare(sh));
                }
                ui.label("places").on_hover_text("How many tokens from the start and the end it may be at");
                if ui.add(egui::DragValue::new(&mut p).range(1..=8)).changed() {
                    set.push(Setting::HeaderPlaces(p));
                }
                let header_changed = (s.header_sample, s.header_min, s.header_share, s.header_places)
                    != (d.header_sample, d.header_min, d.header_share, d.header_places);
                if dot(ui, header_changed, "sample 32 min 8 share 0.5 places 3".into()) {
                    set.extend([
                        Setting::HeaderSample(d.header_sample),
                        Setting::HeaderMin(d.header_min),
                        Setting::HeaderShare(d.header_share),
                        Setting::HeaderPlaces(d.header_places),
                    ]);
                }
                effect(ui, &HEADER);
            });
            ui.end_row();
            ui.label(RichText::new("Stage 2").strong()).on_hover_text(STAGE2);
            name(ui, "merge", &MERGE);
            ui.horizontal(|ui| {
                let mut m = s.merge;
                if ui.checkbox(&mut m, "on").changed() {
                    set.push(Setting::Merge(m));
                }
                if dot(ui, s.merge != d.merge, "on".into()) {
                    set.push(Setting::Merge(d.merge));
                }
                effect(ui, &MERGE);
            });
            ui.end_row();
            ui.label("");
            name(ui, "new slots", &NEW_SLOTS);
            ui.horizontal(|ui| {
                let mut v = s.new_slots;
                if ui.add(egui::DragValue::new(&mut v).range(0..=8)).changed() {
                    set.push(Setting::NewSlots(v));
                }
                if dot(ui, s.new_slots != d.new_slots, d.new_slots.to_string()) {
                    set.push(Setting::NewSlots(d.new_slots));
                }
                effect(ui, &NEW_SLOTS);
            });
            ui.end_row();
            ui.label("");
            name(ui, "side", &SIDE);
            ui.horizontal(|ui| {
                let mut v = s.side;
                if ui.add(egui::DragValue::new(&mut v).range(1..=16)).changed() {
                    set.push(Setting::Side(v));
                }
                if dot(ui, s.side != d.side, d.side.to_string()) {
                    set.push(Setting::Side(d.side));
                }
                effect(ui, &SIDE);
            });
            ui.end_row();
            ui.label("");
            name(ui, "state values", &STATE_VALUES);
            ui.horizontal(|ui| {
                let mut v = s.state_values;
                if ui.add(egui::DragValue::new(&mut v).range(2..=nerka_core::pattern::MAX_VALUES)).changed() {
                    set.push(Setting::StateValues(v));
                }
                if dot(ui, s.state_values != d.state_values, d.state_values.to_string()) {
                    set.push(Setting::StateValues(d.state_values));
                }
                effect(ui, &STATE_VALUES);
            });
            ui.end_row();
            ui.label("");
            name(ui, "lone word", &LONE_WORD);
            ui.horizontal(|ui| {
                let mut m = s.lone_word;
                if ui.checkbox(&mut m, "on").changed() {
                    set.push(Setting::LoneWord(m));
                }
                if dot(ui, s.lone_word != d.lone_word, "on".into()) {
                    set.push(Setting::LoneWord(d.lone_word));
                }
                effect(ui, &LONE_WORD);
            });
            ui.end_row();
        });
        if !set.is_empty() {
            for x in set {
                c.settings.apply(x);
            }
            self.changed = true;
            self.touch();
        }
    }

    fn rules_ui(&mut self, ui: &mut egui::Ui) {
        ui.label(RichText::new("Rules").strong()).on_hover_text(
            "SCOPE: ACTION, read top to bottom; of rules setting one thing the first that applies counts, \
             the others add up",
        );
        let mut edited = false;
        let (mut up, mut remove) = (None, None);
        let n = self.rules.len();
        for (i, (text, error)) in self.rules.iter_mut().enumerate() {
            ui.horizontal(|ui| {
                if ui.add_enabled(i > 0, egui::Button::new("▲").small()).clicked() {
                    up = Some(i);
                }
                if ui.add_enabled(i + 1 < n, egui::Button::new("▼").small()).clicked() {
                    up = Some(i + 1);
                }
                let r = ui.add(egui::TextEdit::singleline(text).desired_width(520.0).font(egui::TextStyle::Monospace));
                edited |= r.lost_focus() || (r.changed() && !r.has_focus());
                if ui.small_button("−").on_hover_text("Remove the rule").clicked() {
                    remove = Some(i);
                }
            });
            if let Some(e) = error {
                ui.colored_label(ui.visuals().error_fg_color, e.as_str());
            }
        }
        if let Some(i) = up {
            self.rules.swap(i - 1, i);
            edited = true;
        }
        if let Some(i) = remove {
            self.rules.remove(i);
            edited = true;
        }
        if edited {
            self.rules_changed();
        }
        ui.horizontal_wrapped(|ui| {
            ui.label("➕");
            for (name, value) in &mut self.form.scope {
                ui.label(format!("{name} =="));
                ui.add(egui::TextEdit::singleline(value).hint_text("any").desired_width(70.0));
            }
            egui::ComboBox::from_id_salt("tuning-action").selected_text(ACTIONS[self.form.action].0).show_ui(
                ui,
                |ui| {
                    for (k, (a, what)) in ACTIONS.iter().enumerate() {
                        ui.selectable_value(&mut self.form.action, k, *a).on_hover_text(*what);
                    }
                },
            );
            ui.add(
                egui::TextEdit::singleline(&mut self.form.arg)
                    .hint_text(ACTIONS[self.form.action].1)
                    .desired_width(220.0),
            );
            let rule = self.form.rule();
            let check = PatternConfig::default().add_rule(&rule);
            let r = ui.add_enabled(check.is_ok(), egui::Button::new("Add rule"));
            let r = match &check {
                Ok(()) => r.on_hover_text(rule.clone()),
                Err(e) => r.on_disabled_hover_text(format!("{rule}\n{e}")),
            };
            if r.clicked() {
                self.add_rule(rule);
                self.form.arg.clear();
            }
        });
    }

    fn save(&mut self) {
        let Some(c) = &self.config else { return };
        let Some(path) = PatternConfig::saved_path() else {
            self.note = Some("No configuration folder to save to".into());
            return;
        };
        let write = path
            .parent()
            .map_or(Ok(()), std::fs::create_dir_all)
            .and_then(|_| nerka_core::write_atomic(&path, c.to_text()));
        match write {
            Ok(()) => {
                self.changed = false;
                self.note = Some(format!("Saved to {}", path.display()));
            }
            Err(e) => self.note = Some(format!("Not saved: {}: {e}", path.display())),
        }
    }

    fn load_file(&mut self) {
        let Some(path) = rfd::FileDialog::new().set_title("Load a pattern configuration").pick_file() else { return };
        match std::fs::read_to_string(&path).map_err(|e| e.to_string()).and_then(|t| PatternConfig::parse(&t)) {
            Ok(c) => {
                self.set(c, true);
                self.note = Some(format!("Loaded {} (not saved)", path.display()));
            }
            Err(e) => self.note = Some(format!("{}: {e}", path.display())),
        }
    }

    fn export(&mut self) {
        let Some(c) = &self.config else { return };
        let Some(path) =
            rfd::FileDialog::new().set_title("Export the pattern configuration").set_file_name("patterns").save_file()
        else {
            return;
        };
        self.note = Some(match nerka_core::write_atomic(&path, c.to_text()) {
            Ok(()) => format!("Exported to {}", path.display()),
            Err(e) => format!("Not exported: {}: {e}", path.display()),
        });
    }
}

#[cfg(test)]
impl TuningUi {
    /// As if `c` had been set in the pane a while ago.
    pub fn tune_for_test(&mut self, c: PatternConfig) {
        self.set(c, true);
        self.edited = Instant::now().checked_sub(DEBOUNCE);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nerka_core::pattern::{PatternOptions, detect};
    use nerka_core::{Progress, TextFile};

    fn run(config: &str) -> Patterns {
        let f = TextFile::from_bytes(
            "t",
            [
                "state changed from IDLE to RUN",
                "state changed from RUN to IDLE",
                "state changed from IDLE to RUN",
                "gear D",
                "gear P",
                "gear D",
            ]
            .join("\n")
            .into_bytes(),
        );
        let options =
            PatternOptions { lines: true, config: PatternConfig::parse(config).unwrap(), ..Default::default() };
        detect(&f, None, &options, &Progress::new()).unwrap()
    }

    #[test]
    fn a_change_says_what_joined() {
        let (a, b) = (run(""), run("new-slots 2\n"));
        let c = change(&a, &b);
        assert_eq!(c.joined.len(), 1, "{c:?}");
        assert!(c.joined[0].0.starts_with("state changed") && c.joined[0].1 == 2, "{c:?}");
        assert_eq!((c.new.len(), c.gone.len(), c.split.len()), (1, 2, 0), "{c:?}");
        let back = change(&b, &a);
        assert_eq!((back.split.len(), back.joined.len()), (1, 0));
        assert_eq!(change(&a, &a), Change::default());
        assert_eq!(change(&a, &a).summary(), "no change");
    }

    #[test]
    fn the_form_makes_rules() {
        let mut f =
            Form { scope: vec![("apid".into(), String::new()), ("ctid".into(), String::new())], ..Default::default() };
        f.fill(&[("apid".into(), "NAV".into())], "keep", "Elapsed".into());
        assert_eq!(f.rule(), "apid == NAV: keep Elapsed");
        f.fill(&[("ctid".into(), "a b".into())], "no-merge", String::new());
        assert_eq!(f.rule(), "ctid == \"a b\": no-merge");
        assert!(PatternConfig::default().add_rule(&f.rule()).is_ok());
    }

    /// A rule starts scoped to the tuned tab's kind (its profile, its
    /// protocol), and keeps it where a pattern gives no value; another
    /// tab's kind starts the scope again.
    #[test]
    fn a_rule_starts_scoped_to_the_files_kind() {
        let mut f = Form::default();
        let text = [("profile".to_owned(), "sail_log".to_owned())];
        f.scope_for(&["profile".into()], &text);
        assert_eq!(f.rule(), "profile == sail_log: keep");
        f.scope[0].1.clear();
        f.scope_for(&["profile".into()], &text);
        assert_eq!(f.rule(), "*: keep", "widened by the user, kept");
        f.fill(&[], "prefix", "/x /".into());
        assert_eq!(f.rule(), "profile == sail_log: prefix /x /");
        let fields: Vec<String> = ["ecu", "apid", "proto"].map(String::from).to_vec();
        f.scope_for(&fields, &[("proto".into(), "DLT".into())]);
        f.fill(&[("apid".into(), "NAV".into())], "keep", "Elapsed".into());
        assert_eq!(f.rule(), "apid == NAV AND proto == DLT: keep Elapsed");
        f.scope_for(&fields, &[]);
        assert_eq!(f.scope.iter().filter(|(_, v)| !v.is_empty()).count(), 0, "a capture of several protocols: any");
    }
}
