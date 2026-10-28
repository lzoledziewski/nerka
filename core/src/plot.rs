//! Saved plots: which signals a plot shows and how, so a plot can be kept,
//! switched to and used again on another file (owner, 2026-10-28). A plot
//! refers to signals by name; the GUI draws it (`ui/src/plot.rs`).
//!
//! File format (`*.nerka-plot`, UTF-8), one item per line:
//!
//! ```text
//! # A comment.
//! plot speeds
//! names 150
//! bookmarks hidden
//! lane speed events
//! lane rpm y visible
//! lane temp y 0 120
//! marker temp 100 "too hot"
//! ```
//!
//! ```text
//! file := "plot" NAME line*
//! line := "lane" SIGNAL ["events"] ["y" ("all" | "visible" | LO HI)]   -- in lane order
//!       | "marker" SIGNAL VALUE [LABEL]                                -- SIGNAL: a lane
//!       | "names" WIDTH                                                -- the names' column, points
//!       | "bookmarks" "hidden"
//! ```
//!
//! NAMEs, SIGNALs and LABELs are quoted (`"…"`, with `\"` and `\\`) when
//! they hold spaces or quotes. Time cursors and the zoom belong to a file,
//! so they aren't kept.

use crate::pack::PackError;
use crate::preset::{quote, valid_name, words};
use std::path::{Path, PathBuf};

/// File extension of saved plots.
pub const PLOT_EXTENSION: &str = "nerka-plot";

/// The per-user folder of saved plots (`<config dir>/plots`).
pub fn default_dir() -> Option<PathBuf> {
    crate::config_dir().map(|d| d.join("plots"))
}

/// A number lane's y axis.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub enum YAxis {
    /// Every value of the signal fits.
    #[default]
    All,
    /// The values in the visible x range fit (changes while zooming).
    Visible,
    /// From, to.
    Fixed(f64, f64),
}

/// A horizontal line across a number lane (a limit, a threshold).
#[derive(Debug, Clone, PartialEq)]
pub struct Marker {
    pub value: f64,
    /// Shown next to the value; may be empty.
    pub label: String,
}

/// One lane: a signal and how it is shown.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Lane {
    pub signal: String,
    /// Its events are ticked.
    pub events: bool,
    pub y: YAxis,
    pub markers: Vec<Marker>,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct PlotLayout {
    pub name: String,
    pub lanes: Vec<Lane>,
    /// Width of the lane names' column; none: the default.
    pub names_width: Option<f32>,
    pub hide_bookmarks: bool,
}

fn num(v: f64) -> String {
    // Shortest text that reads back as the same number.
    format!("{v}")
}

impl PlotLayout {
    /// The file text.
    pub fn serialize(&self) -> String {
        let mut out = format!("plot {}\n", quote(&self.name));
        if let Some(w) = self.names_width {
            out += &format!("names {w}\n");
        }
        if self.hide_bookmarks {
            out += "bookmarks hidden\n";
        }
        for l in &self.lanes {
            out += &format!("lane {}", quote(&l.signal));
            if l.events {
                out += " events";
            }
            match l.y {
                YAxis::All => {}
                YAxis::Visible => out += " y visible",
                YAxis::Fixed(lo, hi) => out += &format!(" y {} {}", num(lo), num(hi)),
            }
            out.push('\n');
        }
        for l in &self.lanes {
            for m in &l.markers {
                out += &format!("marker {} {}", quote(&l.signal), num(m.value));
                if !m.label.is_empty() {
                    out += &format!(" {}", quote(&m.label));
                }
                out.push('\n');
            }
        }
        out
    }

    /// Write to `path` (directories are created).
    pub fn save(&self, path: &Path) -> Result<(), PackError> {
        let file = path.display().to_string();
        let text = self.serialize();
        parse_plot(&file, &text)?;
        let err = |e: std::io::Error| PackError { file: file.clone(), pos: None, msg: e.to_string() };
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(err)?;
        }
        std::fs::write(path, text).map_err(err)
    }
}

/// `<dir>/<name>.nerka-plot`, if `name` can be a file name (the rules of
/// preset names).
pub fn plot_path(dir: &Path, name: &str) -> Result<PathBuf, PackError> {
    let path = dir.join(format!("{name}.{PLOT_EXTENSION}"));
    if !valid_name(name) {
        return Err(PackError {
            file: path.display().to_string(),
            pos: None,
            msg: format!("invalid plot name '{name}' (no . \" # : / \\ * ? < > | and no spaces at the ends)"),
        });
    }
    Ok(path)
}

/// Every `*.nerka-plot` in `dir` (sorted by file name), and the files that
/// didn't load. A missing directory has none.
pub fn load_dir(dir: &Path) -> (Vec<(PathBuf, PlotLayout)>, Vec<PackError>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return (vec![], vec![]) };
    let mut files: Vec<PathBuf> = rd
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.is_file() && p.extension().is_some_and(|x| x == PLOT_EXTENSION))
        .collect();
    files.sort();
    let (mut ok, mut bad) = (Vec::new(), Vec::new());
    for f in files {
        let file = f.display().to_string();
        match std::fs::read_to_string(&f) {
            Ok(t) => match parse_plot(&file, &t) {
                Ok(p) => ok.push((f, p)),
                Err(e) => bad.push(e),
            },
            Err(e) => bad.push(PackError { file, pos: None, msg: e.to_string() }),
        }
    }
    (ok, bad)
}

/// Parse a plot file. `file` is used in error messages.
pub fn parse_plot(file: &str, text: &str) -> Result<PlotLayout, PackError> {
    let err = |ln: usize, col: usize, msg: String| PackError { file: file.to_owned(), pos: Some((ln, col)), msg };
    let mut p = PlotLayout::default();
    let mut named = false;
    for (ln, line) in text.lines().enumerate() {
        let content = line.trim_end();
        if content.trim_start().is_empty() || content.trim_start().starts_with('#') {
            continue;
        }
        let w = words(content).map_err(|col| err(ln, col, "missing closing '\"'".into()))?;
        let end_col = content.chars().count();
        let col = |i: usize| w.get(i).map_or(end_col, |x| x.1);
        let number = |i: usize| -> Result<f64, PackError> {
            w.get(i)
                .and_then(|(t, _)| t.parse::<f64>().ok())
                .filter(|v| v.is_finite())
                .ok_or_else(|| err(ln, col(i), "expected a number".into()))
        };
        let key = w[0].0.as_str();
        if !named && key != "plot" {
            return Err(err(ln, w[0].1, "expected 'plot NAME' as the first line".into()));
        }
        match key {
            "plot" if !named => match w.len() {
                2 if valid_name(&w[1].0) => {
                    p.name = w[1].0.clone();
                    named = true;
                }
                2 => return Err(err(ln, w[1].1, format!("invalid plot name '{}'", w[1].0))),
                1 => return Err(err(ln, end_col, "expected a plot name after 'plot'".into())),
                _ => return Err(err(ln, w[2].1, "a plot name with spaces is quoted".into())),
            },
            "plot" => return Err(err(ln, w[0].1, "a file holds one plot: second 'plot' line".into())),
            "names" => {
                let v = number(1)?;
                if v <= 0.0 || w.len() > 2 {
                    return Err(err(ln, col(1), "expected 'names WIDTH' (points, more than 0)".into()));
                }
                p.names_width = Some(v as f32);
            }
            "bookmarks" => {
                if w.len() != 2 || w[1].0 != "hidden" {
                    return Err(err(ln, col(1), "expected 'bookmarks hidden'".into()));
                }
                p.hide_bookmarks = true;
            }
            "lane" => {
                let Some((signal, _)) = w.get(1) else {
                    return Err(err(ln, end_col, "expected a signal name after 'lane'".into()));
                };
                if p.lanes.iter().any(|l| l.signal == *signal) {
                    return Err(err(ln, w[1].1, format!("'{signal}' has a lane already")));
                }
                let mut lane = Lane { signal: signal.clone(), ..Default::default() };
                let mut i = 2;
                if w.get(i).is_some_and(|x| x.0 == "events") {
                    lane.events = true;
                    i += 1;
                }
                if w.get(i).is_some_and(|x| x.0 == "y") {
                    lane.y = match w.get(i + 1).map(|x| x.0.as_str()) {
                        Some("all") => YAxis::All,
                        Some("visible") => YAxis::Visible,
                        _ => {
                            let (lo, hi) = (number(i + 1)?, number(i + 2)?);
                            if lo >= hi {
                                return Err(err(ln, col(i + 2), "the y axis goes up: 'y LO HI' with LO < HI".into()));
                            }
                            i += 1;
                            YAxis::Fixed(lo, hi)
                        }
                    };
                    i += 2;
                }
                if i < w.len() {
                    return Err(err(ln, w[i].1, format!("expected 'events' or 'y' but found '{}'", w[i].0)));
                }
                p.lanes.push(lane);
            }
            "marker" => {
                let Some((signal, scol)) = w.get(1) else {
                    return Err(err(ln, end_col, "expected 'marker SIGNAL VALUE [LABEL]'".into()));
                };
                let value = number(2)?;
                if w.len() > 4 {
                    return Err(err(ln, w[4].1, "a label with spaces is quoted".into()));
                }
                let label = w.get(3).map(|x| x.0.clone()).unwrap_or_default();
                let Some(lane) = p.lanes.iter_mut().find(|l| l.signal == *signal) else {
                    return Err(err(ln, *scol, format!("no lane '{signal}' (a marker comes after its lane)")));
                };
                lane.markers.push(Marker { value, label });
            }
            w0 => {
                return Err(err(ln, w[0].1, format!("expected lane, marker, names or bookmarks but found '{w0}'")));
            }
        }
    }
    if !named {
        return Err(PackError { file: file.to_owned(), pos: None, msg: "no 'plot NAME' line".into() });
    }
    Ok(p)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> PlotLayout {
        PlotLayout {
            name: "engine speeds".into(),
            lanes: vec![
                Lane { signal: "speed".into(), events: true, ..Default::default() },
                Lane { signal: "rpm".into(), y: YAxis::Visible, ..Default::default() },
                Lane {
                    signal: "temp".into(),
                    y: YAxis::Fixed(-40.0, 120.5),
                    markers: vec![
                        Marker { value: 100.0, label: "too \"hot\"".into() },
                        Marker { value: 0.25, label: String::new() },
                    ],
                    ..Default::default()
                },
            ],
            names_width: Some(150.0),
            hide_bookmarks: true,
        }
    }

    #[test]
    fn round_trips() {
        let p = sample();
        let text = p.serialize();
        assert_eq!(
            text,
            "plot \"engine speeds\"\nnames 150\nbookmarks hidden\nlane speed events\nlane rpm y visible\n\
             lane temp y -40 120.5\nmarker temp 100 \"too \\\"hot\\\"\"\nmarker temp 0.25\n"
        );
        assert_eq!(parse_plot("t", &text).unwrap(), p);
        let bare = PlotLayout { name: "p".into(), ..Default::default() };
        assert_eq!(parse_plot("t", &bare.serialize()).unwrap(), bare);
    }

    fn bad(text: &str, line: usize, col: usize, msg: &str) {
        let e = parse_plot("t", text).unwrap_err();
        assert_eq!(e.pos, Some((line, col)), "{text:?}: {e}");
        assert!(e.msg.contains(msg), "{text:?}: {}", e.msg);
    }

    #[test]
    fn errors_have_positions() {
        bad("lane x\n", 0, 0, "expected 'plot NAME'");
        bad("plot\n", 0, 4, "expected a plot name");
        bad("plot a b\n", 0, 7, "quoted");
        bad("plot a\nplot b\n", 1, 0, "one plot");
        bad("plot a\nlane\n", 1, 4, "signal name");
        bad("plot a\nlane x\nlane x\n", 2, 5, "has a lane already");
        bad("plot a\nlane x y 5 1\n", 1, 11, "goes up");
        bad("plot a\nlane x y lo 1\n", 1, 9, "expected a number");
        bad("plot a\nlane x wide\n", 1, 7, "found 'wide'");
        bad("plot a\nmarker x 1\n", 1, 7, "no lane 'x'");
        bad("plot a\nlane x\nmarker x one\n", 2, 9, "expected a number");
        bad("plot a\nlane x\nmarker x 1 too hot\n", 2, 15, "quoted");
        bad("plot a\nnames -3\n", 1, 6, "more than 0");
        bad("plot a\nbookmarks shown\n", 1, 10, "bookmarks hidden");
        bad("plot a\nzoom 3\n", 1, 0, "found 'zoom'");
        assert!(parse_plot("t", "# only\n").unwrap_err().msg.contains("no 'plot NAME'"));
    }

    #[test]
    fn files_save_and_load() {
        let dir = std::env::temp_dir().join(format!("nerka-plots-{}", std::process::id()));
        let p = sample();
        let path = plot_path(&dir, &p.name).unwrap();
        p.save(&path).unwrap();
        std::fs::write(dir.join("broken.nerka-plot"), "lane x\n").unwrap();
        let (ok, bad) = load_dir(&dir);
        assert_eq!(ok, vec![(path, p)]);
        assert_eq!(bad.len(), 1);
        assert!(plot_path(&dir, "a/b").is_err());
        std::fs::remove_dir_all(&dir).ok();
    }
}
