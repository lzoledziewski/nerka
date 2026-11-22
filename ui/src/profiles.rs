//! Profiles in the GUI: the saved ones (`<config>/nerka/profiles`), read
//! when the window starts and on Reload configuration, used for every file
//! opened from then on; and the window that edits them (Settings →
//! Profiles…): the file's lines as text, checked as typed.

use eframe::egui;
use nerka_core::input::profile::{Profile, Profiles};
use std::sync::{Arc, RwLock};

static CURRENT: RwLock<Option<Arc<Profiles>>> = RwLock::new(None);

/// The profiles files are opened with now.
pub fn current() -> Arc<Profiles> {
    if let Some(p) = CURRENT.read().unwrap_or_else(|e| e.into_inner()).as_ref() {
        return p.clone();
    }
    load();
    CURRENT.read().unwrap_or_else(|e| e.into_inner()).clone().unwrap_or_default()
}

/// Read the saved profiles again; the lines that aren't profiles.
pub fn load() -> Vec<String> {
    let (ps, errors) = Profiles::load_saved();
    set(ps);
    errors
}

fn set(ps: Profiles) {
    *CURRENT.write().unwrap_or_else(|e| e.into_inner()) = Some(Arc::new(ps));
}

/// The names to force a file to (the Files window), the default last.
pub fn names() -> Vec<String> {
    let ps = current();
    let mut out: Vec<String> = ps.iter().map(|p| p.name().to_owned()).collect();
    if !out.iter().any(|n| n == nerka_core::input::profile::DEFAULT) {
        out.push(nerka_core::input::profile::DEFAULT.into());
    }
    out
}

/// The Profiles window's text, as typed.
pub struct Editor {
    text: String,
    /// What the text was when the window opened or was saved.
    saved: String,
}

impl Editor {
    pub fn open() -> Editor {
        let ps = current();
        let mut text: String = ps.iter().map(|p| p.spec() + "\n").collect();
        if text.is_empty() {
            text = "# e.g. app: name app-*.log; time /^(?<time>\\S+ \\S+)/; zone local\n".into();
        }
        Editor { saved: text.clone(), text }
    }
}

/// What the window asks for.
pub enum Outcome {
    Open,
    Close,
    /// Saved: say this.
    Saved(String),
}

/// The Profiles window.
pub fn window(ctx: &egui::Context, edit: &mut Editor) -> Outcome {
    let (mut open, mut save, mut close) = (true, false, false);
    let (parsed, errors) = Profiles::parse_text(&edit.text);
    egui::Window::new("Profiles")
        .id(egui::Id::unique("profiles-window"))
        .open(&mut open)
        .collapsible(false)
        .default_width(760.0)
        .show(ctx, |ui| {
            ui.weak(
                "How text files are read: one profile per line, NAME: CLAUSES. A file gets the profile forced \
                 on it in its tab's Files window, else the first whose name rule (and first-line rule, if it \
                 has one) matches, else the first whose first-line rule alone matches, else the default (the \
                 one named default, or the built-in: the time each line starts with).",
            );
            ui.add_space(4.0);
            egui::Grid::new("profile-clauses").num_columns(2).spacing([12.0, 2.0]).show(ui, |ui| {
                for (c, what) in [
                    ("name *.log, app-*.txt", "files whose name matches (any case)"),
                    ("first /REGEX/", "files whose first non-empty line matches"),
                    ("records lines", "one row per line"),
                    ("records start=/REGEX/", "a record starts at each line matching"),
                    ("records sep=/REGEX/", "records between lines matching (not part of them)"),
                    (
                        "records blank | json | auto",
                        "records between blank lines; JSON values; JSON or lines (default)",
                    ),
                    ("time auto | none | /REGEX/", "the time a line starts with; none; a regex's group time"),
                    ("zone utc | local | +02:00", "the zone of times written without one (default: as shown)"),
                    (
                        "numbers s | ms | us | ns",
                        "a time written as a number (1760695201250, [ 12.345]) is s, ms, µs or ns since 1970",
                    ),
                    ("numbers auto", "… guessed by its size (default): up to 11 digits seconds, then ms, µs, ns"),
                    ("signal SPEC", "a signal on for its files, as in the Signals panel (repeatable)"),
                ] {
                    ui.monospace(c);
                    ui.weak(what);
                    ui.end_row();
                }
            });
            ui.separator();
            egui::ScrollArea::vertical().max_height(320.0).show(ui, |ui| {
                ui.add(
                    egui::TextEdit::multiline(&mut edit.text)
                        .font(egui::TextStyle::Monospace)
                        .desired_rows(8)
                        .desired_width(f32::INFINITY),
                );
            });
            for e in &errors {
                ui.colored_label(ui.visuals().error_fg_color, e);
            }
            ui.separator();
            ui.horizontal(|ui| {
                let changed = edit.text != edit.saved;
                save = ui
                    .add_enabled(changed && errors.is_empty(), egui::Button::new("Save"))
                    .on_hover_text(
                        "Write them; files opened from now on use them (a tab's Reload reads its files again)",
                    )
                    .on_disabled_hover_text(if errors.is_empty() {
                        "Nothing changed"
                    } else {
                        "Fix the lines above first"
                    })
                    .clicked();
                close = ui.button("Close").clicked();
                ui.weak(format!("{} profile{}", parsed.len(), if parsed.len() == 1 { "" } else { "s" }));
            });
        });
    if save {
        return match parsed.save() {
            Ok(()) => {
                let names: Vec<&str> = parsed.iter().map(Profile::name).collect();
                set(parsed.clone());
                edit.saved = edit.text.clone();
                Outcome::Saved(format!(
                    "Profiles saved ({}): files opened from now on use them",
                    if names.is_empty() { "none".into() } else { names.join(", ") }
                ))
            }
            Err(e) => Outcome::Saved(format!("Could not save the profiles: {e}")),
        };
    }
    if close || !open { Outcome::Close } else { Outcome::Open }
}
