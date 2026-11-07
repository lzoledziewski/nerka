//! The signal plot: one lane per plotted signal on the tab's time axis
//! (`doc/sources-and-signals.md` → "What a signal is"). A number is drawn
//! as its state, a step line; an enum as coloured bands. Each lane can show
//! its events (when the value was received). The cursor is the current
//! line. Wheel zooms, drag pans, double-click shows everything, a click
//! goes to the nearest event; right-clicking a lane's name sets its y axis
//! and its markers (horizontal lines at chosen values: limits, thresholds).
//! Time cursors (owner: like a logic analyzer) are vertical lines placed
//! with a right-click in the plot and dragged; the time between neighbours
//! is shown (Δt). Bookmarks are cursors too (owner): a line at their line's
//! time with their note, measured like the others; the plot's right-click
//! menu bookmarks the nearest event's line with a note.
//! The plot can move to a window of its own and back, and be closed (its
//! lanes are kept for when it is shown again). A plot has no name until it
//! is kept by one (Keep: this session, Save: on disk), so any tab can switch
//! to it (`nerka_core::plot`, `crate::plots`); New plot starts a blank one.
//! A lane whose signal the tab doesn't have stays, named above the lanes.
//!
//! Drawn with egui's painter (no plot crate): lanes are decimated to one
//! step per pixel column, so millions of events draw as fast as a few.

use eframe::egui::{self, Align2, Color32, ColorImage, FontId, Pos2, Rect, Sense, Stroke, pos2, vec2};
use nerka_core::plot::{Lane as LaneLayout, PlotLayout};
pub use nerka_core::plot::{Marker, YAxis};
use nerka_core::{Clock, LineNo, LogSource, SignalDef, SignalKind, SignalSeries, Signals, ValueRef, time::format_time};
use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};

/// A tab's plot state, shared with the plot's own window (which paints in a
/// pass of its own).
pub type SharedPlot = Arc<Mutex<PlotState>>;

/// The plot state, also after a panic while it was held.
pub fn lock(p: &SharedPlot) -> MutexGuard<'_, PlotState> {
    p.lock().unwrap_or_else(|e| e.into_inner())
}

/// What the plot's own window draws, set by the main window every frame,
/// and what it asks back (read by the main window's next frame).
///
/// The window is a *deferred* egui viewport: it paints in its own pass.
/// An immediate one (painted inside the main window's frame, switching the
/// shared OpenGL context back and forth) crashed NVIDIA's OpenGL driver
/// when it was maximized (owner, 2026-10-24).
#[derive(Default)]
pub struct Detached {
    pub input: Option<WindowInput>,
    /// The window's own cache of filtered events.
    views: crate::signals::Views,
    pub out: Output,
    /// The window was closed (its ✖).
    pub closed: bool,
}

pub struct WindowInput {
    pub plot: SharedPlot,
    pub signals: Signals,
    pub generation: u64,
    pub file: Arc<dyn LogSource>,
    pub cursor: LineNo,
    pub kept: Option<Arc<nerka_core::SearchResult>>,
    pub bookmarks: Vec<Bookmark>,
    pub saved: Vec<SavedPlot>,
}

/// What the plot's own window shows of its input, to compare between the
/// main window's frames: the window repaints only when it changed. Every
/// frame of a window costs all its pixels, and on a CPU renderer (Mesa's
/// llvmpipe) hovering the main window repainted the plot too, doubling the
/// cost (owner's measurements, 2026-11-06). The window repaints itself for
/// its own input.
#[derive(PartialEq)]
pub struct WindowKey {
    plot: usize,
    layout: PlotLayout,
    name: Option<String>,
    cursors: Vec<f64>,
    range: Option<(f64, f64)>,
    built: bool,
    generation: u64,
    file: (usize, LineNo),
    cursor: LineNo,
    kept: usize,
    bookmarks: Vec<Bookmark>,
    saved: Vec<SavedPlot>,
}

impl WindowInput {
    pub fn key(&self) -> WindowKey {
        let p = lock(&self.plot);
        WindowKey {
            plot: Arc::as_ptr(&self.plot) as usize,
            layout: p.layout(""),
            name: p.name.clone(),
            cursors: p.cursors.clone(),
            range: p.range,
            built: p.data.is_some(),
            generation: self.generation,
            file: (Arc::as_ptr(&self.file) as *const () as usize, self.file.line_count()),
            cursor: self.cursor,
            kept: self.kept.as_ref().map_or(0, |r| Arc::as_ptr(r) as usize),
            bookmarks: self.bookmarks.clone(),
            saved: self.saved.clone(),
        }
    }
}

/// A kept plot, as the plot's list shows it.
#[derive(Debug, Clone, PartialEq)]
pub struct SavedPlot {
    pub name: String,
    /// Kept for this session only (not on disk).
    pub session: bool,
    /// The tab's plot is this one, changed since it was saved or kept.
    pub changed: bool,
}

/// What the plot's list and menu ask for.
#[derive(Debug, Clone, PartialEq)]
pub enum PlotAction {
    /// Switch to the kept plot of this name (the current one is kept as it
    /// is first).
    Use(String),
    /// Keep this plot by a name; `store` writes it to disk too.
    Keep { name: String, store: bool },
    /// A blank plot instead: no lanes, no name (a kept one is kept as it
    /// is first).
    Blank,
    /// Write the current plot to its kept entry (and its file).
    Save,
    /// Delete the kept plot of this name (and its file).
    Delete(String),
}

/// A bookmark of the tab, as the plot shows it.
#[derive(Debug, Clone, PartialEq)]
pub struct Bookmark {
    pub line: LineNo,
    pub name: String,
    pub note: String,
}

impl Bookmark {
    /// The tab's bookmarks, in line order.
    pub fn all(marks: &nerka_core::Bookmarks) -> Vec<Self> {
        marks
            .iter_named()
            .map(|(line, name, note)| Self { line, name: name.to_owned(), note: note.to_owned() })
            .collect()
    }
}

/// Paint the plot's own window (the deferred viewport's callback).
pub fn window_ui(ui: &mut egui::Ui, shared: &Mutex<Detached>) {
    let mut d = shared.lock().unwrap_or_else(|e| e.into_inner());
    let Detached { input, views, out, closed } = &mut *d;
    let Some(i) = input else { return };
    let mut series_of = |def: &SignalDef| views.series(def, &*i.file, i.kept.as_ref(), i.generation);
    let src = Source {
        signals: &i.signals,
        generation: i.generation,
        file: &*i.file,
        cursor: i.cursor,
        kept: i.kept.as_ref().map_or(0, |r| Arc::as_ptr(r) as usize),
        series_of: &mut series_of,
        in_window: true,
        bookmarks: &i.bookmarks,
        saved: &i.saved,
    };
    let mut plot = lock(&i.plot);
    let o = egui::CentralPanel::default().show(ui, |ui| plot_panel(ui, &mut plot, src)).inner;
    drop(plot);
    out.goto = o.goto.or(out.goto);
    out.toggle_window |= o.toggle_window;
    out.bookmark = o.bookmark.or(out.bookmark.take());
    out.plots = o.plots.or(out.plots.take());
    out.close |= o.close;
    *closed |= ui.input(|i| i.viewport().close_requested());
    let asked = out.goto.is_some() || out.toggle_window || out.bookmark.is_some() || out.plots.is_some();
    if asked || out.close || *closed {
        ui.ctx().request_repaint_of(egui::ViewportId::ROOT);
    }
}

/// Which signals are plotted and how, per tab.
#[derive(Default)]
pub struct PlotState {
    /// The plot is shown (closed: its lanes are kept).
    pub shown: bool,
    /// The kept plot this is (`crate::plots`), if any.
    pub name: Option<String>,
    /// A name being typed to keep this plot by.
    naming: Option<String>,
    /// Delete was clicked once for this kept plot.
    confirm_delete: Option<String>,
    /// Plotted signals, in lane order.
    pub plotted: Vec<String>,
    /// Each plotted signal's colour (a palette slot, given when first
    /// drawn), so moving a lane keeps its colour.
    colors: HashMap<String, usize>,
    /// Width of the lane names' column; 0: the default (`GUTTER`).
    gutter: f32,
    /// A lane being dragged by its name to a new place, and where it goes
    /// (applied at the next frame: `(from, to)` in lane order).
    drag_lane: Option<usize>,
    reorder: Option<(usize, usize)>,
    /// A picture of the whole plot being taken (all lanes, scrolling).
    capture: Option<Capture>,
    /// Lanes that show their events.
    pub events: std::collections::HashSet<String>,
    /// Visible x range; none: everything.
    range: Option<(f64, f64)>,
    /// Lanes' y axes (absent: [`YAxis::All`]).
    pub y: HashMap<String, YAxis>,
    /// Lanes' markers: horizontal lines at chosen values.
    pub markers: HashMap<String, Vec<Marker>>,
    /// Time cursors (A, B, C… in the order placed), x values.
    pub cursors: Vec<f64>,
    /// The cursor being dragged, and where the context menu was opened.
    drag_cursor: Option<usize>,
    menu_x: Option<f64>,
    /// The event nearest to where the menu was opened: its line and lane.
    menu_row: Option<(LineNo, String)>,
    /// The note typed in the menu for a new bookmark.
    bookmark_note: String,
    /// Bookmarks aren't drawn.
    pub hide_bookmarks: bool,
    /// How far the lanes are scrolled down, in points (more lanes than fit).
    scroll: f32,
    /// The scrollbar is being dragged.
    drag_scroll: bool,
    /// Lanes, built for (signals generation, the filter result).
    data: Option<((u64, usize), PlotData)>,
}

impl PlotState {
    pub fn is_plotted(&self, name: &str) -> bool {
        self.plotted.iter().any(|n| n == name)
    }

    pub fn toggle(&mut self, name: &str) {
        match self.plotted.iter().position(|n| n == name) {
            Some(i) => {
                self.plotted.remove(i);
            }
            None => {
                self.plotted.push(name.to_owned());
                self.shown = true;
            }
        }
        self.data = None;
    }

    /// The lanes and their settings, to keep by `name`.
    pub fn layout(&self, name: &str) -> PlotLayout {
        let lanes = self
            .plotted
            .iter()
            .map(|n| LaneLayout {
                signal: n.clone(),
                events: self.events.contains(n),
                y: self.y.get(n).copied().unwrap_or_default(),
                markers: self.markers.get(n).cloned().unwrap_or_default(),
            })
            .collect();
        PlotLayout {
            name: name.to_owned(),
            lanes,
            names_width: (self.gutter > 0.0).then_some(self.gutter),
            hide_bookmarks: self.hide_bookmarks,
        }
    }

    /// Show `p` instead: its lanes and their settings. The time cursors and
    /// the zoom stay (they belong to the file).
    pub fn apply(&mut self, p: &PlotLayout) {
        self.plotted = p.lanes.iter().map(|l| l.signal.clone()).collect();
        self.events = p.lanes.iter().filter(|l| l.events).map(|l| l.signal.clone()).collect();
        self.y = p.lanes.iter().filter(|l| l.y != YAxis::All).map(|l| (l.signal.clone(), l.y)).collect();
        self.markers =
            p.lanes.iter().filter(|l| !l.markers.is_empty()).map(|l| (l.signal.clone(), l.markers.clone())).collect();
        self.gutter = p.names_width.unwrap_or(0.0);
        self.hide_bookmarks = p.hide_bookmarks;
        self.name = Some(p.name.clone());
        self.shown = true;
        self.scroll = 0.0;
        self.data = None;
    }

    /// A blank plot: no lanes and no name (the time cursors and the zoom
    /// stay: they belong to the file).
    pub fn blank(&mut self) {
        self.apply(&PlotLayout::default());
        self.name = None;
    }
}

struct PlotData {
    lanes: Vec<Lane>,
    /// x range of all events.
    full: (f64, f64),
    is_time: bool,
}

struct Lane {
    /// What it shows: the signal's name, with the key for a signal bound to
    /// a lifecycle of several keys (`ota A`).
    name: String,
    /// The signal it is of (several lanes for one bound per key).
    signal: String,
    kind: SignalKind,
    /// Events by x (ties keep row order): x, y (value or enum id), row.
    /// For a signal bound to a lifecycle, also a point where each lifecycle
    /// starts: its default (`assumed`), or NaN (unset).
    xs: Vec<f64>,
    ys: Vec<f64>,
    rows: Vec<LineNo>,
    assumed: Vec<bool>,
    /// Enum ids' texts.
    labels: Vec<String>,
    y_range: (f64, f64),
}

/// The y range a lane shows: its values' range `(lo, hi)` widened to its
/// markers (fitted axes only; a fixed axis is what it says).
fn with_markers((lo, hi): (f64, f64), markers: &[Marker]) -> (f64, f64) {
    markers.iter().fold((lo, hi), |(lo, hi), m| (lo.min(m.value), hi.max(m.value)))
}

/// What the plot asks the tab to do.
#[derive(Default)]
pub struct Output {
    pub goto: Option<LineNo>,
    /// Move the plot to its own window, or back.
    pub toggle_window: bool,
    /// Bookmark this line with this note (added, or its note replaced).
    pub bookmark: Option<(LineNo, String)>,
    /// The kept plots' list or menu was used.
    pub plots: Option<PlotAction>,
    /// The plot was closed (its ✖).
    pub close: bool,
}

const GUTTER: f32 = 120.0;
const GUTTER_MIN: f32 = 60.0;
/// A row of the strip above the lanes (tags, Δt).
const MARK_ROW: f32 = 17.0;
/// Rows the tags stack in, at most (more on one x share the last).
const MAX_TAG_ROWS: usize = 4;
/// Width of a character of the lane names (monospace 11), and room kept for
/// the y labels beside them.
const CHAR_W: f32 = 6.7;
const Y_LABELS_W: f32 = 36.0;
const AXIS_H: f32 = 18.0;
const LANE_MAX: f32 = 110.0;
/// Lanes are never lower than this (name, events and y labels fit); more
/// lanes than fit scroll.
const LANE_MIN: f32 = 52.0;
const SCROLLBAR_W: f32 = 6.0;

/// A lane's point: x, y, row, assumed (a default, not an event).
type Point = (f64, f64, LineNo, bool);

/// Build the lanes, or say why they can't be built yet.
fn build(
    state: &PlotState,
    signals: &Signals,
    file: &dyn LogSource,
    clock: &Clock,
    series_of: &mut dyn FnMut(&SignalDef) -> Option<Arc<SignalSeries>>,
) -> Result<PlotData, String> {
    let clock_series = match clock {
        Clock::Signal(d) => Some(d.cached_series(file).ok_or("Collecting the time signal's events…")?),
        _ => None,
    };
    let x_of = |row: LineNo| clock.at(file, clock_series.as_ref(), row);
    let mut lanes = Vec::new();
    // A plotted lifecycle is a line where each starts, no lane (owner,
    // 2026-11-07: lifecycles on and off in the plot): its span counts.
    let mut life_span = (f64::INFINITY, f64::NEG_INFINITY);
    for name in &state.plotted {
        let Some(def) = signals.get(name) else { continue };
        if let Some(life) = def.as_lifecycle() {
            let lives = life.cached(file).ok_or("Finding lifecycles…")?;
            if let Some(first) = lives.lives.first() {
                let last = file.line_count().saturating_sub(1);
                for x in [x_of(first.first), x_of(last)].into_iter().flatten() {
                    life_span = (life_span.0.min(x), life_span.1.max(x));
                }
            }
            continue;
        }
        let series = series_of(def).ok_or("Collecting events…")?;
        let mut ids: HashMap<Vec<u8>, usize> = HashMap::new();
        let mut labels = Vec::new();
        let mut y_of = |v: ValueRef<'_>| match v {
            ValueRef::Num(n) => n,
            ValueRef::Enum(t) => *ids.entry(t.to_vec()).or_insert_with(|| {
                labels.push(String::from_utf8_lossy(t).into_owned());
                labels.len() - 1
            }) as f64,
        };
        // Points: (x, y, row, assumed); for a signal bound to a lifecycle
        // also where each starts (its state resets there to the default).
        let mut groups: Vec<(Option<String>, Vec<Point>)> = Vec::new();
        let mut pts: Vec<Point> = Vec::new();
        let mut starts: Vec<LineNo> = match series.lifecycles() {
            Some(life) => life.lives.iter().rev().map(|l| l.first).collect(),
            None => Vec::new(),
        };
        let reset = def.default_value().map_or(f64::NAN, |d| y_of(d.as_ref()));
        for (row, v) in series.iter() {
            while starts.last().is_some_and(|&s| s <= row) {
                let s = starts.pop().expect("checked");
                if s < row
                    && let Some(x) = x_of(s)
                {
                    pts.push((x, reset, s, true));
                }
            }
            if let Some(x) = x_of(row) {
                pts.push((x, y_of(v), row, false));
            }
        }
        for s in starts.into_iter().rev() {
            if let Some(x) = x_of(s) {
                pts.push((x, reset, s, true));
            }
        }
        groups.push((None, pts));
        for (key, mut pts) in groups {
            // Clocks may step back (a logger's time jumps): keep x sorted.
            if pts.windows(2).any(|w| w[1].0 < w[0].0) {
                pts.sort_by(|a, b| a.0.total_cmp(&b.0));
            }
            let (lo, hi) = pts
                .iter()
                .filter(|p| p.1.is_finite())
                .fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), p| (lo.min(p.1), hi.max(p.1)));
            lanes.push(Lane {
                name: match key {
                    Some(k) => format!("{name} {k}"),
                    None => name.clone(),
                },
                signal: name.clone(),
                kind: def.kind(),
                xs: pts.iter().map(|p| p.0).collect(),
                ys: pts.iter().map(|p| p.1).collect(),
                rows: pts.iter().map(|p| p.2).collect(),
                assumed: pts.iter().map(|p| p.3).collect(),
                labels: labels.clone(),
                y_range: if lo <= hi { (lo, hi) } else { (0.0, 1.0) },
            });
        }
    }
    let full = lanes
        .iter()
        .filter(|l| !l.xs.is_empty())
        .fold(life_span, |(lo, hi), l| (lo.min(l.xs[0]), hi.max(l.xs[l.xs.len() - 1])));
    let full = if full.0 < full.1 {
        full
    } else if full.0.is_finite() {
        (full.0 - 1.0, full.0 + 1.0)
    } else {
        (0.0, 1.0)
    };
    Ok(PlotData { lanes, full, is_time: clock.is_time() })
}

/// What a plot is drawn from: the tab's signals (`generation` changes
/// whenever they do), the file, the current line, the filter result
/// (`kept`, an identity: filtered signals' events change with it) and
/// each signal's events as shown.
pub struct Source<'a> {
    pub signals: &'a Signals,
    pub generation: u64,
    pub file: &'a dyn LogSource,
    pub cursor: LineNo,
    pub kept: usize,
    pub series_of: &'a mut dyn FnMut(&SignalDef) -> Option<Arc<SignalSeries>>,
    /// Drawn in a window of its own.
    pub in_window: bool,
    /// The tab's bookmarks.
    pub bookmarks: &'a [Bookmark],
    /// The kept plots, to switch between.
    pub saved: &'a [SavedPlot],
}

pub fn plot_panel(ui: &mut egui::Ui, state: &mut PlotState, src: Source<'_>) -> Output {
    let Source { signals, generation, file, cursor, kept, series_of, in_window, bookmarks, saved } = src;
    let mut out = Output::default();
    if let Some((from, to)) = state.reorder.take() {
        let names: Vec<String> =
            state.data.as_ref().map(|(_, d)| d.lanes.iter().map(|l| l.signal.clone()).collect()).unwrap_or_default();
        move_lane(&mut state.plotted, &names, from, to);
        state.data = None;
    }
    let panel = ui.max_rect();
    ui.horizontal(|ui| {
        // ✖ first, at the right, so it stays in view however narrow.
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let tip = "Close the plot (its lanes are kept: 📈 Plot in the Signals panel shows it again)";
            out.close = crate::close_button(ui).on_hover_text(tip).clicked();
            ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                if let Some(a) = plots_list(ui, state, saved) {
                    out.plots = Some(a);
                }
                let label = if in_window { "⧉ Back into the main window" } else { "⧉ Own window" };
                out.toggle_window = ui.small_button(label).clicked();
                if ui.small_button("📷").on_hover_text("Screenshot of the plot to the clipboard").clicked() {
                    state.capture = Some(Capture { back_to: state.scroll, settle: 2, ..Default::default() });
                    state.scroll = 0.0;
                }
                let fit = ui.add_enabled(state.range.is_some(), egui::Button::new("↔ Fit").small());
                if fit.on_hover_text("Show the whole x range (also: double-click the plot)").clicked() {
                    state.range = None;
                }
                ui.weak(format!("x: {}", Clock::of(file, signals).describe())).on_hover_text(
                    "The file's own time, else the first signal of kind time that has events, else line numbers",
                );
                // The help is a hover, so the row never gets wider than the panel.
                let mut show = !state.hide_bookmarks;
                if ui.checkbox(&mut show, "🔖").on_hover_text("Show the bookmarks (with their notes) as cursors").changed() {
                    state.hide_bookmarks = !show;
                }
                ui.weak("ℹ").on_hover_text(
                    "Wheel: zoom (over the lane names: scroll). Drag: pan, up and down through the lanes (drag a time cursor to move it). Double-click: everything. Click: go to the \
                     nearest event. Right-click: place or remove time cursors (Δt between them). Right-click a lane's \
                     name: its y axis and markers.",
                );

            });
        });
    });
    if let Some(a) = naming_row(ui, state, saved) {
        out.plots = Some(a);
    }
    if state.plotted.is_empty() {
        ui.weak("Nothing plotted yet: tick a signal in the Signals panel (📈 Signals).");
        return out;
    }
    // Lanes whose signal this tab doesn't have (a plot kept in another tab):
    // they stay, so they are drawn once a signal of that name is, and are
    // named here.
    let missing: Vec<String> = state.plotted.iter().filter(|n| signals.get(n).is_none()).cloned().collect();
    if !missing.is_empty() {
        ui.horizontal_wrapped(|ui| {
            ui.weak("Not in this tab:").on_hover_text(
                "These lanes' signals aren't defined here (another tab's selection, or a FIBEX argument of \
                 files not loaded). They are drawn once a signal of that name is.",
            );
            for n in &missing {
                if ui.small_button(format!("@{n} ✖")).on_hover_text("Take the lane out of this plot").clicked() {
                    state.toggle(n);
                }
            }
        });
    }
    let clock = Clock::of(file, signals);
    let clock_series = match &clock {
        Clock::Signal(d) => d.cached_series(file),
        _ => None,
    };
    // Bookmarks as cursors: their line's x, name and note.
    let marks: Vec<BookmarkCursor> = if state.hide_bookmarks {
        Vec::new()
    } else {
        bookmarks
            .iter()
            .filter_map(|b| {
                Some(BookmarkCursor {
                    line: b.line,
                    name: b.name.clone(),
                    note: b.note.clone(),
                    x: clock.at(file, clock_series.as_ref(), b.line)?,
                })
            })
            .collect()
    };
    if !state.cursors.is_empty() || !marks.is_empty() {
        ui.horizontal_wrapped(|ui| ui.monospace(cursor_summary(&state.cursors, &marks, clock.is_time())));
    }
    let key = (generation, kept);
    if state.data.as_ref().is_none_or(|(k, _)| *k != key) {
        match build(state, signals, file, &clock, series_of) {
            Ok(d) => state.data = Some((key, d)),
            Err(why) => {
                state.data = None;
                ui.weak(why);
                ui.ctx().request_repaint_after(std::time::Duration::from_millis(100));
                return out;
            }
        }
    }
    let Some((_, data)) = &state.data else { return out };
    if !data.is_time {
        ui.weak("No time in this file: plotted by line number. Define a signal of kind time to plot by time.");
    }

    let rect = ui.available_rect_before_wrap();
    let response = ui.allocate_rect(rect, Sense::click_and_drag());
    let gutter = if state.gutter > 0.0 { state.gutter } else { GUTTER }.min((rect.width() / 2.0).max(GUTTER_MIN));
    // The strip for the cursors' and bookmarks' tags (rows: tags at the same
    // or nearly the same x stack) and the Δt between them (a row below),
    // above the lanes. The rows are counted in the view before this frame's
    // zoom or pan; a change shows on the next frame.
    let tagged = state.cursors.len() + marks.len();
    let tag_rows = if tagged == 0 {
        0
    } else {
        let (left, right) = (rect.min.x + gutter, rect.max.x - 8.0);
        let (x0, x1) = state.range.unwrap_or(data.full);
        let tags = tags_in_view(&state.cursors, &marks, (x0, x1), |x| {
            left + ((x - x0) / (x1 - x0) * (right - left) as f64) as f32
        });
        tag_rows_of(ui, &tags, MAX_TAG_ROWS).iter().max().map_or(1, |r| r + 1)
    };
    let strip_h = MARK_ROW * (tag_rows + usize::from(tagged > 1)) as f32;
    let plot = Rect::from_min_max(
        pos2(rect.min.x + gutter, rect.min.y + strip_h),
        pos2(rect.max.x - 8.0, rect.max.y - AXIS_H),
    );
    let strip = Rect::from_min_max(pos2(plot.min.x, rect.min.y), pos2(plot.max.x, plot.min.y));
    if plot.width() < 20.0 || plot.height() < 10.0 {
        return out;
    }
    // The names' column: drag its edge to widen it, double-click to fit the
    // longest name (owner, 2026-10-25).
    let edge = Rect::from_min_max(pos2(plot.min.x - 3.0, plot.min.y), pos2(plot.min.x + 3.0, plot.max.y));
    let edge_r = ui.interact(edge, ui.scope_id().with("names-edge"), Sense::click_and_drag());
    if edge_r.hovered() || edge_r.dragged() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeColumn);
    }
    if edge_r.dragged() {
        state.gutter = (gutter + edge_r.drag_delta().x).clamp(GUTTER_MIN, (rect.width() / 2.0).max(GUTTER_MIN));
    }
    if edge_r.double_clicked() {
        let longest = data.lanes.iter().map(|l| l.name.chars().count() + 1).max().unwrap_or(0);
        state.gutter =
            (longest as f32 * CHAR_W + Y_LABELS_W + 8.0).clamp(GUTTER_MIN, (rect.width() / 2.0).max(GUTTER_MIN));
    }
    edge_r.on_hover_text("Drag: widen the names. Double-click: fit the longest name.");
    // Lanes share the height, between LANE_MIN and LANE_MAX; more than fit
    // scroll (wheel over the names, or the scrollbar).
    let n = data.lanes.len().max(1) as f32;
    let lane_h = (plot.height() / n).clamp(LANE_MIN, LANE_MAX);
    let max_scroll = (n * lane_h - plot.height()).max(0.0);
    let bar = Rect::from_min_max(pos2(rect.max.x - SCROLLBAR_W - 1.0, plot.min.y), pos2(rect.max.x - 1.0, plot.max.y));
    let thumb = (max_scroll > 0.0).then(|| {
        let h = (plot.height() * plot.height() / (n * lane_h)).max(20.0);
        let top = bar.min.y + (bar.height() - h) * state.scroll.min(max_scroll) / max_scroll;
        Rect::from_min_size(pos2(bar.min.x, top), vec2(SCROLLBAR_W, h))
    });
    if let Some(thumb) = thumb {
        let r = ui.interact(bar, ui.scope_id().with("lanes-scrollbar"), Sense::click_and_drag());
        if r.drag_started() {
            state.drag_scroll = true;
        }
        if r.dragged() && state.drag_scroll {
            state.scroll += r.drag_delta().y * max_scroll / (bar.height() - thumb.height()).max(1.0);
        } else if r.clicked()
            && let Some(p) = r.interact_pointer_pos()
        {
            // A page up or down.
            state.scroll += if p.y < thumb.min.y { -plot.height() } else { plot.height() };
        }
        if r.drag_stopped() {
            state.drag_scroll = false;
        }
    }
    state.scroll = state.scroll.clamp(0.0, max_scroll);
    // ----- a picture of all the lanes -----
    if let Some(c) = &mut state.capture {
        ui.ctx().request_repaint();
        let got = c.pending.lock().unwrap_or_else(|e| e.into_inner()).take();
        if let Some(part) = got {
            c.parts.push(part);
            c.scrolls.push(state.scroll);
            c.waiting = false;
            if state.scroll >= max_scroll - 0.5 {
                let (top, bottom) = (plot.min.y - panel.min.y, plot.max.y - panel.min.y);
                let ppp = ui.ctx().pixels_per_point();
                if let Some(all) = crate::shot::stitch(&c.parts, &c.scrolls, top, bottom, ppp) {
                    crate::shot::deliver(ui.ctx(), all);
                }
                state.scroll = c.back_to.min(max_scroll);
                state.capture = None;
            } else {
                state.scroll = (state.scroll + plot.height()).min(max_scroll);
                c.settle = 2;
            }
        } else if !c.waiting {
            if c.settle > 0 {
                c.settle -= 1;
            } else {
                crate::shot::request_part(ui.ctx(), panel, c.pending.clone());
                c.waiting = true;
            }
        }
    }
    let capturing = state.capture.is_some();
    let (mut x0, mut x1) = state.range.unwrap_or(data.full);
    let w = plot.width() as f64;
    let to_px = |x: f64, x0: f64, x1: f64| plot.min.x + ((x - x0) / (x1 - x0) * w) as f32;

    // ----- input -----
    if response.double_clicked() {
        state.range = None;
        (x0, x1) = data.full;
    }
    // A drag that starts on a time cursor moves it; elsewhere it pans.
    let cursor_px: Vec<f32> = state.cursors.iter().map(|&c| to_px(c, x0, x1)).collect();
    let near_cursor = |p: Pos2| {
        ((strip.min.y..=plot.max.y).contains(&p.y))
            .then(|| cursor_px.iter().position(|&c| (c - p.x).abs() <= 5.0))
            .flatten()
    };
    if response.hover_pos().and_then(near_cursor).is_some() || state.drag_cursor.is_some() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
    }
    if response.drag_started() {
        // Where the press began: egui starts a drag only after some movement.
        state.drag_cursor = ui.input(|i| i.pointer.press_origin()).and_then(near_cursor);
    }
    if response.dragged() {
        let dx = response.drag_delta().x as f64 * (x1 - x0) / w;
        match state.drag_cursor {
            Some(i) => state.cursors[i] += dx,
            None => {
                if dx != 0.0 {
                    (x0, x1) = (x0 - dx, x1 - dx);
                    state.range = Some((x0, x1));
                }
                // Up and down: through the lanes (owner, 2026-10-25).
                state.scroll = (state.scroll - response.drag_delta().y).clamp(0.0, max_scroll);
            }
        }
    }
    if response.drag_stopped() {
        state.drag_cursor = None;
    }
    // Right-click in the plot: the time cursors' menu, at that x.
    if response.secondary_clicked()
        && let Some(p) = response.interact_pointer_pos()
    {
        let mx = x0 + (p.x - plot.min.x).clamp(0.0, plot.width()) as f64 / w * (x1 - x0);
        state.menu_x = Some(mx);
        // The event nearest to it in any lane: what a bookmark goes on.
        state.menu_row = data
            .lanes
            .iter()
            .filter_map(|l| {
                let j = l.xs.partition_point(|&v| v < mx);
                let near = [j.checked_sub(1), (j < l.xs.len()).then_some(j)]
                    .into_iter()
                    .flatten()
                    .min_by(|&a, &b| (l.xs[a] - mx).abs().total_cmp(&(l.xs[b] - mx).abs()))?;
                Some(((l.xs[near] - mx).abs(), l.rows[near], l.name.clone()))
            })
            .min_by(|a, b| a.0.total_cmp(&b.0))
            .map(|(_, row, lane)| (row, lane));
        state.bookmark_note = state
            .menu_row
            .as_ref()
            .and_then(|(row, _)| bookmarks.iter().find(|b| b.line == *row))
            .map(|b| b.note.clone())
            .unwrap_or_default();
    }
    let (menu_x, menu_row) = (state.menu_x, state.menu_row.clone());
    let mut bookmark = None;
    response.context_menu_with_fields(|ui| {
        bookmark = cursor_menu(ui, &mut state.cursors, menu_x, menu_row, &mut state.bookmark_note);
    });
    out.bookmark = bookmark;
    if let Some(p) = ui.input(|i| i.pointer.hover_pos()).filter(|_| response.contains_pointer()) {
        let notches = ui.input_mut(|i| {
            i.smooth_scroll_delta = egui::Vec2::ZERO;
            i.events
                .iter()
                .filter_map(|e| match e {
                    egui::Event::MouseWheel { unit, delta, .. } => Some(match unit {
                        egui::MouseWheelUnit::Line | egui::MouseWheelUnit::Page => delta.y,
                        egui::MouseWheelUnit::Point => delta.y / 50.0,
                    }),
                    _ => None,
                })
                .sum::<f32>()
        });
        if notches != 0.0 && p.x < plot.min.x {
            state.scroll = (state.scroll - notches * lane_h / 2.0).clamp(0.0, max_scroll);
        } else if notches != 0.0 {
            let at = x0 + (p.x - plot.min.x).clamp(0.0, plot.width()) as f64 / w * (x1 - x0);
            let f = 0.8f64.powf(notches as f64);
            (x0, x1) = (at - (at - x0) * f, at + (x1 - at) * f);
            if x1 - x0 > 1e-9 {
                state.range = Some((x0, x1));
            }
        }
    }

    // ----- lanes -----
    let base = ui.painter_at(rect);
    let vis = ui.visuals();
    let text_color = vis.text_color();
    let weak = vis.weak_text_color();
    let grid = vis.widgets.noninteractive.bg_stroke.color;
    let font = FontId::monospace(11.0);
    let mut hovered_lane = None;
    // The lanes' own drawing stays above the axis (scrolled lanes are cut).
    let lanes_clip = Rect::from_min_max(pos2(rect.min.x, plot.min.y), pos2(rect.max.x, plot.max.y));
    let painter = base.with_clip_rect(lanes_clip);
    let mut drop_at = None;
    for (k, lane) in data.lanes.iter().enumerate() {
        let top = plot.min.y + k as f32 * lane_h - state.scroll;
        if top + lane_h < plot.min.y {
            continue;
        }
        if top > plot.max.y {
            break;
        }
        // Widgets only where they're in view.
        let shown = |r: Rect| r.min.y >= plot.min.y - 0.5 && r.max.y <= plot.max.y + 0.5;
        let area = Rect::from_min_max(pos2(plot.min.x, top + 3.0), pos2(plot.max.x, top + lane_h - 3.0));
        let next = state.colors.len();
        let color = crate::library::palette(*state.colors.entry(lane.name.clone()).or_insert(next));
        painter.line_segment([pos2(rect.min.x, top + lane_h), pos2(plot.max.x, top + lane_h)], Stroke::new(1.0, grid));
        // The lane's name, and a checkbox for its events, in the gutter.
        // Long names are shortened to fit beside the y labels (hover: all).
        let name = format!("@{}", lane.name);
        let fit = (((gutter - Y_LABELS_W - 8.0) / CHAR_W) as usize).max(4);
        let short = name_tail(&name, fit);
        painter.text(pos2(rect.min.x + 4.0, top + 4.0), Align2::LEFT_TOP, short, font.clone(), color);
        // Right-click the name: the y axis.
        let name_rect = Rect::from_min_size(pos2(rect.min.x, top), vec2(gutter - 4.0, 18.0));
        // Clipped to the lanes, so a lane scrolled half out of view doesn't
        // take the pointer from the rows above. Drag the name: move the lane.
        let name_r = ui.interact(
            name_rect.intersect(lanes_clip),
            ui.scope_id().with(("lane", &lane.name)),
            Sense::click_and_drag(),
        );
        if name_r.drag_started() {
            state.drag_lane = Some(k);
        }
        if name_r.dragged() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
        } else if name_r.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::Grab);
        }
        if name_r.drag_stopped()
            && let (Some(from), Some(p)) = (state.drag_lane.take(), ui.ctx().pointer_latest_pos())
        {
            let to = lane_at(p.y, plot.min.y, lane_h, state.scroll, data.lanes.len());
            if to != from {
                state.reorder = Some((from, to));
            }
        }
        let last = data.lanes.len() - 1;
        let mut moved = None;
        let move_menu = |ui: &mut egui::Ui, moved: &mut Option<(usize, usize)>| {
            ui.label(egui::RichText::new("Lane").strong());
            for (label, to, ok) in [
                ("↑ Move up", k.saturating_sub(1), k > 0),
                ("↓ Move down", k + 1, k < last),
                ("To the top", 0, k > 0),
                ("To the bottom", last, k < last),
            ] {
                if ui.add_enabled(ok, egui::Button::new(label)).clicked() {
                    *moved = Some((k, to));
                    ui.close();
                }
            }
        };
        if lane.kind != SignalKind::Enum {
            let y = state.y.entry(lane.name.clone()).or_insert(YAxis::All);
            let markers = state.markers.entry(lane.name.clone()).or_default();
            name_r
                .on_hover_text(format!("{name}\nDrag: move the lane. Right-click: y axis, markers, order"))
                .context_menu_with_fields(|ui| {
                    y_axis_menu(ui, y, lane.y_range);
                    ui.separator();
                    markers_menu(ui, markers, lane.y_range);
                    ui.separator();
                    move_menu(ui, &mut moved);
                });
        } else {
            name_r
                .on_hover_text(format!("{name}\nDrag: move the lane. Right-click: order"))
                .context_menu(|ui| move_menu(ui, &mut moved));
        }
        if moved.is_some() {
            state.reorder = moved;
        }
        let cb = Rect::from_min_size(pos2(rect.min.x + 4.0, top + 20.0), vec2(gutter - 10.0, 16.0));
        let mut show = state.events.contains(&lane.name);
        if shown(cb)
            && ui
                .put(cb, egui::Checkbox::new(&mut show, egui::RichText::new("events").small()))
                .on_hover_text("Mark the events: the lines where the value was received")
                .changed()
        {
            if show {
                state.events.insert(lane.name.clone());
            } else {
                state.events.remove(&lane.name);
            }
        }
        if response.hover_pos().is_some_and(|p| p.y >= top.max(plot.min.y) && p.y < top + lane_h) {
            hovered_lane = Some((k, area));
        }
        if lane.xs.is_empty() {
            painter.text(area.left_center(), Align2::LEFT_CENTER, "  no events", font.clone(), weak);
            continue;
        }
        let (i0, i1) = (lane.xs.partition_point(|&x| x < x0), lane.xs.partition_point(|&x| x <= x1));
        // The state at the left edge is the last event before it.
        let from = i0.saturating_sub(1);
        match lane.kind {
            SignalKind::Enum => draw_bands(&painter, lane, area, from, i1, |x| to_px(x, x0, x1), &font, weak),
            _ => {
                let markers = state.markers.get(&lane.name).map_or(&[][..], Vec::as_slice);
                let (lo, hi) = match state.y.get(&lane.name).copied().unwrap_or(YAxis::All) {
                    YAxis::All => with_markers(lane.y_range, markers),
                    YAxis::Visible => with_markers(
                        lane.ys[from..i1.max(from + 1).min(lane.ys.len())]
                            .iter()
                            .fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), &v| (lo.min(v), hi.max(v))),
                        markers,
                    ),
                    YAxis::Fixed(lo, hi) => (lo, hi),
                };
                // Labels show the range; a little room around the values.
                let (lo_l, hi_l) = (lo, hi);
                let pad = if hi > lo { (hi - lo) * 0.08 } else { 1.0 };
                let (lo, hi) = (lo - pad, hi + pad);
                let to_py =
                    |y: f64| (area.max.y - ((y - lo) / (hi - lo)) as f32 * area.height()).clamp(area.min.y, area.max.y);
                draw_steps(&painter, lane, area, from..i1, |x| to_px(x, x0, x1), to_py, color);
                // Markers: dashed lines, the label and value at the right.
                for m in markers.iter().filter(|m| (lo..=hi).contains(&m.value)) {
                    let y = to_py(m.value);
                    let stroke = Stroke::new(1.0, text_color.gamma_multiply(0.7));
                    painter.extend(egui::Shape::dashed_line(
                        &[pos2(area.min.x, y), pos2(area.max.x, y)],
                        stroke,
                        6.0,
                        4.0,
                    ));
                    let text = if m.label.is_empty() {
                        short_num(m.value)
                    } else {
                        format!("{} {}", m.label, short_num(m.value))
                    };
                    painter.text(pos2(area.max.x - 4.0, y - 1.0), Align2::RIGHT_BOTTOM, text, font.clone(), text_color);
                }
                let fmt = |v: f64| short_num(v);
                painter.text(pos2(plot.min.x - 4.0, area.min.y), Align2::RIGHT_TOP, fmt(hi_l), font.clone(), weak);
                painter.text(pos2(plot.min.x - 4.0, area.max.y), Align2::RIGHT_BOTTOM, fmt(lo_l), font.clone(), weak);
            }
        }
        if show {
            draw_ticks(&painter, lane, area, i0, i1, |x| to_px(x, x0, x1), color);
        }
    }
    // Where a dragged lane would go: a line between lanes.
    if let (Some(from), Some(p)) = (state.drag_lane, ui.ctx().pointer_latest_pos()) {
        let to = lane_at(p.y, plot.min.y, lane_h, state.scroll, data.lanes.len());
        drop_at =
            (to != from).then(|| plot.min.y + (if to > from { to + 1 } else { to }) as f32 * lane_h - state.scroll);
    }
    if let Some(y) = drop_at {
        painter.line_segment([pos2(rect.min.x, y), pos2(plot.max.x, y)], Stroke::new(2.5, text_color));
    }

    // Axis, cursors and tags: not clipped to the lanes.
    let painter = base;
    if let Some(thumb) = thumb.filter(|_| !capturing) {
        painter.rect_filled(bar, 3.0, grid.gamma_multiply(0.4));
        let c = if state.drag_scroll { text_color } else { weak };
        painter.rect_filled(thumb, 3.0, c.gamma_multiply(0.8));
    }

    // ----- axis -----
    let axis_y = plot.max.y;
    painter.line_segment([pos2(plot.min.x, axis_y), pos2(plot.max.x, axis_y)], Stroke::new(1.0, grid));
    let step = nice_step((x1 - x0) / (w / 110.0).max(1.0), data.is_time);
    // Round in the time shown (local time's hours and days).
    let shift = if data.is_time { nerka_core::time::shown(x0) - x0 } else { 0.0 };
    let mut t = ((x0 + shift) / step).ceil() * step - shift;
    while t <= x1 {
        let px = to_px(t, x0, x1);
        painter.line_segment([pos2(px, plot.min.y), pos2(px, axis_y)], Stroke::new(1.0, grid.gamma_multiply(0.5)));
        painter.text(pos2(px, axis_y + 2.0), Align2::CENTER_TOP, x_label(t, step, data.is_time), font.clone(), weak);
        t += step;
    }

    // ----- time cursors and bookmarks: lines through the lanes; their tags
    // and the Δt between neighbours in a strip of their own above the lanes
    // (owner, 2026-10-25: not over the values) -----
    let amber = Color32::from_rgb(230, 150, 20);
    let red = Color32::from_rgb(220, 60, 60);
    let mut placed: Vec<(String, f64)> = state.cursors.iter().enumerate().map(|(i, &c)| (cursor_name(i), c)).collect();
    placed.extend(marks.iter().map(|m| (m.name(), m.x)));
    placed.sort_by(|a, b| a.1.total_cmp(&b.1));
    for &c in state.cursors.iter().filter(|c| (x0..=x1).contains(*c)) {
        let px = to_px(c, x0, x1);
        let line = [pos2(px, strip.min.y), pos2(px, axis_y)];
        painter.extend(egui::Shape::dashed_line(&line, Stroke::new(1.5, amber), 5.0, 3.0));
    }
    for m in marks.iter().filter(|m| (x0..=x1).contains(&m.x)) {
        let px = to_px(m.x, x0, x1);
        painter.line_segment([pos2(px, strip.min.y), pos2(px, axis_y)], Stroke::new(1.5, red));
    }
    // Tags, left to right, in rows: one that would hide the name of the one
    // before it goes a row down (owner, 2026-11-02: two bookmarks on one x
    // are two). Each is cut where the next one in its row starts (hover:
    // all of it).
    let tags = tags_in_view(&state.cursors, &marks, (x0, x1), |x| to_px(x, x0, x1));
    let mut rows = tag_rows_of(ui, &tags, MAX_TAG_ROWS);
    if tagged > 0 && rows.iter().max().map_or(1, |r| r + 1) != tag_rows {
        // The strip has another height from the next frame on.
        ui.ctx().request_repaint();
        rows.iter_mut().for_each(|r| *r = (*r).min(tag_rows.max(1) - 1));
    }
    for (k, (px, tag)) in tags.iter().enumerate() {
        let row = rows[k];
        let next = (k + 1..tags.len()).find(|&j| rows[j] == row).map_or(rect.max.x, |j| tags[j].0);
        let (text, fill, ink) = match tag {
            Tag::Cursor(i) => (cursor_name(*i), amber, Color32::BLACK),
            Tag::Mark(m) => (format!("🔖{} {}", m.name, m.note).trim_end().to_owned(), red, Color32::WHITE),
        };
        let galley = painter.layout_no_wrap(text, font.clone(), ink);
        let w = (galley.size().x + 4.0).min((next - px - 1.0).max(14.0));
        let top = strip.min.y + row as f32 * MARK_ROW;
        let r = Rect::from_min_size(pos2(px - 1.0, top), vec2(w, MARK_ROW - 2.0));
        let clip = painter.with_clip_rect(r.intersect(rect));
        clip.rect_filled(r, 2.0, fill);
        clip.galley(pos2(r.min.x + 2.0, r.center().y - galley.size().y / 2.0), galley, ink);
        if let Tag::Mark(m) = tag {
            let resp = ui.interact(r, ui.scope_id().with(("bookmark", m.line)), Sense::click());
            let tip = if m.note.is_empty() {
                format!("{}, line {}", m.name, m.line + 1)
            } else {
                format!("{}, line {}: {}", m.name, m.line + 1, m.note)
            };
            if resp.on_hover_text(format!("{tip}\nClick: go to the line")).clicked() {
                out.goto = Some(m.line);
            }
        }
    }
    // Δt between neighbours, on the strip's second row, cut to its span.
    let dt_y = strip.min.y + tag_rows as f32 * MARK_ROW + MARK_ROW / 2.0;
    for pair in placed.windows(2) {
        let ((ni, a), (nj, b)) = ((&pair[0].0, pair[0].1), (&pair[1].0, pair[1].1));
        let (pa, pb) = (to_px(a, x0, x1).max(plot.min.x), to_px(b, x0, x1).min(plot.max.x));
        if pb <= pa {
            continue;
        }
        let stroke = Stroke::new(1.0, amber);
        painter.line_segment([pos2(pa, dt_y), pos2(pb, dt_y)], stroke);
        painter.line_segment([pos2(pa, dt_y - 4.0), pos2(pa, dt_y + 4.0)], stroke);
        painter.line_segment([pos2(pb, dt_y - 4.0), pos2(pb, dt_y + 4.0)], stroke);
        // Just the time: the ends' names are on their tags; an end out of
        // view is named (owner, 2026-10-25).
        let dt = duration(b - a, data.is_time);
        let text = match (a < x0, b > x1) {
            (false, false) => dt,
            (true, false) => format!("‹ {ni}  {dt}"),
            (false, true) => format!("{dt}  {nj} ›"),
            (true, true) => format!("‹ {ni}  {dt}  {nj} ›"),
        };
        let galley = painter.layout_no_wrap(text, font.clone(), amber);
        let at = pos2(((pa + pb) / 2.0 - galley.size().x / 2.0).max(pa + 3.0), dt_y - galley.size().y / 2.0);
        let span = Rect::from_min_max(pos2(pa + 3.0, dt_y - 8.0), pos2(pb - 3.0, dt_y + 8.0));
        let clip = painter.with_clip_rect(span.intersect(rect));
        clip.rect_filled(Rect::from_min_size(at, galley.size()).expand(1.0), 2.0, ui.visuals().panel_fill);
        clip.galley(at, galley, amber);
    }

    // ----- lifecycles: a line where each starts (red after one that ended
    // badly), labelled at the top of the lanes (owner, 2026-11-06); only
    // those plotted (owner, 2026-11-07) -----
    // Labels only where they don't run into the one before.
    let mut label_end = f32::NEG_INFINITY;
    for def in signals.lifecycles().filter(|d| state.is_plotted(d.name())) {
        let Some(l) = def.cached(file) else { continue };
        for (i, life) in l.lives.iter().enumerate() {
            let Some(x) = clock.at(file, clock_series.as_ref(), life.first).filter(|x| (x0..=x1).contains(x)) else {
                continue;
            };
            let bad = i > 0 && l.lives[i - 1].outcome.is_bad();
            let c = if bad { red } else { weak };
            let px = to_px(x, x0, x1);
            let line = [pos2(px, plot.min.y), pos2(px, axis_y)];
            painter.extend(egui::Shape::dashed_line(&line, Stroke::new(1.0, c), 2.0, 2.0));
            if px > label_end + 4.0 {
                let label = format!("{} #{}", def.name(), life.number);
                let r =
                    painter.text(pos2(px + 2.0, plot.min.y + 1.0), Align2::LEFT_TOP, label, FontId::monospace(9.0), c);
                label_end = r.max.x;
            }
        }
    }

    // ----- cursor -----
    if let Some(cx) = clock.at(file, clock_series.as_ref(), cursor).filter(|x| (x0..=x1).contains(x)) {
        let px = to_px(cx, x0, x1);
        painter.line_segment([pos2(px, plot.min.y), pos2(px, axis_y)], Stroke::new(1.5, text_color));
    }

    // ----- hover and click: the lane's value at the pointer -----
    if let (Some(p), Some((k, _))) = (response.hover_pos(), hovered_lane)
        && plot.contains(p)
    {
        let lane = &data.lanes[k];
        let x = x0 + (p.x - plot.min.x) as f64 / w * (x1 - x0);
        let at = lane.xs.partition_point(|&v| v <= x).checked_sub(1);
        let mut tip = format!("{}  ", if data.is_time { format_time(x, 6) } else { format!("line {}", x as u64 + 1) });
        match at {
            Some(i) if !lane.ys[i].is_finite() => {
                tip.push_str(&format!("@{}: unset (a lifecycle started on line {})", lane.name, lane.rows[i] + 1));
            }
            Some(i) => {
                let v = match lane.kind {
                    SignalKind::Enum => lane.labels.get(lane.ys[i] as usize).cloned().unwrap_or_default(),
                    _ => lane.ys[i].to_string(),
                };
                if lane.assumed[i] {
                    tip.push_str(&format!(
                        "@{} = {v}  (default: a lifecycle started on line {})",
                        lane.name,
                        lane.rows[i] + 1
                    ));
                } else {
                    tip.push_str(&format!("@{} = {v}  (event on line {})", lane.name, lane.rows[i] + 1));
                }
            }
            None => tip.push_str(&format!("@{}: no event yet", lane.name)),
        }
        let clicked = response.clicked();
        response.on_hover_text_at_pointer(tip);
        if clicked {
            // The nearest event in the lane.
            let j = lane.xs.partition_point(|&v| v < x);
            let near = [j.checked_sub(1), (j < lane.xs.len()).then_some(j)]
                .into_iter()
                .flatten()
                .min_by(|&a, &b| (lane.xs[a] - x).abs().total_cmp(&(lane.xs[b] - x).abs()));
            out.goto = near.map(|i| lane.rows[i]);
        }
    }
    out
}

/// The kept plots: a list to switch to one, and ⋮ to keep this one by a
/// name, make a new empty one, save or delete (like presets).
fn plots_list(ui: &mut egui::Ui, state: &mut PlotState, saved: &[SavedPlot]) -> Option<PlotAction> {
    let mut action = None;
    let cur = state.name.as_deref().and_then(|n| saved.iter().find(|p| p.name == n));
    let label = |p: &SavedPlot| {
        let mut l = p.name.clone();
        if p.changed {
            l.push_str(" *");
        }
        if p.session {
            l.push_str(" •");
        }
        l
    };
    let selected = cur.map_or("(unnamed)".to_owned(), label);
    egui::ComboBox::from_id_salt("kept-plots").width(130.0).selected_text(selected).show_ui(ui, |ui| {
        if saved.is_empty() {
            ui.weak("No kept plots yet: ⋮ → Keep this plot as…");
        }
        for p in saved {
            let tip = if p.session { "Switch to it\nKept for this session only (•)" } else { "Switch to it" };
            if ui.selectable_label(cur.is_some_and(|c| c.name == p.name), label(p)).on_hover_text(tip).clicked() {
                action = Some(PlotAction::Use(p.name.clone()));
            }
        }
    });
    ui.menu_button("⋮", |ui| {
        if ui.button("New plot").on_hover_text("A blank plot, without a name (a kept one stays kept)").clicked() {
            action = Some(PlotAction::Blank);
            ui.close();
        }
        let tip = "Its lanes, y axes, markers and events, by a name: Keep (this session) or Save (to disk)";
        if ui.button("Keep this plot as…").on_hover_text(tip).clicked() {
            state.naming = Some(String::new());
            ui.close();
        }
        if let Some(c) = cur {
            let name = c.name.clone();
            let (text, tip) = if c.session {
                (format!("Save '{name}' to disk"), "To the plots folder, for later sessions")
            } else {
                (format!("Save '{name}'"), "Overwrites its file")
            };
            if ui.add_enabled(c.changed || c.session, egui::Button::new(text)).on_hover_text(tip).clicked() {
                action = Some(PlotAction::Save);
                ui.close();
            }
            let sure = state.confirm_delete.as_deref() == Some(&name);
            let del = if sure { format!("Really delete '{name}'?") } else { format!("Delete '{name}'…") };
            if ui.button(del).clicked() {
                if sure {
                    action = Some(PlotAction::Delete(name));
                    state.confirm_delete = None;
                    ui.close();
                } else {
                    state.confirm_delete = Some(name);
                }
            }
        }
    })
    .response
    .on_hover_text("New plot, keep, save or delete plots");
    action
}

/// The row where a name is typed to keep this plot by (⋮ → Keep…).
fn naming_row(ui: &mut egui::Ui, state: &mut PlotState, saved: &[SavedPlot]) -> Option<PlotAction> {
    let name = state.naming.as_mut()?;
    let (mut action, mut close) = (None, false);
    ui.horizontal(|ui| {
        ui.label("Keep this plot as:");
        let r = ui.add(egui::TextEdit::singleline(name).hint_text("plot name").desired_width(120.0));
        r.request_focus();
        let n = name.trim().to_owned();
        let problem = if n.is_empty() {
            Some("Type a name")
        } else if !nerka_core::preset::valid_name(&n) {
            Some("No . \" # : / \\ * ? < > | and no spaces at the ends")
        } else if saved.iter().any(|p| p.name == n) {
            Some("A plot with this name is kept: pick it and save")
        } else {
            None
        };
        let ok = problem.is_none();
        let keep = ui.add_enabled(ok, egui::Button::new("Keep")).on_hover_text("For this session only");
        let save = ui.add_enabled(ok, egui::Button::new("Save")).on_hover_text("To disk");
        let enter = ok && r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
        if keep.clicked() || enter {
            action = Some(PlotAction::Keep { name: n.clone(), store: false });
            close = true;
        }
        if save.clicked() {
            action = Some(PlotAction::Keep { name: n.clone(), store: true });
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
        state.naming = None;
    }
    action
}

/// `name` cut to `fit` characters, keeping its end (owner, 2026-10-28:
/// names differ at the end, `nav_speed_front` / `nav_speed_rear`).
pub(crate) fn name_tail(name: &str, fit: usize) -> String {
    let n = name.chars().count();
    if n <= fit {
        return name.to_owned();
    }
    let (i, _) = name.char_indices().nth(n - (fit - 1)).expect("n > fit");
    format!("…{}", &name[i..])
}

/// A time cursor's name: A … Z, then C27 and so on.
fn cursor_name(i: usize) -> String {
    match u8::try_from(i).ok().filter(|&i| i < 26) {
        Some(i) => char::from(b'A' + i).to_string(),
        None => format!("C{}", i + 1),
    }
}

/// A time difference: µs, ms, s, or h m s; a number of lines without time.
fn duration(dt: f64, is_time: bool) -> String {
    if !is_time {
        let n = dt.round() as i64;
        return format!("{n} line{}", if n.abs() == 1 { "" } else { "s" });
    }
    let a = dt.abs();
    let sign = if dt < 0.0 { "-" } else { "" };
    if a < 1e-3 {
        format!("{sign}{:.1} µs", a * 1e6)
    } else if a < 1.0 {
        format!("{sign}{:.3} ms", a * 1e3)
    } else if a < 3600.0 {
        format!("{sign}{a:.3} s")
    } else {
        let s = a.rem_euclid(60.0);
        let m = (a / 60.0).floor() as u64;
        format!("{sign}{}h {}m {s:.3}s", m / 60, m % 60)
    }
}

/// The header line: each cursor's place, and Δt between neighbours.
fn cursor_summary(cursors: &[f64], marks: &[BookmarkCursor], is_time: bool) -> String {
    let at = |x: f64| if is_time { format_time(x, 6) } else { format!("line {}", x.round() as i64 + 1) };
    let mut parts: Vec<String> =
        cursors.iter().enumerate().map(|(i, &c)| format!("{} {}", cursor_name(i), at(c))).collect();
    parts.extend(marks.iter().map(|m| format!("{} {}", m.name(), at(m.x))));
    let mut placed: Vec<(String, f64)> = cursors.iter().enumerate().map(|(i, &c)| (cursor_name(i), c)).collect();
    placed.extend(marks.iter().map(|m| (m.name(), m.x)));
    placed.sort_by(|a, b| a.1.total_cmp(&b.1));
    for pair in placed.windows(2) {
        parts.push(format!("Δ {}→{} {}", pair[0].0, pair[1].0, duration(pair[1].1 - pair[0].1, is_time)));
    }
    parts.join("   ")
}

/// A picture of the plot with all its lanes: the lanes are scrolled
/// through, a picture taken at each place, and the parts put together.
#[derive(Default)]
struct Capture {
    /// Where the lanes were scrolled before.
    back_to: f32,
    parts: Vec<ColorImage>,
    scrolls: Vec<f32>,
    /// The part being taken (filled outside the frame).
    pending: Arc<Mutex<Option<ColorImage>>>,
    waiting: bool,
    /// Frames to let pass after scrolling, before taking a part.
    settle: u8,
}

/// The lane at height `y` (for a dropped lane), within `0..n`.
fn lane_at(y: f32, top: f32, lane_h: f32, scroll: f32, n: usize) -> usize {
    (((y - top + scroll) / lane_h).floor().max(0.0) as usize).min(n.saturating_sub(1))
}

/// Move lane `from` to place `to` (`lanes`: the lanes' names in order) in
/// `plotted`, which may also hold signals without a lane (not built yet).
fn move_lane(plotted: &mut Vec<String>, lanes: &[String], from: usize, to: usize) {
    let (Some(name), Some(target)) = (lanes.get(from), lanes.get(to)) else { return };
    let Some(i) = plotted.iter().position(|n| n == name) else { return };
    let moving = plotted.remove(i);
    let Some(j) = plotted.iter().position(|n| n == target) else {
        plotted.insert(i, moving);
        return;
    };
    // Down: after the target; up: before it.
    plotted.insert(if to > from { j + 1 } else { j }, moving);
}

/// A bookmark drawn as a cursor.
struct BookmarkCursor {
    line: LineNo,
    name: String,
    note: String,
    x: f64,
}

impl BookmarkCursor {
    /// Its name in Δt and the summary: 🔖 and the bookmark's name.
    fn name(&self) -> String {
        format!("🔖{}", self.name)
    }
}

/// A tag in the strip above the lanes.
enum Tag<'m> {
    Cursor(usize),
    Mark(&'m BookmarkCursor),
}

/// The time cursors' and bookmarks' tags in `x0..=x1`, left to right, with
/// where each starts (`to_px`).
fn tags_in_view<'m>(
    cursors: &[f64],
    marks: &'m [BookmarkCursor],
    (x0, x1): (f64, f64),
    to_px: impl Fn(f64) -> f32,
) -> Vec<(f32, Tag<'m>)> {
    let mut tags: Vec<(f32, Tag)> = cursors
        .iter()
        .enumerate()
        .filter(|(_, c)| (x0..=x1).contains(*c))
        .map(|(i, &c)| (to_px(c), Tag::Cursor(i)))
        .collect();
    tags.extend(marks.iter().filter(|m| (x0..=x1).contains(&m.x)).map(|m| (to_px(m.x), Tag::Mark(m))));
    tags.sort_by(|a, b| a.0.total_cmp(&b.0));
    tags
}

/// The row of each tag (`stack_tags`): what must show of one is its name.
fn tag_rows_of(ui: &egui::Ui, tags: &[(f32, Tag)], rows: usize) -> Vec<usize> {
    let font = FontId::monospace(11.0);
    let spans: Vec<(f32, f32)> = tags
        .iter()
        .map(|(px, tag)| {
            let name = match tag {
                Tag::Cursor(i) => cursor_name(*i),
                Tag::Mark(m) => m.name(),
            };
            (*px, ui.painter().layout_no_wrap(name, font.clone(), Color32::WHITE).size().x + 4.0)
        })
        .collect();
    stack_tags(&spans, rows)
}

/// Rows for tags at `px` (left to right) that need `w` to show: each goes in
/// the first row free from its x on, so none hides the one before it; at
/// most `rows` rows (at least one), and with none free, the row that frees
/// first.
fn stack_tags(spans: &[(f32, f32)], rows: usize) -> Vec<usize> {
    let mut ends: Vec<f32> = Vec::new();
    spans
        .iter()
        .map(|&(px, w)| {
            let row = match ends.iter().position(|&end| px >= end) {
                Some(r) => r,
                None if ends.len() < rows.max(1) => {
                    ends.push(px);
                    ends.len() - 1
                }
                None => (0..ends.len()).min_by(|&a, &b| ends[a].total_cmp(&ends[b])).unwrap_or(0),
            };
            ends[row] = px + w + 1.0;
            row
        })
        .collect()
}

/// A right-click menu with fields in it (a note, marker labels and values).
/// A plain context menu closes on any click, inside it too, so its fields
/// couldn't be typed in; this one closes on a click outside, Esc or its own
/// buttons.
trait FieldMenu {
    fn context_menu_with_fields(&self, add: impl FnOnce(&mut egui::Ui));
}

impl FieldMenu for egui::Response {
    fn context_menu_with_fields(&self, add: impl FnOnce(&mut egui::Ui)) {
        egui::Popup::context_menu(self).close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside).show(add);
    }
}

/// The plot's right-click menu: time cursors (`x`: where it was opened),
/// and a bookmark (with `note`) on `row`, the nearest event's line (in
/// that lane). Returns the bookmark to set.
fn cursor_menu(
    ui: &mut egui::Ui,
    cursors: &mut Vec<f64>,
    x: Option<f64>,
    row: Option<(LineNo, String)>,
    note: &mut String,
) -> Option<(LineNo, String)> {
    let mut bookmark = None;
    if let Some((line, lane)) = row {
        ui.label(egui::RichText::new("Bookmark").strong());
        ui.add(egui::TextEdit::singleline(note).hint_text("note").desired_width(200.0));
        let r = ui.button(format!("🔖 Bookmark line {}", line + 1));
        if r.on_hover_text(format!("The event of @{lane} nearest to where you clicked")).clicked() {
            bookmark = Some((line, note.trim().to_owned()));
            ui.close();
        }
        ui.separator();
    }
    ui.label(egui::RichText::new("Time cursors").strong());
    let Some(x) = x else { return bookmark };
    if ui.button(format!("Place cursor {} here", cursor_name(cursors.len()))).clicked() {
        cursors.push(x);
        ui.close();
    }
    let near = (0..cursors.len()).min_by(|&a, &b| (cursors[a] - x).abs().total_cmp(&(cursors[b] - x).abs()));
    if let Some(i) = near
        && ui.button(format!("Remove cursor {}", cursor_name(i))).clicked()
    {
        cursors.remove(i);
        ui.close();
    }
    if !cursors.is_empty() && ui.button("Remove all cursors").clicked() {
        cursors.clear();
        ui.close();
    }
    bookmark
}

/// The y-axis menu of a number lane; `all` is the range of all its values.
fn y_axis_menu(ui: &mut egui::Ui, y: &mut YAxis, all: (f64, f64)) {
    ui.label(egui::RichText::new("Y axis").strong());
    if ui.radio(*y == YAxis::All, "Fit all values").clicked() {
        *y = YAxis::All;
    }
    if ui.radio(*y == YAxis::Visible, "Fit the visible values").clicked() {
        *y = YAxis::Visible;
    }
    let fixed = matches!(y, YAxis::Fixed(..));
    if ui.radio(fixed, "Fixed").clicked() && !fixed {
        *y = YAxis::Fixed(all.0, all.1);
    }
    if let YAxis::Fixed(lo, hi) = y {
        let speed = (*hi - *lo).abs().max(1.0) * 0.005;
        ui.horizontal(|ui| {
            ui.label("from");
            ui.add(egui::DragValue::new(lo).speed(speed));
            ui.label("to");
            ui.add(egui::DragValue::new(hi).speed(speed));
        });
        if *hi <= *lo {
            *hi = *lo + 1.0;
        }
    }
}

/// A number lane's markers: each value and label, remove; add one (at the
/// middle of `all`, the values' range).
fn markers_menu(ui: &mut egui::Ui, markers: &mut Vec<Marker>, all: (f64, f64)) {
    ui.label(egui::RichText::new("Markers").strong())
        .on_hover_text("Horizontal lines at chosen values: limits, thresholds");
    let speed = (all.1 - all.0).abs().max(1.0) * 0.005;
    let mut remove = None;
    for (i, m) in markers.iter_mut().enumerate() {
        ui.horizontal(|ui| {
            ui.add(egui::DragValue::new(&mut m.value).speed(speed));
            ui.add(egui::TextEdit::singleline(&mut m.label).hint_text("label").desired_width(90.0));
            if ui.small_button("−").on_hover_text("Remove").clicked() {
                remove = Some(i);
            }
        });
    }
    if let Some(i) = remove {
        markers.remove(i);
    }
    if ui.button("+ Add marker").clicked() {
        markers.push(Marker { value: (all.0 + all.1) / 2.0, label: String::new() });
    }
}

/// A number's state: a step line, at most one step per pixel column.
fn draw_steps(
    painter: &egui::Painter,
    lane: &Lane,
    area: Rect,
    events: std::ops::Range<usize>,
    to_px: impl Fn(f64) -> f32,
    to_py: impl Fn(f64) -> f32,
    color: Color32,
) {
    let (from, to) = (events.start, events.end);
    let stroke = Stroke::new(1.5, color);
    // A lane with defaults or unset spans: drawn piece by piece.
    if lane.assumed.iter().any(|&a| a) {
        return draw_pieces(painter, lane, area, from, to, to_px, to_py, stroke);
    }
    let mut line: Vec<Pos2> = Vec::new();
    // Per column: the range of values seen, drawn as one vertical stroke.
    let (mut col, mut lo, mut hi) = (f32::NAN, f32::NAN, f32::NAN);
    let flush = |col: f32, lo: f32, hi: f32| {
        if col.is_finite() && hi - lo >= 1.0 {
            painter.line_segment([pos2(col, lo), pos2(col, hi)], stroke);
        }
    };
    for i in from..to.min(lane.xs.len()) {
        let px = to_px(lane.xs[i]).max(area.min.x).floor();
        let py = to_py(lane.ys[i]);
        if px == col {
            (lo, hi) = (lo.min(py), hi.max(py));
            line.last_mut().expect("a point in this column").y = py;
            continue;
        }
        flush(col, lo, hi);
        if let Some(prev) = line.last().copied() {
            line.push(pos2(px, prev.y));
        }
        line.push(pos2(px, py));
        (col, lo, hi) = (px, py, py);
    }
    flush(col, lo, hi);
    // The last state holds to the right edge (if the lane goes on).
    if let Some(last) = line.last().copied() {
        let end = if to < lane.xs.len() { to_px(lane.xs[to]) } else { area.max.x };
        line.push(pos2(end.min(area.max.x), last.y));
    }
    painter.add(egui::Shape::line(line, stroke));
}

/// A number's state with defaults (dashed: assumed) and unset spans
/// (nothing): a step per point, no thinning per pixel column.
#[allow(clippy::too_many_arguments)]
fn draw_pieces(
    painter: &egui::Painter,
    lane: &Lane,
    area: Rect,
    from: usize,
    to: usize,
    to_px: impl Fn(f64) -> f32,
    to_py: impl Fn(f64) -> f32,
    stroke: Stroke,
) {
    let end = to.min(lane.xs.len());
    for i in from..end {
        let y = lane.ys[i];
        if !y.is_finite() {
            continue;
        }
        let a = to_px(lane.xs[i]).max(area.min.x);
        let b = if i + 1 < lane.xs.len() { to_px(lane.xs[i + 1]) } else { area.max.x }.min(area.max.x);
        let py = to_py(y);
        let seg = [pos2(a, py), pos2(b.max(a), py)];
        if lane.assumed[i] {
            painter.extend(egui::Shape::dashed_line(&seg, stroke, 4.0, 3.0));
        } else {
            painter.line_segment(seg, stroke);
        }
        // The step to the next value.
        if i + 1 < end && lane.ys[i + 1].is_finite() && b < area.max.x {
            painter.line_segment([pos2(b, py), pos2(b, to_py(lane.ys[i + 1]))], stroke);
        }
    }
}

/// A band of an enum lane, in pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Band {
    /// One value (its id; NaN: unset), from an event or assumed (a default).
    Value { from: f32, to: f32, y: f64, assumed: bool },
    /// The value changes within these pixels: no one value to show.
    Mixed { from: f32, to: f32 },
}

/// The bands of points `from..to` of `lane` in `area`. Runs of one value
/// (a default is a run of its own) narrower than a pixel gather in a mixed
/// band (owner, 2026-11-07: a colour there looked like one value, and
/// zooming in showed another).
fn bands(lane: &Lane, area: Rect, from: usize, to: usize, to_px: impl Fn(f64) -> f32) -> Vec<Band> {
    let end = to.min(lane.xs.len());
    let mut out = Vec::new();
    let mut mixed: Option<(f32, f32)> = None;
    let mut i = from;
    while i < end {
        let assumed = lane.assumed.get(i).copied().unwrap_or(false);
        let mut j = i + 1;
        while j < end && !assumed && lane.ys[j].to_bits() == lane.ys[i].to_bits() && !lane.assumed[j] {
            j += 1;
        }
        let start = to_px(lane.xs[i]).max(area.min.x);
        let stop = if j < lane.xs.len() { to_px(lane.xs[j]) } else { area.max.x }.min(area.max.x);
        if start >= area.max.x {
            break;
        }
        if stop - start < 1.0 {
            mixed = Some(match mixed {
                Some((a, b)) => (a.min(start), b.max(stop)),
                None => (start, stop),
            });
        } else {
            if let Some((a, b)) = mixed.take() {
                out.push(Band::Mixed { from: a, to: b.max(a + 1.0) });
            }
            out.push(Band::Value { from: start, to: stop, y: lane.ys[i], assumed });
        }
        i = j;
    }
    if let Some((a, b)) = mixed {
        out.push(Band::Mixed { from: a, to: b.max(a + 1.0) });
    }
    out
}

/// An enum's state: bands coloured by value, labelled where wide enough;
/// grey where it changes within a pixel.
#[allow(clippy::too_many_arguments)]
fn draw_bands(
    painter: &egui::Painter,
    lane: &Lane,
    area: Rect,
    from: usize,
    to: usize,
    to_px: impl Fn(f64) -> f32,
    font: &FontId,
    mixed_color: Color32,
) {
    for b in bands(lane, area, from, to, to_px) {
        match b {
            Band::Mixed { from, to } => {
                let band = Rect::from_min_max(pos2(from, area.min.y), pos2(to, area.max.y));
                painter.rect_filled(band, 0.0, mixed_color.gamma_multiply(0.35));
            }
            // Unset (a lifecycle started, no default): no band.
            Band::Value { y, .. } if !y.is_finite() => {}
            Band::Value { from, to, y, assumed } => {
                let id = y as usize;
                let band = Rect::from_min_max(pos2(from, area.min.y), pos2(to, area.max.y));
                let c = crate::library::palette(id + 3);
                if assumed {
                    // A default: assumed, not from the log (owner, 2026-11-06).
                    painter.rect_filled(band, 0.0, c.gamma_multiply(0.15));
                    let s = Stroke::new(1.0, c);
                    let (t, b) = (area.min.y + 0.5, area.max.y - 0.5);
                    painter.extend(egui::Shape::dashed_line(&[pos2(from, t), pos2(to, t)], s, 4.0, 3.0));
                    painter.extend(egui::Shape::dashed_line(&[pos2(from, b), pos2(to, b)], s, 4.0, 3.0));
                } else {
                    painter.rect_filled(band, 0.0, c.gamma_multiply(0.45));
                }
                let label = &lane.labels[id];
                let label = if assumed { format!("{label} (default)") } else { label.clone() };
                if band.width() > 8.0 * label.chars().count() as f32 + 6.0 {
                    painter.text(
                        band.left_center() + vec2(3.0, 0.0),
                        Align2::LEFT_CENTER,
                        label,
                        font.clone(),
                        Color32::BLACK,
                    );
                }
            }
        }
    }
}

/// The events: a tick per pixel column that has one.
fn draw_ticks(
    painter: &egui::Painter,
    lane: &Lane,
    area: Rect,
    from: usize,
    to: usize,
    to_px: impl Fn(f64) -> f32,
    color: Color32,
) {
    let mut last = f32::NAN;
    for i in from..to.min(lane.xs.len()) {
        if lane.assumed[i] {
            continue;
        }
        let px = to_px(lane.xs[i]).floor();
        if px != last {
            painter.line_segment([pos2(px, area.max.y - 6.0), pos2(px, area.max.y + 2.0)], Stroke::new(1.0, color));
            last = px;
        }
    }
}

/// A round tick step near `raw`: seconds (1 µs … days) on a time axis,
/// 1-2-5 steps of lines otherwise.
fn nice_step(raw: f64, is_time: bool) -> f64 {
    const TIME: [f64; 22] = [
        1e-6, 2e-6, 5e-6, 1e-5, 2e-5, 5e-5, 1e-4, 2e-4, 5e-4, 1e-3, 2e-3, 5e-3, 0.01, 0.02, 0.05, 0.1, 0.2, 0.5, 1.0,
        2.0, 5.0, 10.0,
    ];
    const LONG: [f64; 9] = [15.0, 30.0, 60.0, 120.0, 300.0, 600.0, 1800.0, 3600.0, 21_600.0];
    if is_time {
        if let Some(s) = TIME.iter().chain(LONG.iter()).find(|&&s| s >= raw) {
            return *s;
        }
        return (raw / 86_400.0).ceil() * 86_400.0;
    }
    let mag = 10f64.powf(raw.max(1.0).log10().floor());
    [1.0, 2.0, 5.0, 10.0].iter().map(|m| m * mag).find(|&s| s >= raw).unwrap_or(10.0 * mag)
}

fn x_label(x: f64, step: f64, is_time: bool) -> String {
    if !is_time {
        return format!("{}", x as u64 + 1);
    }
    let digits = if step >= 1.0 { 0 } else { (-step.log10()).ceil() as usize };
    let s = format_time(x, digits);
    // Only the clock part: the date is in the tooltip.
    s.rsplit(' ').find(|p| p.contains(':')).unwrap_or(&s).to_owned()
}

/// A value for the y labels: short, without float noise.
fn short_num(v: f64) -> String {
    if v.fract() == 0.0 && v.abs() < 1e12 {
        format!("{v}")
    } else {
        format!("{v:.3}").trim_end_matches('0').trim_end_matches('.').to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A signal bound to a lifecycle of two ECUs: a lane per ECU, a
    /// default point (assumed) where each lifecycle starts, its events.
    /// Values changing within a pixel are one grey band, not the colour of
    /// the first of them (bug, owner, 2026-11-07).
    #[test]
    fn values_changing_within_a_pixel_are_mixed() {
        // a, b, a, b at x 0.0, 0.1, 0.2, 0.3, then a from 1 to 5, b from 5.
        let lane = Lane {
            name: "s".into(),
            signal: "s".into(),
            kind: SignalKind::Enum,
            xs: vec![0.0, 0.1, 0.2, 0.3, 1.0, 5.0],
            ys: vec![0.0, 1.0, 0.0, 1.0, 0.0, 1.0],
            rows: vec![0, 1, 2, 3, 4, 5],
            assumed: vec![false, false, false, false, true, false],
            labels: vec!["a".into(), "b".into()],
            y_range: (0.0, 1.0),
        };
        let area = Rect::from_min_max(pos2(0.0, 0.0), pos2(10.0, 10.0));
        // One pixel per x unit.
        let got = bands(&lane, area, 0, lane.xs.len(), |x| x as f32);
        assert_eq!(
            got,
            [
                Band::Mixed { from: 0.0, to: 1.0 },
                Band::Value { from: 1.0, to: 5.0, y: 0.0, assumed: true },
                Band::Value { from: 5.0, to: 10.0, y: 1.0, assumed: false },
            ]
        );
        // Zoomed in, each is its own band.
        let got = bands(&lane, area, 0, 4, |x| x as f32 * 100.0);
        assert!(got.iter().all(|b| matches!(b, Band::Value { .. })), "{got:?}");
    }

    #[test]
    fn a_bound_signal_resets_where_its_lifecycle_starts() {
        use nerka_core::input::dlt::writer::{Msg, Value};
        // (ecu, uptime in s, text): A reboots before line 4.
        let rows = [("A", 1.0, "s=up"), ("B", 1.0, "x"), ("A", 20.0, "s=down"), ("B", 21.0, "s=on"), ("A", 0.5, "y")];
        let mut bytes = Vec::new();
        for (i, (ecu, up, text)) in rows.iter().enumerate() {
            let mut m = Msg::log(nerka_core::Level::Info, ecu, "APP", "CTX", vec![Value::Str((*text).into()).into()]);
            m.secs = 1_760_000_000 + i as u32;
            m.timestamp = Some((up * 10_000.0) as u32);
            m.encode(&mut bytes);
        }
        let file = nerka_core::DltFile::from_bytes("t.dlt", bytes);
        let mut signals = Signals::default();
        for spec in
            ["lifecycle a: lines ecu == A; reset uptime", r"s:enum:per(a):default(idle)=ecu == A AND /s=(?<s>\w+)/"]
        {
            signals.set(SignalDef::parse_spec(spec).unwrap());
        }
        let d = signals.get("s").unwrap().clone();
        d.series(&file, &nerka_core::Progress::new()).unwrap();
        let state = PlotState { plotted: vec!["s".into()], ..Default::default() };
        let mut series_of = |d: &SignalDef| d.cached_series(&file).map(Arc::new);
        let data = build(&state, &signals, &file, &Clock::of(&file, &signals), &mut series_of).unwrap();
        let lane = &data.lanes[0];
        let pts: Vec<(LineNo, bool)> = lane.rows.iter().copied().zip(lane.assumed.iter().copied()).collect();
        // Its events (A's lines); where A's second lifecycle starts, the
        // default (no point where an event is on the start line).
        assert_eq!((lane.name.as_str(), pts), ("s", vec![(0, false), (2, false), (4, true)]));
        assert_eq!(lane.labels[lane.ys[2] as usize], "idle");

        // A lifecycle plotted (owner, 2026-11-07: on and off in the plot):
        // no lane, its starts drawn; alone, the plot spans its lifecycles.
        let clock = Clock::of(&file, &signals);
        let state = PlotState { plotted: vec!["a".into()], ..Default::default() };
        let data = build(&state, &signals, &file, &clock, &mut series_of).unwrap();
        assert!(data.lanes.is_empty());
        let x = |r| clock.at(&file, None, r).unwrap();
        assert_eq!(data.full, (x(0), x(4)));
        let state = PlotState { plotted: vec!["a".into(), "s".into()], ..Default::default() };
        let data = build(&state, &signals, &file, &clock, &mut series_of).unwrap();
        assert_eq!(data.lanes.iter().map(|l| l.name.as_str()).collect::<Vec<_>>(), ["s"]);
    }

    /// A right-click menu with a text field: clicking into the field keeps
    /// the menu open and typing reaches it (a plain context menu closes on
    /// that click).
    #[test]
    fn menus_with_fields_take_typing() {
        let typed = |with_fields: bool| {
            let ctx = egui::Context::default();
            let (mut text, mut field, mut t) = (String::new(), None::<Rect>, 0.0);
            let mut frame = |events: Vec<egui::Event>, text: &mut String, field: &mut Option<Rect>| {
                t += 1.0 / 60.0;
                let screen = Rect::from_min_size(pos2(0.0, 0.0), vec2(600.0, 400.0));
                let input = egui::RawInput { screen_rect: Some(screen), time: Some(t), events, ..Default::default() };
                let mut out = ctx.run_ui(input, |ui| {
                    egui::CentralPanel::default().show(ui, |ui| {
                        let r = ui.allocate_rect(
                            Rect::from_min_size(pos2(0.0, 0.0), vec2(600.0, 400.0)),
                            egui::Sense::click(),
                        );
                        let menu = |ui: &mut egui::Ui| *field = Some(ui.text_edit_singleline(text).rect);
                        if with_fields {
                            r.context_menu_with_fields(menu);
                        } else {
                            r.context_menu(menu);
                        }
                    });
                });
                out.textures_delta.clear();
            };
            let button = |pos, button, pressed| egui::Event::PointerButton {
                pos,
                button,
                pressed,
                modifiers: egui::Modifiers::NONE,
            };
            let at = pos2(100.0, 100.0);
            let right = egui::PointerButton::Secondary;
            frame(vec![], &mut text, &mut field);
            frame(
                vec![egui::Event::PointerMoved(at), button(at, right, true), button(at, right, false)],
                &mut text,
                &mut field,
            );
            frame(vec![], &mut text, &mut field);
            let f = field.take()?;
            let left = egui::PointerButton::Primary;
            let c = f.center();
            frame(
                vec![egui::Event::PointerMoved(c), button(c, left, true), button(c, left, false)],
                &mut text,
                &mut field,
            );
            frame(vec![egui::Event::Text("note".into())], &mut text, &mut field);
            frame(vec![], &mut text, &mut field);
            Some((text, field.is_some()))
        };
        assert_eq!(typed(true), Some(("note".to_owned(), true)), "typed into, still open");
        let plain = typed(false);
        assert_ne!(plain, Some(("note".to_owned(), true)), "a plain context menu closes on the click: {plain:?}");
    }

    /// One frame of a plot of `@v` (one event per second) at 800 x 300.
    fn frame(
        ctx: &egui::Context,
        t: f64,
        events: Vec<egui::Event>,
        st: &mut PlotState,
        sig: &Signals,
        file: &dyn LogSource,
    ) -> Output {
        let screen = Rect::from_min_size(pos2(0.0, 0.0), vec2(800.0, 300.0));
        let input = egui::RawInput { screen_rect: Some(screen), time: Some(t), events, ..Default::default() };
        let mut out = Output::default();
        let mut full = ctx.run_ui(input, |ui| {
            egui::CentralPanel::default().show(ui, |ui| {
                let mut series_of = |d: &SignalDef| d.cached_series(file).map(Arc::new);
                let src = Source {
                    signals: sig,
                    generation: 1,
                    file,
                    cursor: 0,
                    kept: 0,
                    series_of: &mut series_of,
                    in_window: false,
                    bookmarks: &[],
                    saved: &[],
                };
                out = plot_panel(ui, st, src);
            });
        });
        full.textures_delta.clear();
        out
    }

    #[test]
    fn wheel_zooms_drag_pans_click_goes_to_an_event() {
        let text: String = (0..100)
            .map(|i| {
                format!(
                    "00:{:02}:{:02} v={}
",
                    i / 60,
                    i % 60,
                    i % 7
                )
            })
            .collect();
        let file = nerka_core::TextFile::from_bytes("t", text.into_bytes());
        let mut sig = Signals::default();
        sig.set(nerka_core::SignalDef::parse_spec(r"t:time=/^(?<t>\S+)/").unwrap());
        sig.set(nerka_core::SignalDef::parse_spec(r"v=/v=(?<v>\d+)/").unwrap());
        for d in sig.iter() {
            d.series(&file, &nerka_core::Progress::new()).unwrap();
        }
        let mut st = PlotState::default();
        st.toggle("v");
        let ctx = egui::Context::default();
        let mut t = 0.0;
        let mut step = |events, st: &mut PlotState| {
            t += 1.0 / 60.0;
            frame(&ctx, t, events, st, &sig, &file)
        };
        step(vec![], &mut st);
        let full = st.data.as_ref().map(|(_, d)| d.full).expect("built");
        assert_eq!(full, (0.0, 99.0), "x is the captured time");
        assert!(st.range.is_none());
        let at = pos2(GUTTER + 300.0, 40.0);
        step(vec![egui::Event::PointerMoved(at)], &mut st);
        let wheel = egui::Event::MouseWheel {
            unit: egui::MouseWheelUnit::Line,
            delta: vec2(0.0, 2.0),
            phase: egui::TouchPhase::Move,
            source: egui::MouseWheelSource::Wheel,
            modifiers: egui::Modifiers::NONE,
        };
        step(vec![wheel], &mut st);
        let (x0, x1) = st.range.expect("zoomed in");
        assert!((x1 - x0 - 99.0 * 0.64).abs() < 1e-6, "{x0} {x1}");
        assert!(x0 > 0.0 && x1 < 99.0, "around the pointer");
        // A click goes to the nearest event.
        let button = |pressed| egui::Event::PointerButton {
            pos: at,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };
        step(vec![button(true)], &mut st);
        let out = step(vec![button(false)], &mut st);
        let x = x0 + 300.0 / (800.0 - 8.0 - GUTTER as f64) * (x1 - x0);
        assert_eq!(out.goto, Some(x.round() as LineNo));
        // Dragging right shows earlier times.
        let press = |pos, pressed| egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };
        let to = at + vec2(120.0, 0.0);
        step(vec![press(at, true)], &mut st);
        for k in 1..=6 {
            step(vec![egui::Event::PointerMoved(at + vec2(20.0 * k as f32, 0.0))], &mut st);
        }
        step(vec![press(to, false)], &mut st);
        let (y0, y1) = st.range.unwrap();
        assert!(y0 < x0 && (y1 - y0 - (x1 - x0)).abs() < 1e-9, "{y0} {y1}");
    }

    #[test]
    fn many_lanes_keep_their_height_and_scroll() {
        let text: String = (0..50)
            .map(|i| {
                format!(
                    "00:00:{i:02} {}
",
                    (0..12).map(|k| format!("v{k}={i}")).collect::<Vec<_>>().join(" ")
                )
            })
            .collect();
        let file = nerka_core::TextFile::from_bytes("t", text.into_bytes());
        let mut sig = Signals::default();
        sig.set(nerka_core::SignalDef::parse_spec(r"t:time=/^(?<t>\S+)/").unwrap());
        let mut st = PlotState::default();
        for k in 0..12 {
            sig.set(nerka_core::SignalDef::parse_spec(&format!(r"v{k}=/v{k}=(?<v{k}>\d+)/")).unwrap());
            st.toggle(&format!("v{k}"));
        }
        for d in sig.iter() {
            d.series(&file, &nerka_core::Progress::new()).unwrap();
        }
        let ctx = egui::Context::default();
        let mut t = 0.0;
        let mut step = |events, st: &mut PlotState| {
            t += 1.0 / 60.0;
            frame(&ctx, t, events, st, &sig, &file)
        };
        step(vec![], &mut st);
        assert_eq!(st.data.as_ref().map(|(_, d)| d.lanes.len()), Some(12));
        assert!(st.data.as_ref().unwrap().1.lanes.iter().all(|l| l.xs.len() == 50), "every lane has its events");
        let wheel = |y| egui::Event::MouseWheel {
            unit: egui::MouseWheelUnit::Line,
            delta: vec2(0.0, y),
            phase: egui::TouchPhase::Move,
            source: egui::MouseWheelSource::Wheel,
            modifiers: egui::Modifiers::NONE,
        };
        // The wheel over the names scrolls the lanes, not the x axis.
        let names = pos2(40.0, 120.0);
        step(vec![egui::Event::PointerMoved(names)], &mut st);
        step(vec![wheel(-3.0)], &mut st);
        assert!(st.range.is_none(), "no zoom");
        assert_eq!(st.scroll, 3.0 * LANE_MIN / 2.0, "lanes are at their least height");
        // Not past the last lane.
        for _ in 0..20 {
            step(vec![wheel(-3.0)], &mut st);
        }
        let plot_h = st.scroll;
        assert!(plot_h > 0.0 && plot_h < 12.0 * LANE_MIN, "{plot_h}");
        step(vec![wheel(30.0)], &mut st);
        assert_eq!(st.scroll, 0.0);
        // Over the plot, the wheel still zooms.
        step(vec![egui::Event::PointerMoved(pos2(GUTTER + 200.0, 120.0))], &mut st);
        step(vec![wheel(1.0)], &mut st);
        assert!(st.range.is_some() && st.scroll == 0.0);
        // Dragging up scrolls down through the lanes, the x range stays.
        let range = st.range;
        let at = pos2(GUTTER + 200.0, 200.0);
        let button = |pos, pressed| egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };
        step(vec![button(at, true)], &mut st);
        for k in 1..=5 {
            step(vec![egui::Event::PointerMoved(at - vec2(0.0, 10.0 * k as f32))], &mut st);
        }
        step(vec![button(at - vec2(0.0, 50.0), false)], &mut st);
        assert_eq!(st.range, range);
        assert!(st.scroll > 30.0 && st.scroll <= 50.0, "{}", st.scroll);
    }

    #[test]
    fn dragging_a_time_cursor_moves_it_not_the_view() {
        let text: String = (0..100)
            .map(|i| {
                format!(
                    "00:{:02}:{:02} v={}
",
                    i / 60,
                    i % 60,
                    i % 7
                )
            })
            .collect();
        let file = nerka_core::TextFile::from_bytes("t", text.into_bytes());
        let mut sig = Signals::default();
        sig.set(nerka_core::SignalDef::parse_spec(r"t:time=/^(?<t>\S+)/").unwrap());
        sig.set(nerka_core::SignalDef::parse_spec(r"v=/v=(?<v>\d+)/").unwrap());
        for d in sig.iter() {
            d.series(&file, &nerka_core::Progress::new()).unwrap();
        }
        let mut st = PlotState::default();
        st.toggle("v");
        st.cursors = vec![49.5];
        let ctx = egui::Context::default();
        let mut t = 0.0;
        let mut step = |events, st: &mut PlotState| {
            t += 1.0 / 60.0;
            frame(&ctx, t, events, st, &sig, &file)
        };
        step(vec![], &mut st);
        let w = 800.0 - 8.0 - GUTTER;
        let at = pos2(GUTTER + w / 2.0, 60.0);
        let button = |pos, pressed| egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };
        step(vec![egui::Event::PointerMoved(at)], &mut st);
        step(vec![button(at, true)], &mut st);
        for k in 1..=5 {
            step(vec![egui::Event::PointerMoved(at + vec2(10.0 * k as f32, 0.0))], &mut st);
        }
        step(vec![button(at + vec2(50.0, 0.0), false)], &mut st);
        assert!(st.range.is_none(), "the view didn't pan");
        let moved = st.cursors[0] - 49.5;
        assert!((moved - 50.0 / w as f64 * 99.0).abs() < 0.5, "{moved}");
    }

    /// Two bookmarks on one x: their tags stack, each shows its name, and a
    /// click on each goes to its own line.
    #[test]
    fn bookmarks_on_one_x_are_two() {
        // Two lines a second: lines 50 and 51 are both at 00:00:25.
        let text: String = (0..100).map(|i| format!("00:{:02}:{:02} v={}\n", i / 120, i / 2 % 60, i % 7)).collect();
        let file = nerka_core::TextFile::from_bytes("t", text.into_bytes());
        let mut sig = Signals::default();
        sig.set(nerka_core::SignalDef::parse_spec(r"t:time=/^(?<t>\S+)/").unwrap());
        sig.set(nerka_core::SignalDef::parse_spec(r"v=/v=(?<v>\d+)/").unwrap());
        for d in sig.iter() {
            d.series(&file, &nerka_core::Progress::new()).unwrap();
        }
        let marks = [
            Bookmark { line: 50, name: "B1".into(), note: "first".into() },
            Bookmark { line: 51, name: "B2".into(), note: "second".into() },
        ];
        let mut st = PlotState::default();
        st.toggle("v");
        let ctx = egui::Context::default();
        let mut t = 0.0;
        let mut scope = egui::Id::NULL;
        let mut step = |events: Vec<egui::Event>, st: &mut PlotState| {
            t += 1.0 / 60.0;
            let screen = Rect::from_min_size(pos2(0.0, 0.0), vec2(800.0, 300.0));
            let input = egui::RawInput { screen_rect: Some(screen), time: Some(t), events, ..Default::default() };
            let mut out = Output::default();
            let mut full = ctx.run_ui(input, |ui| {
                egui::CentralPanel::default().show(ui, |ui| {
                    let mut series_of = |d: &SignalDef| d.cached_series(&file).map(Arc::new);
                    let src = Source {
                        signals: &sig,
                        generation: 1,
                        file: &file,
                        cursor: 0,
                        kept: 0,
                        series_of: &mut series_of,
                        in_window: false,
                        bookmarks: &marks,
                        saved: &[],
                    };
                    scope = ui.scope_id();
                    out = plot_panel(ui, st, src);
                });
            });
            full.textures_delta.clear();
            (out, scope)
        };
        step(vec![], &mut st);
        let (_, scope) = step(vec![], &mut st);
        let tag = |line: LineNo| ctx.read_response(scope.with(("bookmark", line))).expect("a tag").rect;
        let (b1, b2) = (tag(50), tag(51));
        assert!((b1.min.x - b2.min.x).abs() < 1.0, "one x: {b1:?} {b2:?}");
        assert!(b2.min.y >= b1.max.y, "B2 is a row below B1: {b1:?} {b2:?}");
        let name_w =
            ctx.fonts_mut(|f| f.layout_no_wrap("🔖B1".into(), FontId::monospace(11.0), Color32::WHITE).size().x);
        assert!(b1.width() >= name_w && b2.width() >= name_w, "names show: {b1:?} {b2:?}");
        for (line, r) in [(50, b1), (51, b2)] {
            let at = r.center();
            let button = |pressed| egui::Event::PointerButton {
                pos: at,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: egui::Modifiers::NONE,
            };
            step(vec![egui::Event::PointerMoved(at)], &mut st);
            step(vec![button(true)], &mut st);
            let (out, _) = step(vec![button(false)], &mut st);
            assert_eq!(out.goto, Some(line), "the click on its tag");
        }
    }

    #[test]
    fn tags_stack_where_they_would_hide_a_name() {
        // Apart: one row; on one x or a few px apart: rows down; a row is
        // used again where it is free.
        assert_eq!(stack_tags(&[(0.0, 20.0), (40.0, 20.0)], 4), [0, 0]);
        assert_eq!(stack_tags(&[(0.0, 20.0), (0.0, 20.0), (3.0, 20.0), (30.0, 20.0)], 4), [0, 1, 2, 0]);
        // More than the rows: the one that frees first.
        assert_eq!(stack_tags(&[(0.0, 20.0), (1.0, 20.0), (2.0, 20.0)], 2), [0, 1, 0]);
        assert_eq!(stack_tags(&[(0.0, 20.0), (1.0, 20.0)], 0), [0, 0]);
    }

    /// The plot's ✖ closes it; with nothing plotted it says how to plot.
    #[test]
    fn the_plot_closes_and_keeps_its_lanes() {
        let file = nerka_core::TextFile::from_bytes(
            "t",
            b"v=1
v=2
"
            .to_vec(),
        );
        let mut sig = Signals::default();
        sig.set(nerka_core::SignalDef::parse_spec(r"v=/v=(?<v>\d+)/").unwrap());
        let mut st = PlotState::default();
        st.toggle("v");
        assert!(st.shown, "plotting shows it");
        let ctx = egui::Context::default();
        let at = pos2(783.0, 17.0);
        let click = |pressed| egui::Event::PointerButton {
            pos: at,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };
        frame(&ctx, 0.0, vec![egui::Event::PointerMoved(at)], &mut st, &sig, &file);
        frame(&ctx, 0.1, vec![click(true)], &mut st, &sig, &file);
        let out = frame(&ctx, 0.2, vec![click(false)], &mut st, &sig, &file);
        assert!(out.close, "✖ at the right of the header");
        assert_eq!(st.plotted, ["v"]);

        let layout = st.layout("p");
        let mut empty = PlotState::default();
        empty.apply(&PlotLayout { name: "e".into(), ..Default::default() });
        assert!(empty.shown && empty.plotted.is_empty());
        empty.apply(&layout);
        assert_eq!(empty.layout("p"), layout);
        assert_eq!(empty.name.as_deref(), Some("p"));
    }

    #[test]
    fn long_names_keep_their_end() {
        assert_eq!(name_tail("@speed", 6), "@speed");
        assert_eq!(name_tail("@nav_speed_rear", 8), "…ed_rear");
        assert_eq!(name_tail("@żółw_ćma", 4), "…ćma");
    }

    #[test]
    fn durations_and_cursor_names() {
        assert_eq!(duration(0.000_250, true), "250.0 µs");
        assert_eq!(duration(0.0123, true), "12.300 ms");
        assert_eq!(duration(-3.2, true), "-3.200 s");
        assert_eq!(duration(3725.5, true), "1h 2m 5.500s");
        assert_eq!(duration(12.0, false), "12 lines");
        assert_eq!(duration(1.0, false), "1 line");
        assert_eq!((cursor_name(0), cursor_name(25), cursor_name(26)), ("A".into(), "Z".into(), "C27".into()));
        // In placing order; Δ between neighbours in time.
        assert_eq!(
            cursor_summary(&[10.0, 4.0, 12.5], &[], false),
            "A line 11   B line 5   C line 14   Δ B→A 6 lines   Δ A→C 3 lines"
        );
        // Bookmarks are cursors too.
        let m = BookmarkCursor { line: 7, name: "B1".into(), note: "boot".into(), x: 7.0 };
        assert_eq!(cursor_summary(&[10.0], &[m], false), "A line 11   🔖B1 line 8   Δ 🔖B1→A 3 lines");
    }

    #[test]
    fn lanes_move_by_name() {
        let names = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let lanes = names(&["a", "b", "c", "d"]);
        // `x` is plotted but has no lane yet: it stays where it is.
        let mut plotted = names(&["a", "x", "b", "c", "d"]);
        move_lane(&mut plotted, &lanes, 0, 2);
        assert_eq!(plotted, names(&["x", "b", "c", "a", "d"]), "down: after the target");
        move_lane(&mut plotted, &lanes, 3, 1);
        assert_eq!(plotted, names(&["x", "d", "b", "c", "a"]), "up: before the target");
        // Dropped on a lane: the lane under the pointer, scrolled or not.
        assert_eq!(lane_at(10.0, 0.0, 50.0, 0.0, 4), 0);
        assert_eq!(lane_at(10.0, 0.0, 50.0, 60.0, 4), 1);
        assert_eq!(lane_at(999.0, 0.0, 50.0, 0.0, 4), 3);
        assert_eq!(lane_at(-5.0, 0.0, 50.0, 0.0, 4), 0);
    }

    #[test]
    fn markers_widen_a_fitted_axis() {
        let m = |value| Marker { value, label: String::new() };
        assert_eq!(with_markers((10.0, 20.0), &[]), (10.0, 20.0));
        assert_eq!(with_markers((10.0, 20.0), &[m(15.0)]), (10.0, 20.0));
        assert_eq!(with_markers((10.0, 20.0), &[m(25.0), m(-5.0)]), (-5.0, 25.0));
    }

    #[test]
    fn steps_are_round() {
        assert_eq!(nice_step(0.7, true), 1.0);
        assert_eq!(nice_step(0.0013, true), 0.002);
        assert_eq!(nice_step(40.0, true), 60.0);
        assert_eq!(nice_step(130.0, false), 200.0);
        assert_eq!(x_label(36_001.25, 0.5, true), "10:00:01.3");
        assert_eq!(x_label(20_378.0 * 86_400.0 + 36_001.0, 60.0, true), "10:00:01");
        assert_eq!(short_num(12.5000001), "12.5");
    }
}
