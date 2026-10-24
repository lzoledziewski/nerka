//! The signal plot: one lane per plotted signal on the tab's time axis
//! (`doc/sources-and-signals.md` → "What a signal is"). A number is drawn
//! as its state, a step line; an enum as coloured bands. Each lane can show
//! its events (when the value was received). The cursor is the current
//! line. Wheel zooms, drag pans, double-click shows everything, a click
//! goes to the nearest event; right-clicking a lane's name sets its y axis.
//! The plot can move to a window of its own and back.
//!
//! Drawn with egui's painter (no plot crate): lanes are decimated to one
//! step per pixel column, so millions of events draw as fast as a few.

use eframe::egui::{self, Align2, Color32, FontId, Pos2, Rect, Sense, Stroke, pos2, vec2};
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
    };
    let mut plot = lock(&i.plot);
    let o = egui::CentralPanel::default().show(ui, |ui| plot_panel(ui, &mut plot, src)).inner;
    drop(plot);
    out.goto = o.goto.or(out.goto);
    out.toggle_window |= o.toggle_window;
    *closed |= ui.input(|i| i.viewport().close_requested());
    if out.goto.is_some() || out.toggle_window || *closed {
        ui.ctx().request_repaint_of(egui::ViewportId::ROOT);
    }
}

/// A number lane's y axis.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum YAxis {
    /// Every value of the signal fits.
    All,
    /// The values in the visible x range fit (changes while zooming).
    Visible,
    /// From, to.
    Fixed(f64, f64),
}

/// Which signals are plotted and how, per tab.
#[derive(Default)]
pub struct PlotState {
    /// Plotted signals, in lane order.
    pub plotted: Vec<String>,
    /// Lanes that show their events.
    pub events: std::collections::HashSet<String>,
    /// Visible x range; none: everything.
    range: Option<(f64, f64)>,
    /// Lanes' y axes (absent: [`YAxis::All`]).
    pub y: HashMap<String, YAxis>,
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
            None => self.plotted.push(name.to_owned()),
        }
        self.data = None;
    }

    /// Forget signals that are gone.
    pub fn retain(&mut self, signals: &Signals) {
        self.plotted.retain(|n| signals.get(n).is_some());
        self.events.retain(|n| signals.get(n).is_some());
    }
}

struct PlotData {
    lanes: Vec<Lane>,
    /// x range of all events.
    full: (f64, f64),
    is_time: bool,
}

struct Lane {
    name: String,
    kind: SignalKind,
    /// Events by x (ties keep row order): x, y (value or enum id), row.
    xs: Vec<f64>,
    ys: Vec<f64>,
    rows: Vec<LineNo>,
    /// Enum ids' texts.
    labels: Vec<String>,
    y_range: (f64, f64),
}

/// What the plot asks the tab to do.
#[derive(Default)]
pub struct Output {
    pub goto: Option<LineNo>,
    /// Move the plot to its own window, or back.
    pub toggle_window: bool,
}

const GUTTER: f32 = 120.0;
const AXIS_H: f32 = 18.0;
const LANE_MAX: f32 = 110.0;

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
    for name in &state.plotted {
        let Some(def) = signals.get(name) else { continue };
        let series = series_of(def).ok_or("Collecting events…")?;
        let mut ids: HashMap<Vec<u8>, usize> = HashMap::new();
        let mut labels = Vec::new();
        let mut pts: Vec<(f64, f64, LineNo)> = series
            .iter()
            .filter_map(|(row, v)| {
                let x = x_of(row)?;
                let y = match v {
                    ValueRef::Num(n) => n,
                    ValueRef::Enum(t) => *ids.entry(t.to_vec()).or_insert_with(|| {
                        labels.push(String::from_utf8_lossy(t).into_owned());
                        labels.len() - 1
                    }) as f64,
                };
                Some((x, y, row))
            })
            .collect();
        // Clocks may step back (a logger's time jumps): keep x sorted.
        if pts.windows(2).any(|w| w[1].0 < w[0].0) {
            pts.sort_by(|a, b| a.0.total_cmp(&b.0));
        }
        let (lo, hi) = pts.iter().fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), p| (lo.min(p.1), hi.max(p.1)));
        lanes.push(Lane {
            name: name.clone(),
            kind: def.kind(),
            xs: pts.iter().map(|p| p.0).collect(),
            ys: pts.iter().map(|p| p.1).collect(),
            rows: pts.iter().map(|p| p.2).collect(),
            labels,
            y_range: if lo <= hi { (lo, hi) } else { (0.0, 1.0) },
        });
    }
    let full = lanes
        .iter()
        .filter(|l| !l.xs.is_empty())
        .fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), l| (lo.min(l.xs[0]), hi.max(l.xs[l.xs.len() - 1])));
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
}

pub fn plot_panel(ui: &mut egui::Ui, state: &mut PlotState, src: Source<'_>) -> Output {
    let Source { signals, generation, file, cursor, kept, series_of, in_window } = src;
    let mut out = Output::default();
    ui.horizontal(|ui| {
        let label = if in_window { "⧉ Back into the main window" } else { "⧉ Own window" };
        out.toggle_window = ui.small_button(label).clicked();
        ui.weak(format!("x: {}", Clock::of(file, signals).describe())).on_hover_text(
            "The file's own time, else the first signal of kind time that has events, else line numbers",
        );
        ui.weak("· wheel: zoom · drag: pan · double-click: all · click: go to the event · right-click a name: y axis");
    });
    let clock = Clock::of(file, signals);
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
    let plot = Rect::from_min_max(pos2(rect.min.x + GUTTER, rect.min.y), pos2(rect.max.x - 8.0, rect.max.y - AXIS_H));
    if plot.width() < 20.0 || plot.height() < 10.0 {
        return out;
    }
    let (mut x0, mut x1) = state.range.unwrap_or(data.full);
    let w = plot.width() as f64;
    let to_px = |x: f64, x0: f64, x1: f64| plot.min.x + ((x - x0) / (x1 - x0) * w) as f32;

    // ----- input -----
    if response.double_clicked() {
        state.range = None;
        (x0, x1) = data.full;
    }
    if response.dragged() {
        let dx = response.drag_delta().x as f64 * (x1 - x0) / w;
        (x0, x1) = (x0 - dx, x1 - dx);
        state.range = Some((x0, x1));
    }
    if let Some(p) = response.hover_pos().filter(|_| response.hovered()) {
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
        if notches != 0.0 {
            let at = x0 + (p.x - plot.min.x).clamp(0.0, plot.width()) as f64 / w * (x1 - x0);
            let f = 0.8f64.powf(notches as f64);
            (x0, x1) = (at - (at - x0) * f, at + (x1 - at) * f);
            if x1 - x0 > 1e-9 {
                state.range = Some((x0, x1));
            }
        }
    }

    // ----- lanes -----
    let painter = ui.painter_at(rect);
    let vis = ui.visuals();
    let text_color = vis.text_color();
    let weak = vis.weak_text_color();
    let grid = vis.widgets.noninteractive.bg_stroke.color;
    let n = data.lanes.len().max(1) as f32;
    // Lanes shrink to fit a small panel rather than disappear.
    let lane_h = (plot.height() / n).min(LANE_MAX);
    let font = FontId::monospace(11.0);
    let mut hovered_lane = None;
    for (k, lane) in data.lanes.iter().enumerate() {
        let top = plot.min.y + k as f32 * lane_h;
        if top + lane_h > plot.max.y + 1.0 || lane_h < 8.0 {
            break;
        }
        let area = Rect::from_min_max(pos2(plot.min.x, top + 3.0), pos2(plot.max.x, top + lane_h - 3.0));
        let color = crate::library::palette(k);
        painter.line_segment([pos2(rect.min.x, top + lane_h), pos2(plot.max.x, top + lane_h)], Stroke::new(1.0, grid));
        // The lane's name, and a checkbox for its events, in the gutter.
        // Long names are shortened to fit beside the y labels (hover: all).
        let name = format!("@{}", lane.name);
        let short = match name.char_indices().nth(12) {
            Some((i, _)) if name.chars().count() > 13 => format!("{}…", &name[..i]),
            _ => name.clone(),
        };
        painter.text(pos2(rect.min.x + 4.0, top + 4.0), Align2::LEFT_TOP, short, font.clone(), color);
        // Right-click the name: the y axis.
        let name_rect = Rect::from_min_size(pos2(rect.min.x, top), vec2(GUTTER - 4.0, 18.0));
        let name_r = ui.interact(name_rect, ui.scope_id().with(("lane", &lane.name)), Sense::click());
        if lane.kind != SignalKind::Enum {
            let y = state.y.entry(lane.name.clone()).or_insert(YAxis::All);
            name_r
                .on_hover_text(format!(
                    "{name}
Right-click: y axis"
                ))
                .context_menu(|ui| y_axis_menu(ui, y, lane.y_range));
        } else {
            name_r.on_hover_text(name.as_str());
        }
        let cb = Rect::from_min_size(pos2(rect.min.x + 4.0, top + 20.0), vec2(GUTTER - 10.0, 16.0));
        let mut show = state.events.contains(&lane.name);
        let r = ui.put(cb, egui::Checkbox::new(&mut show, egui::RichText::new("events").small()));
        if r.on_hover_text("Mark the events: the lines where the value was received").changed() {
            if show {
                state.events.insert(lane.name.clone());
            } else {
                state.events.remove(&lane.name);
            }
        }
        if response.hover_pos().is_some_and(|p| p.y >= top && p.y < top + lane_h) {
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
            SignalKind::Enum => draw_bands(&painter, lane, area, from, i1, |x| to_px(x, x0, x1), &font),
            _ => {
                let (lo, hi) = match state.y.get(&lane.name).copied().unwrap_or(YAxis::All) {
                    YAxis::All => lane.y_range,
                    YAxis::Visible => lane.ys[from..i1.max(from + 1).min(lane.ys.len())]
                        .iter()
                        .fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), &v| (lo.min(v), hi.max(v))),
                    YAxis::Fixed(lo, hi) => (lo, hi),
                };
                // Labels show the range; a little room around the values.
                let (lo_l, hi_l) = (lo, hi);
                let pad = if hi > lo { (hi - lo) * 0.08 } else { 1.0 };
                let (lo, hi) = (lo - pad, hi + pad);
                let to_py =
                    |y: f64| (area.max.y - ((y - lo) / (hi - lo)) as f32 * area.height()).clamp(area.min.y, area.max.y);
                draw_steps(&painter, lane, area, from..i1, |x| to_px(x, x0, x1), to_py, color);
                let fmt = |v: f64| short_num(v);
                painter.text(pos2(plot.min.x - 4.0, area.min.y), Align2::RIGHT_TOP, fmt(hi_l), font.clone(), weak);
                painter.text(pos2(plot.min.x - 4.0, area.max.y), Align2::RIGHT_BOTTOM, fmt(lo_l), font.clone(), weak);
            }
        }
        if show {
            draw_ticks(&painter, lane, area, i0, i1, |x| to_px(x, x0, x1), color);
        }
    }

    // ----- axis -----
    let axis_y = plot.max.y;
    painter.line_segment([pos2(plot.min.x, axis_y), pos2(plot.max.x, axis_y)], Stroke::new(1.0, grid));
    let step = nice_step((x1 - x0) / (w / 110.0).max(1.0), data.is_time);
    let mut t = (x0 / step).ceil() * step;
    while t <= x1 {
        let px = to_px(t, x0, x1);
        painter.line_segment([pos2(px, plot.min.y), pos2(px, axis_y)], Stroke::new(1.0, grid.gamma_multiply(0.5)));
        painter.text(pos2(px, axis_y + 2.0), Align2::CENTER_TOP, x_label(t, step, data.is_time), font.clone(), weak);
        t += step;
    }

    // ----- cursor -----
    let clock_series = match &clock {
        Clock::Signal(d) => d.cached_series(file),
        _ => None,
    };
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
            Some(i) => {
                let v = match lane.kind {
                    SignalKind::Enum => lane.labels.get(lane.ys[i] as usize).cloned().unwrap_or_default(),
                    _ => lane.ys[i].to_string(),
                };
                tip.push_str(&format!("@{} = {v}  (event on line {})", lane.name, lane.rows[i] + 1));
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

/// An enum's state: bands coloured by value, labelled where wide enough.
fn draw_bands(
    painter: &egui::Painter,
    lane: &Lane,
    area: Rect,
    from: usize,
    to: usize,
    to_px: impl Fn(f64) -> f32,
    font: &FontId,
) {
    let end = to.min(lane.xs.len());
    let mut i = from;
    // The last pixel column painted: runs narrower than a pixel share one,
    // which shows the first of them.
    let mut painted = f32::NEG_INFINITY;
    while i < end {
        let id = lane.ys[i] as usize;
        // The run of events with this value.
        let start = to_px(lane.xs[i]).max(area.min.x);
        let mut j = i + 1;
        while j < end && lane.ys[j] as usize == id {
            j += 1;
        }
        let stop = if j < lane.xs.len() { to_px(lane.xs[j]) } else { area.max.x }.min(area.max.x);
        let start = start.max(painted);
        let stop = stop.max(start.floor() + 1.0);
        if start < stop && start < area.max.x {
            painted = stop;
            let band = Rect::from_min_max(pos2(start, area.min.y), pos2(stop, area.max.y));
            let c = crate::library::palette(id + 3);
            painter.rect_filled(band, 0.0, c.gamma_multiply(0.45));
            let label = &lane.labels[id];
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
        i = j;
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
