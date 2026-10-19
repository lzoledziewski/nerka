//! Presets in the GUI: a bar above the Selection pane to use, make, update,
//! save and delete them (`nerka_core::preset` holds the format).
//!
//! A preset is first kept for the session (marked `•`, gone when the app
//! closes) or saved to disk at once; a session preset can be saved later. A
//! saved preset can be updated (overwritten) from the active tab or deleted.

use eframe::egui::{self, RichText};
use nerka_core::preset::{self, Preset};
use std::path::PathBuf;

/// One preset: from disk (`path`), or kept for the session only.
#[derive(Debug, Clone)]
pub struct Entry {
    pub preset: Preset,
    pub path: Option<PathBuf>,
}

/// Every preset of the app, shared by the tabs.
#[derive(Debug, Default)]
pub struct Presets {
    pub entries: Vec<Entry>,
    /// Files that didn't load, and the last save / delete error.
    pub errors: Vec<String>,
    /// Name typed for a new preset, while that row is open.
    naming: Option<String>,
    /// Delete was clicked once for this preset.
    confirm_delete: Option<String>,
}

/// What the user asked for in the bar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    None,
    /// Use preset `i` in the active tab.
    Use(usize),
    /// A new preset from the active tab; `store` writes it to disk too.
    New {
        name: String,
        store: bool,
    },
    /// Overwrite preset `i` with the active tab (and its file, if saved).
    Update(usize),
    /// Write session preset `i` to disk.
    Store(usize),
    Delete(usize),
}

impl Presets {
    /// (Re)load the saved presets; session presets stay.
    pub fn load(&mut self) {
        self.entries.retain(|e| e.path.is_none());
        self.errors.clear();
        let Some(dir) = preset::default_dir() else { return };
        let (ok, bad) = preset::load_dir(&dir);
        for (path, p) in ok {
            // A session preset of the same name gives way to the file.
            self.entries.retain(|e| e.preset.name != p.name);
            self.entries.push(Entry { preset: p, path: Some(path) });
        }
        self.errors.extend(bad.iter().map(|e| e.to_string()));
        self.sort();
    }

    fn sort(&mut self) {
        self.entries.sort_by_key(|e| e.preset.name.to_lowercase());
    }

    pub fn find(&self, name: &str) -> Option<usize> {
        self.entries.iter().position(|e| e.preset.name == name)
    }

    /// Add or replace `p`; `store` writes it to disk. Returns its index.
    pub fn put(&mut self, p: Preset, store: bool) -> Result<usize, String> {
        let old = self.find(&p.name);
        let path = match old.and_then(|i| self.entries[i].path.clone()) {
            Some(path) => Some(path),
            None if store => {
                let dir = preset::default_dir().ok_or("no preset folder on this system")?;
                Some(preset::preset_path(&dir, &p.name).map_err(|e| e.to_string())?)
            }
            None => None,
        };
        if let Some(path) = &path {
            p.save(path).map_err(|e| e.to_string())?;
        }
        let name = p.name.clone();
        match old {
            Some(i) => self.entries[i] = Entry { preset: p, path },
            None => self.entries.push(Entry { preset: p, path }),
        }
        self.sort();
        Ok(self.find(&name).expect("just added"))
    }

    /// Remove preset `i` and its file.
    pub fn delete(&mut self, i: usize) -> Result<(), String> {
        if let Some(path) = &self.entries[i].path {
            std::fs::remove_file(path).map_err(|e| format!("{}: {e}", path.display()))?;
        }
        self.entries.remove(i);
        Ok(())
    }
}

/// The bar: the preset in use (`current`) with a list to pick another, and
/// a ⋮ menu to make, update, save and delete presets.
pub fn presets_bar(ui: &mut egui::Ui, presets: &mut Presets, current: Option<&str>) -> Action {
    let mut action = Action::None;
    let cur = current.and_then(|c| presets.find(c));
    ui.horizontal(|ui| {
        ui.label(RichText::new("Preset").strong())
            .on_hover_text("A saved selection: its queries and settings, parameters and the boxes' typed text");
        let label = |e: &Entry| if e.path.is_none() { format!("{} •", e.preset.name) } else { e.preset.name.clone() };
        let selected = cur.map_or("(none)".to_owned(), |i| label(&presets.entries[i]));
        egui::ComboBox::from_id_salt("preset")
            .width((ui.available_width() - 40.0).clamp(80.0, 220.0))
            .selected_text(selected)
            .show_ui(ui, |ui| {
                if presets.entries.is_empty() {
                    ui.weak("No presets yet: ⋮ → New from this tab");
                }
                for (i, e) in presets.entries.iter().enumerate() {
                    let tip = match &e.path {
                        Some(p) => format!("Use it in this tab\n{}", p.display()),
                        None => "Use it in this tab\nKept for this session only (•)".to_owned(),
                    };
                    if ui.selectable_label(cur == Some(i), label(e)).on_hover_text(tip).clicked() {
                        action = Action::Use(i);
                    }
                }
            });
        ui.menu_button("⋮", |ui| {
            if ui.button("New from this tab…").clicked() {
                presets.naming = Some(String::new());
                ui.close();
            }
            if let Some(i) = cur {
                let e = &presets.entries[i];
                let name = e.preset.name.clone();
                let tip = if e.path.is_some() { "Overwrites its file" } else { "Kept for this session" };
                if ui.button(format!("Update '{name}' from this tab")).on_hover_text(tip).clicked() {
                    action = Action::Update(i);
                    ui.close();
                }
                if e.path.is_none() && ui.button(format!("Save '{name}' to disk")).clicked() {
                    action = Action::Store(i);
                    ui.close();
                }
                let sure = presets.confirm_delete.as_deref() == Some(&name);
                let del = if sure { format!("Really delete '{name}'?") } else { format!("Delete '{name}'…") };
                if ui.button(del).clicked() {
                    if sure {
                        action = Action::Delete(i);
                        presets.confirm_delete = None;
                        ui.close();
                    } else {
                        presets.confirm_delete = Some(name);
                    }
                }
            }
        })
        .response
        .on_hover_text("New, update, save or delete presets");
    });
    if let Some(name) = &mut presets.naming {
        let mut close = false;
        ui.horizontal(|ui| {
            let r = ui.add(egui::TextEdit::singleline(name).hint_text("preset name").desired_width(120.0));
            r.request_focus();
            let n = name.trim().to_owned();
            let problem = if n.is_empty() {
                Some("Type a name")
            } else if !preset::valid_name(&n) {
                Some("No . \" # : / \\ * ? < > | and no spaces at the ends")
            } else if presets.entries.iter().any(|e| e.preset.name == n) {
                Some("A preset with this name exists: pick it and use Update")
            } else {
                None
            };
            let ok = problem.is_none();
            let keep = ui.add_enabled(ok, egui::Button::new("Keep")).on_hover_text("For this session only");
            let save = ui.add_enabled(ok, egui::Button::new("Save")).on_hover_text("To disk");
            let enter = ok && r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            if keep.clicked() || enter {
                action = Action::New { name: n.clone(), store: false };
                close = true;
            }
            if save.clicked() {
                action = Action::New { name: n.clone(), store: true };
                close = true;
            }
            if ui.button("Cancel").clicked() || ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                close = true;
            }
            if let Some(p) = problem
                && !n.is_empty()
            {
                ui.colored_label(ui.visuals().error_fg_color, p);
            }
        });
        if close {
            presets.naming = None;
        }
    }
    for e in &presets.errors {
        ui.colored_label(ui.visuals().error_fg_color, RichText::new(e).small());
    }
    action
}
