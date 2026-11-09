//! Kept plots, shared by the tabs (owner, 2026-10-28: switch between plots
//! without building them again, save them and use them again). A plot has
//! no name until it is kept for the session (Keep, `•`) or saved to disk
//! (Save, `<config>/nerka/plots`, format in `nerka_core::plot`).
//!
//! A kept plot is like a preset (owner, 2026-11-09: U5, reversing
//! 2026-10-28's "switching keeps the current plot in its entry"): it
//! changes only on Keep changes or Save. Changes made in a tab are that
//! tab's draft, marked `*`; switching away keeps the draft in the tab for
//! the session, switching back shows it, Revert drops it.

use crate::plot::{PlotAction, PlotState, SavedPlot};
use nerka_core::plot::{self, PlotLayout};
use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct Entry {
    /// The plot as last kept (switching away keeps it here).
    pub layout: PlotLayout,
    /// Its file, and the plot as written there; none: this session only.
    pub file: Option<(PathBuf, PlotLayout)>,
}

#[derive(Debug, Default)]
pub struct Plots {
    pub entries: Vec<Entry>,
    /// Files that didn't load, and the last save / delete error.
    pub errors: Vec<String>,
}

impl Plots {
    /// (Re)load the saved plots; session plots stay.
    pub fn load(&mut self) {
        let Some(dir) = plot::default_dir() else { return };
        self.load_from(&dir);
    }

    fn load_from(&mut self, dir: &std::path::Path) {
        self.entries.retain(|e| e.file.is_none());
        self.errors.clear();
        let (ok, bad) = plot::load_dir(dir);
        for (path, p) in ok {
            self.entries.retain(|e| e.layout.name != p.name);
            self.entries.push(Entry { layout: p.clone(), file: Some((path, p)) });
        }
        self.errors.extend(bad.iter().map(|e| e.to_string()));
        self.sort();
    }

    fn sort(&mut self) {
        self.entries.sort_by_key(|e| e.layout.name.to_lowercase());
    }

    fn find(&self, name: &str) -> Option<usize> {
        self.entries.iter().position(|e| e.layout.name == name)
    }

    /// The list a tab's plot shows: a plot is marked changed when the tab
    /// has changes to it that aren't kept (its current plot, or a draft).
    pub fn listed(&self, state: &PlotState) -> Vec<SavedPlot> {
        self.entries
            .iter()
            .map(|e| {
                let name = &e.layout.name;
                let changed = if state.name.as_ref() == Some(name) {
                    state.layout(name) != e.layout
                } else {
                    state.drafts.contains_key(name)
                };
                SavedPlot { name: name.clone(), session: e.file.is_none(), changed }
            })
            .collect()
    }

    /// Do what a tab's plot asked, on `state` (that tab's plot). Returns the
    /// name of a deleted plot (other tabs showing it forget its name and
    /// their drafts of it).
    pub fn act(&mut self, state: &mut PlotState, action: PlotAction) -> Result<Option<String>, String> {
        match action {
            PlotAction::Use(name) => {
                let i = self.find(&name).ok_or(format!("no kept plot '{name}'"))?;
                self.stash_draft(state);
                let layout = state.drafts.remove(&name).unwrap_or_else(|| self.entries[i].layout.clone());
                state.apply(&layout);
                Ok(None)
            }
            PlotAction::Keep { name, store } => {
                if self.find(&name).is_some() {
                    return Err(format!("a plot '{name}' is kept already"));
                }
                // A plot kept by another name before stays as it was kept;
                // the changes go to the new one.
                let layout = state.layout(&name);
                let file = if store { Some(self.write(&layout, None)?) } else { None };
                if let Some(old) = state.name.take() {
                    state.drafts.remove(&old);
                }
                self.entries.push(Entry { layout, file });
                self.sort();
                state.name = Some(name);
                Ok(None)
            }
            PlotAction::Blank => {
                self.stash_draft(state);
                state.blank();
                Ok(None)
            }
            PlotAction::Update | PlotAction::Save => {
                let name = state.name.clone().ok_or("this plot isn't kept: ⋮ → Keep this plot as…")?;
                let i = self.find(&name).ok_or(format!("no kept plot '{name}'"))?;
                let layout = state.layout(&name);
                let file = if action == PlotAction::Save {
                    let path = self.entries[i].file.as_ref().map(|(p, _)| p.clone());
                    Some(self.write(&layout, path)?)
                } else {
                    self.entries[i].file.clone()
                };
                self.entries[i] = Entry { layout, file };
                state.drafts.remove(&name);
                Ok(None)
            }
            PlotAction::Revert => {
                let name = state.name.clone().ok_or("this plot isn't kept")?;
                let i = self.find(&name).ok_or(format!("no kept plot '{name}'"))?;
                state.drafts.remove(&name);
                state.apply(&self.entries[i].layout);
                Ok(None)
            }
            PlotAction::Delete(name) => {
                let i = self.find(&name).ok_or(format!("no kept plot '{name}'"))?;
                if let Some((path, _)) = &self.entries[i].file {
                    std::fs::remove_file(path).map_err(|e| format!("{}: {e}", path.display()))?;
                }
                self.entries.remove(i);
                state.drafts.remove(&name);
                Ok(Some(name))
            }
        }
    }

    /// `state` is about to show another plot: its changes to the kept plot
    /// it shows stay with the tab as a draft, the entry as it was.
    fn stash_draft(&self, state: &mut PlotState) {
        let Some(name) = state.name.clone() else { return };
        let Some(i) = self.find(&name) else { return };
        let current = state.layout(&name);
        if current == self.entries[i].layout {
            state.drafts.remove(&name);
        } else {
            state.drafts.insert(name, current);
        }
    }

    /// Write `layout` to `path` (default: the plots folder).
    fn write(&self, layout: &PlotLayout, path: Option<PathBuf>) -> Result<(PathBuf, PlotLayout), String> {
        let path = match path {
            Some(p) => p,
            None => {
                let dir = plot::default_dir().ok_or("no plots folder on this system")?;
                plot::plot_path(&dir, &layout.name).map_err(|e| e.to_string())?
            }
        };
        layout.save(&path).map_err(|e| e.to_string())?;
        Ok((path, layout.clone()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state_with(names: &[&str]) -> PlotState {
        let mut s = PlotState::default();
        names.iter().for_each(|n| s.toggle(n));
        s
    }

    #[test]
    fn switching_keeps_each_plot_as_it_was() {
        let mut plots = Plots::default();
        let mut s = state_with(&["speed", "rpm"]);
        s.events.insert("rpm".into());
        plots.act(&mut s, PlotAction::Keep { name: "drive".into(), store: false }).unwrap();
        assert_eq!(s.name.as_deref(), Some("drive"));
        // New plot: blank and unnamed (owner, 2026-10-28), until kept.
        plots.act(&mut s, PlotAction::Blank).unwrap();
        assert!(s.plotted.is_empty() && s.events.is_empty() && s.name.is_none() && s.shown);
        assert_eq!(plots.entries.len(), 1, "a blank plot isn't kept by itself");
        s.toggle("temp");
        plots.act(&mut s, PlotAction::Keep { name: "temps".into(), store: false }).unwrap();
        assert!(plots.listed(&s).iter().all(|p| p.session && !p.changed), "nothing changed since kept");

        // Back to "drive": as it was; "temps" kept with its lane.
        plots.act(&mut s, PlotAction::Use("drive".into())).unwrap();
        assert_eq!(s.plotted, ["speed", "rpm"]);
        assert!(s.events.contains("rpm"));
        plots.act(&mut s, PlotAction::Use("temps".into())).unwrap();
        assert_eq!(s.plotted, ["temp"]);

        assert_eq!(plots.act(&mut s, PlotAction::Delete("temps".into())), Ok(Some("temps".into())));
        assert_eq!(plots.listed(&s).iter().map(|p| p.name.as_str()).collect::<Vec<_>>(), ["drive"]);
        assert!(plots.act(&mut s, PlotAction::Keep { name: "drive".into(), store: false }).is_err());
    }

    /// A kept plot is a preset (owner, 2026-11-09: U5): a change without
    /// Keep changes leaves it as kept; the tab keeps its draft, marked, and
    /// shows it again; Revert drops it; Keep changes writes it.
    #[test]
    fn a_kept_plot_changes_only_on_keep() {
        let mut plots = Plots::default();
        let mut a = state_with(&["speed"]);
        plots.act(&mut a, PlotAction::Keep { name: "drive".into(), store: false }).unwrap();
        a.toggle("rpm");
        assert!(plots.listed(&a)[0].changed, "the tab's change is marked");
        plots.act(&mut a, PlotAction::Blank).unwrap();
        assert_eq!(plots.entries[0].layout.lanes.len(), 1, "the kept plot is as kept");
        assert!(plots.listed(&a)[0].changed, "the draft is marked after switching away");

        // Another tab uses the plot as kept, not the first tab's draft.
        let mut b = PlotState::default();
        plots.act(&mut b, PlotAction::Use("drive".into())).unwrap();
        assert_eq!(b.plotted, ["speed"]);
        assert!(!plots.listed(&b)[0].changed);

        // Back in the first tab: the draft; Revert: as kept.
        plots.act(&mut a, PlotAction::Use("drive".into())).unwrap();
        assert_eq!(a.plotted, ["speed", "rpm"]);
        plots.act(&mut a, PlotAction::Revert).unwrap();
        assert_eq!(a.plotted, ["speed"]);
        assert!(a.drafts.is_empty() && !plots.listed(&a)[0].changed);

        // Keep changes: the entry changes, the draft is gone.
        a.toggle("temp");
        plots.act(&mut a, PlotAction::Update).unwrap();
        assert_eq!(plots.entries[0].layout.lanes.len(), 2);
        assert!(plots.entries[0].file.is_none(), "kept for the session only");
        assert!(!plots.listed(&a)[0].changed);
        // A change undone by hand isn't a draft.
        a.toggle("rpm");
        a.toggle("rpm");
        plots.act(&mut a, PlotAction::Blank).unwrap();
        assert!(a.drafts.is_empty());

        // Kept by a new name: the old one stays as kept, no draft of it.
        plots.act(&mut a, PlotAction::Use("drive".into())).unwrap();
        a.toggle("oil");
        plots.act(&mut a, PlotAction::Keep { name: "drive2".into(), store: false }).unwrap();
        assert_eq!(plots.entries[0].layout.lanes.len(), 2);
        assert_eq!(plots.entries[1].layout.lanes.len(), 3);
        assert!(plots.listed(&a).iter().all(|p| !p.changed));
    }

    #[test]
    fn a_saved_plot_is_marked_changed_until_saved() {
        let dir = std::env::temp_dir().join(format!("nerka-gui-plots-{}", std::process::id()));
        let path = plot::plot_path(&dir, "drive").unwrap();
        let layout = PlotLayout { name: "drive".into(), ..state_with(&["speed"]).layout("drive") };
        layout.save(&path).unwrap();
        let mut plots = Plots::default();
        plots.load_from(&dir);
        assert!(plots.errors.is_empty());
        let mut s = PlotState::default();
        plots.act(&mut s, PlotAction::Use("drive".into())).unwrap();
        assert_eq!(s.plotted, ["speed"]);
        assert!(!plots.listed(&s)[0].changed);
        s.toggle("rpm");
        assert!(plots.listed(&s)[0].changed);
        plots.act(&mut s, PlotAction::Save).unwrap();
        assert!(!plots.listed(&s)[0].changed);
        let again = plot::parse_plot("t", &std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(again.lanes.len(), 2);
        // Switched away unsaved: still marked.
        s.toggle("rpm");
        plots.act(&mut s, PlotAction::Blank).unwrap();
        assert!(plots.listed(&s).iter().find(|p| p.name == "drive").unwrap().changed);
        plots.act(&mut s, PlotAction::Delete("drive".into())).unwrap();
        assert!(!path.exists());
        std::fs::remove_dir_all(&dir).ok();
    }
}
