//! Kept plots, shared by the tabs (owner, 2026-10-28: switch between plots
//! without building them again, save them and use them again). A plot is
//! kept for the session (`•`) or saved to disk (`<config>/nerka/plots`,
//! format in `nerka_core::plot`). Switching keeps the current plot as it is
//! in its entry first, so nothing is lost; a saved plot changed since it was
//! written is marked `*` until it is saved.

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

    /// The list a tab's plot shows: a saved plot is marked changed when it
    /// differs from its file (`state`'s own one as `state` shows it).
    pub fn listed(&self, state: &PlotState) -> Vec<SavedPlot> {
        self.entries
            .iter()
            .map(|e| {
                let name = &e.layout.name;
                let changed = e.file.as_ref().is_some_and(|(_, written)| {
                    if state.name.as_ref() == Some(name) {
                        *written != state.layout(name)
                    } else {
                        *written != e.layout
                    }
                });
                SavedPlot { name: name.clone(), session: e.file.is_none(), changed }
            })
            .collect()
    }

    /// Do what a tab's plot asked, on `state` (that tab's plot). Returns the
    /// name of a deleted plot (other tabs showing it forget its name).
    pub fn act(&mut self, state: &mut PlotState, action: PlotAction) -> Result<Option<String>, String> {
        match action {
            PlotAction::Use(name) => {
                self.keep_current(state);
                let i = self.find(&name).ok_or(format!("no kept plot '{name}'"))?;
                state.apply(&self.entries[i].layout);
                Ok(None)
            }
            PlotAction::New { name, store, empty } => {
                if self.find(&name).is_some() {
                    return Err(format!("a plot '{name}' is kept already"));
                }
                self.keep_current(state);
                let layout =
                    if empty { PlotLayout { name: name.clone(), ..Default::default() } } else { state.layout(&name) };
                let file = if store { Some(self.write(&layout, None)?) } else { None };
                self.entries.push(Entry { layout: layout.clone(), file });
                self.sort();
                state.apply(&layout);
                Ok(None)
            }
            PlotAction::Save => {
                let name = state.name.clone().ok_or("this plot isn't kept: ⋮ → Keep this plot as…")?;
                let i = self.find(&name).ok_or(format!("no kept plot '{name}'"))?;
                let layout = state.layout(&name);
                let path = self.entries[i].file.as_ref().map(|(p, _)| p.clone());
                let file = self.write(&layout, path)?;
                self.entries[i] = Entry { layout, file: Some(file) };
                Ok(None)
            }
            PlotAction::Delete(name) => {
                let i = self.find(&name).ok_or(format!("no kept plot '{name}'"))?;
                if let Some((path, _)) = &self.entries[i].file {
                    std::fs::remove_file(path).map_err(|e| format!("{}: {e}", path.display()))?;
                }
                self.entries.remove(i);
                Ok(Some(name))
            }
        }
    }

    /// `state` is about to show another plot: keep it in its entry.
    fn keep_current(&mut self, state: &PlotState) {
        if let Some(i) = state.name.as_deref().and_then(|n| self.find(n)) {
            self.entries[i].layout = state.layout(&self.entries[i].layout.name);
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
        plots.act(&mut s, PlotAction::New { name: "drive".into(), store: false, empty: false }).unwrap();
        assert_eq!(s.name.as_deref(), Some("drive"));
        plots.act(&mut s, PlotAction::New { name: "temps".into(), store: false, empty: true }).unwrap();
        assert!(s.plotted.is_empty() && s.shown);
        s.toggle("temp");
        assert!(plots.listed(&s).iter().all(|p| p.session && !p.changed), "session plots aren't marked changed");

        // Back to "drive": as it was; "temps" kept with its lane.
        plots.act(&mut s, PlotAction::Use("drive".into())).unwrap();
        assert_eq!(s.plotted, ["speed", "rpm"]);
        assert!(s.events.contains("rpm"));
        plots.act(&mut s, PlotAction::Use("temps".into())).unwrap();
        assert_eq!(s.plotted, ["temp"]);

        assert_eq!(plots.act(&mut s, PlotAction::Delete("temps".into())), Ok(Some("temps".into())));
        assert_eq!(plots.listed(&s).iter().map(|p| p.name.as_str()).collect::<Vec<_>>(), ["drive"]);
        assert!(plots.act(&mut s, PlotAction::New { name: "drive".into(), store: false, empty: true }).is_err());
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
        plots.act(&mut s, PlotAction::New { name: "other".into(), store: false, empty: true }).unwrap();
        assert!(plots.listed(&s).iter().find(|p| p.name == "drive").unwrap().changed);
        plots.act(&mut s, PlotAction::Delete("drive".into())).unwrap();
        assert!(!path.exists());
        std::fs::remove_dir_all(&dir).ok();
    }
}
