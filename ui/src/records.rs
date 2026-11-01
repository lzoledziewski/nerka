//! A tab's records: how its text files are split into rows (one per line,
//! or multi-line records), chosen per tab (its menu → Records…) and
//! remembered per file in `<config>/nerka/records` (`spec<TAB>path` per
//! line; a file read as lines isn't listed).

use eframe::egui;
use nerka_core::input::records::RecordSplit;
use std::path::{Path, PathBuf};

fn memo_path() -> Option<PathBuf> {
    nerka_core::config_dir().map(|d| d.join("records"))
}

/// (path, spec) of every remembered file.
fn read_memo() -> Vec<(PathBuf, String)> {
    let text = memo_path().and_then(|p| std::fs::read_to_string(p).ok()).unwrap_or_default();
    text.lines().filter_map(|l| l.split_once('\t')).map(|(spec, path)| (PathBuf::from(path), spec.to_owned())).collect()
}

/// How `path` was last read: its remembered split, else lines.
pub fn remembered(path: &Path) -> RecordSplit {
    read_memo()
        .into_iter()
        .find(|(p, _)| p == path)
        .and_then(|(_, spec)| RecordSplit::parse(&spec).ok())
        .unwrap_or_default()
}

/// Remember how `path` is read (lines: forget it).
pub fn remember(path: &Path, split: &RecordSplit) -> std::io::Result<()> {
    let Some(file) = memo_path() else { return Ok(()) };
    let mut memo: Vec<(PathBuf, String)> = read_memo().into_iter().filter(|(p, _)| p != path).collect();
    if *split != RecordSplit::Lines {
        memo.push((path.to_path_buf(), split.spec()));
    }
    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(file, memo.iter().map(|(p, s)| format!("{s}\t{}\n", p.display())).collect::<String>())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Lines,
    Start,
    Blank,
    Separator,
}

/// The Records window's fields, as typed.
#[derive(Debug, Clone)]
pub struct Edit {
    kind: Kind,
    start: String,
    sep: String,
    error: Option<String>,
}

impl Edit {
    /// The window for a tab read as `split`.
    pub fn of(split: &RecordSplit) -> Edit {
        let spec = split.spec();
        let re = |prefix: &str| spec.strip_prefix(prefix).map(|s| s.trim_matches('/').to_owned()).unwrap_or_default();
        let kind = match split {
            RecordSplit::Lines => Kind::Lines,
            RecordSplit::Start(_) => Kind::Start,
            RecordSplit::Blank => Kind::Blank,
            RecordSplit::Separator(_) => Kind::Separator,
        };
        let start = Some(re("start=")).filter(|s| !s.is_empty()).unwrap_or_else(|| r"\d{4}-\d\d-\d\d".to_owned());
        Edit { kind, start, sep: re("sep="), error: None }
    }

    fn spec(&self) -> String {
        match self.kind {
            Kind::Lines => "lines".into(),
            Kind::Blank => "blank".into(),
            Kind::Start => format!("start={}", self.start),
            Kind::Separator => format!("sep={}", self.sep),
        }
    }
}

/// What the window asks for.
pub enum Outcome {
    Open,
    Apply(RecordSplit),
    Close,
}

/// The Records window of the tab `title`.
pub fn window(ctx: &egui::Context, title: &str, edit: &mut Edit) -> Outcome {
    let (mut open, mut apply, mut cancel) = (true, false, false);
    egui::Window::new(format!("Records of {title}"))
        .id(egui::Id::unique("records-window"))
        .open(&mut open)
        .collapsible(false)
        .resizable(false)
        .show(ctx, |ui| {
            ui.weak("How the tab's text files are split into rows. A record of several lines is one row: filters and searches see all of it, and Enter shows it whole.");
            ui.separator();
            ui.radio_value(&mut edit.kind, Kind::Lines, "One row per line");
            ui.horizontal(|ui| {
                ui.radio_value(&mut edit.kind, Kind::Start, "A line matching this starts a record:");
                let r = ui.add_enabled(
                    edit.kind == Kind::Start,
                    egui::TextEdit::singleline(&mut edit.start).font(egui::TextStyle::Monospace).desired_width(220.0),
                );
                r.on_hover_text("A regex matched at the line's start, e.g. a timestamp: \\d{4}-\\d\\d-\\d\\d");
            });
            ui.radio_value(&mut edit.kind, Kind::Blank, "Blank lines separate records");
            ui.horizontal(|ui| {
                ui.radio_value(&mut edit.kind, Kind::Separator, "Lines matching this separate records:");
                ui.add_enabled(
                    edit.kind == Kind::Separator,
                    egui::TextEdit::singleline(&mut edit.sep)
                        .font(egui::TextStyle::Monospace)
                        .hint_text("----")
                        .desired_width(220.0),
                );
            });
            if let Some(e) = &edit.error {
                ui.colored_label(ui.visuals().error_fg_color, e);
            }
            ui.separator();
            ui.horizontal(|ui| {
                apply = ui.button("Apply").on_hover_text("Read the files again; filter, search and bookmarks stay").clicked();
                cancel = ui.button("Cancel").clicked();
            });
        });
    if apply {
        match RecordSplit::parse(&edit.spec()) {
            Ok(split) => return Outcome::Apply(split),
            Err(e) => edit.error = Some(e.to_string()),
        }
    }
    if cancel || !open { Outcome::Close } else { Outcome::Open }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_window_reads_back_what_it_shows() {
        for spec in ["lines", "blank", r"start=/\d{4}-/", "sep=/----/"] {
            let split = RecordSplit::parse(spec).unwrap();
            let e = Edit::of(&split);
            assert_eq!(RecordSplit::parse(&e.spec()).unwrap(), split, "{spec}");
        }
        // A tab read as lines offers a timestamp as the start pattern.
        assert_eq!(Edit::of(&RecordSplit::Lines).start, r"\d{4}-\d\d-\d\d");
    }
}
