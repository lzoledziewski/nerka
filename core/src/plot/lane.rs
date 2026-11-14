//! A plot's lanes as data (review 2026-11-14, R-B): a signal's points on
//! the tab's clock, and what a frame draws of them — a number's steps, an
//! enum's bands, the ticks of the events. With more points in view than
//! pixels, a frame reads a summary (the least and greatest value per block
//! of points) instead of every point, so it costs about the plot's width
//! at any zoom; what it draws is the same either way (tested below). The
//! GUI paints what these return (`ui/src/plot.rs`).

use crate::signal::{Clock, SignalDef, SignalKind, SignalSeries, ValueRef};
use crate::{LineNo, LogSource};
use rayon::prelude::*;
use std::collections::HashMap;

/// Points per block of the summary's two levels.
const B1: usize = 64;
const B2: usize = 64 * 64;

/// (least, greatest) of nothing.
const NONE: (f64, f64) = (f64::INFINITY, f64::NEG_INFINITY);

/// Both ranges together. `f64::min` / `max` skip NaN (unset values).
fn join(a: (f64, f64), b: (f64, f64)) -> (f64, f64) {
    (a.0.min(b.0), a.1.max(b.1))
}

/// The least and greatest value per block of `B1` and of `B2` points, and
/// each block's first x (a search for a pixel column's end reads these, a
/// few cache lines, instead of probing the points across memory).
#[derive(Debug, Clone, Default)]
struct Summary {
    l1: Vec<(f64, f64)>,
    l2: Vec<(f64, f64)>,
    x1: Vec<f64>,
    x2: Vec<f64>,
}

impl Summary {
    fn of(xs: &[f64], ys: &[f64]) -> Summary {
        let l1: Vec<_> = ys.par_chunks(B1).map(|c| c.iter().fold(NONE, |m, &y| join(m, (y, y)))).collect();
        let l2 = l1.chunks(B2 / B1).map(|c| c.iter().fold(NONE, |m, &v| join(m, v))).collect();
        Summary { l1, l2, x1: xs.iter().step_by(B1).copied().collect(), x2: xs.iter().step_by(B2).copied().collect() }
    }
}

/// One lane: a signal's points by x.
#[derive(Debug, Clone)]
pub struct Lane {
    /// What it shows: the signal's name, with the key for a signal bound to
    /// a lifecycle of several keys (`ota A`).
    pub name: String,
    /// The signal it is of (several lanes for one bound per key).
    pub signal: String,
    pub kind: SignalKind,
    /// Events by x (ties keep row order): x, y (value or enum id), row.
    /// For a signal bound to a lifecycle, also a point where each lifecycle
    /// starts: its default (`assumed`), or NaN (unset).
    pub xs: Vec<f64>,
    pub ys: Vec<f64>,
    pub rows: Vec<LineNo>,
    pub assumed: Vec<bool>,
    /// Enum ids' texts.
    pub labels: Vec<String>,
    /// The values' range (the finite ones; `(0, 1)` without any).
    pub y_range: (f64, f64),
    summary: Summary,
    /// Some point is assumed (or unset): drawn point by point.
    any_assumed: bool,
}

/// A band of an enum lane, in pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Band {
    /// One value (its id; NaN: unset), from an event or assumed (a default).
    Value { from: f32, to: f32, y: f64, assumed: bool },
    /// The value changes within these pixels: no one value to show.
    Mixed { from: f32, to: f32 },
}

/// What a number's lane draws: per pixel column with points, a vertical
/// stroke over the range of its values where that spans a pixel or more
/// (`[x, y_lo, y_hi]`), and the step line through each column's last value.
#[derive(Debug, Default, PartialEq)]
pub struct Steps {
    pub strokes: Vec<[f32; 3]>,
    pub line: Vec<(f32, f32)>,
}

impl Lane {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        name: String,
        signal: String,
        kind: SignalKind,
        xs: Vec<f64>,
        ys: Vec<f64>,
        rows: Vec<LineNo>,
        assumed: Vec<bool>,
        labels: Vec<String>,
    ) -> Lane {
        let summary = Summary::of(&xs, &ys);
        let (lo, hi) = summary.l2.iter().fold(NONE, |m, &v| join(m, v));
        let any_assumed = assumed.iter().any(|&a| a);
        Lane {
            name,
            signal,
            kind,
            xs,
            ys,
            rows,
            assumed,
            labels,
            y_range: if lo <= hi { (lo, hi) } else { (0.0, 1.0) },
            summary,
            any_assumed,
        }
    }

    /// Whether some point is assumed (a default where a lifecycle starts).
    pub fn any_assumed(&self) -> bool {
        self.any_assumed
    }

    /// The least and greatest value of points `from..to` (NaN skipped).
    pub fn min_max(&self, from: usize, to: usize) -> (f64, f64) {
        let to = to.min(self.ys.len());
        let mut m = NONE;
        let mut i = from;
        while i < to {
            if i.is_multiple_of(B2) && i + B2 <= to {
                m = join(m, self.summary.l2[i / B2]);
                i += B2;
            } else if i.is_multiple_of(B1) && i + B1 <= to {
                m = join(m, self.summary.l1[i / B1]);
                i += B1;
            } else {
                m = join(m, (self.ys[i], self.ys[i]));
                i += 1;
            }
        }
        m
    }

    /// The first point after `i` (before `end`) with another value than
    /// point `i`'s; blocks of one value are passed whole. NaN only comes
    /// with assumed points, which are walked one by one.
    fn run_end(&self, i: usize, end: usize) -> usize {
        let v = self.ys[i];
        let same = |b: (f64, f64)| b.0 == v && b.1 == v;
        let mut j = i + 1;
        while j < end {
            if j.is_multiple_of(B2) && j + B2 <= end && same(self.summary.l2[j / B2]) {
                j += B2;
            } else if j.is_multiple_of(B1) && j + B1 <= end && same(self.summary.l1[j / B1]) {
                j += B1;
            } else if self.ys[j].to_bits() == v.to_bits() {
                j += 1;
            } else {
                break;
            }
        }
        j
    }

    /// Points `from..to` by pixel column (`col`, non-decreasing in x): the
    /// first and the end of each column's points.
    fn columns(&self, from: usize, to: usize, col: impl Fn(f64) -> f32, mut f: impl FnMut(f32, usize, usize)) {
        let end = to.min(self.xs.len());
        let mut a = from;
        while a < end {
            let c = col(self.xs[a]);
            let b = self.column_end(a, end, |x| col(x) <= c);
            f(c, a, b);
            a = b;
        }
    }

    /// The first point after `a` (before `end`) whose x isn't `within`
    /// (true for the points from `a` on up to some x). Blocks that start
    /// within are passed whole: a few reads of the block starts, then at
    /// most a block of points.
    fn column_end(&self, a: usize, end: usize, within: impl Fn(f64) -> bool) -> usize {
        let s = &self.summary;
        let mut i = a + 1;
        if i >= end || !within(self.xs[i]) {
            return i.min(end);
        }
        // Point `i` is within; so is every point before it.
        let mut k = i / B2 + 1;
        while k * B2 < end && within(s.x2[k]) {
            i = k * B2;
            k += 1;
        }
        let mut k = i / B1 + 1;
        while k * B1 < end && within(s.x1[k]) {
            i = k * B1;
            k += 1;
        }
        i += 1;
        while i < end && within(self.xs[i]) {
            i += 1;
        }
        i
    }

    /// A number's steps over points `from..to` between pixels `left` and
    /// `right` (`to_px`: x to pixels; `to_py`: value to pixels, falling as
    /// the value grows). Lanes with assumed points are drawn piece by
    /// piece by the GUI instead.
    pub fn steps(
        &self,
        from: usize,
        to: usize,
        (left, right): (f32, f32),
        to_px: impl Fn(f64) -> f32,
        to_py: impl Fn(f64) -> f32,
    ) -> Steps {
        let mut out = Steps::default();
        let col = |x: f64| to_px(x).max(left).floor();
        self.columns(from, to, col, |px, a, b| {
            // The column's values: the step from the last column's value,
            // to this one's last value; a stroke over its range.
            let (ymin, ymax) = self.min_max(a, b);
            let (lo, hi) = if a + 1 == b {
                let py = to_py(self.ys[a]);
                (py, py)
            } else {
                (to_py(ymax), to_py(ymin))
            };
            if ymin <= ymax && hi - lo >= 1.0 {
                out.strokes.push([px, lo, hi]);
            }
            if let Some(&(_, y)) = out.line.last() {
                out.line.push((px, y));
            }
            out.line.push((px, to_py(self.ys[b - 1])));
        });
        // The last state holds to the right edge (if the lane goes on).
        if let Some(&(_, y)) = out.line.last() {
            let end = if to < self.xs.len() { to_px(self.xs[to]) } else { right };
            out.line.push((end.min(right), y));
        }
        out
    }

    /// The events' ticks over points `from..to`: a pixel column each that
    /// has one (assumed points aren't events).
    pub fn ticks(&self, from: usize, to: usize, to_px: impl Fn(f64) -> f32) -> Vec<f32> {
        let mut out = Vec::new();
        let col = |x: f64| to_px(x).floor();
        if self.any_assumed {
            for i in from..to.min(self.xs.len()) {
                let px = col(self.xs[i]);
                if !self.assumed[i] && out.last() != Some(&px) {
                    out.push(px);
                }
            }
            return out;
        }
        self.columns(from, to, col, |px, _, _| out.push(px));
        out
    }

    /// The bands of points `from..to` between pixels `left` and `right`.
    /// Runs of one value (a default is a run of its own) narrower than a
    /// pixel gather in a mixed band (owner, 2026-11-07: a colour there
    /// looked like one value, and zooming in showed another).
    pub fn bands(&self, (left, right): (f32, f32), from: usize, to: usize, to_px: impl Fn(f64) -> f32) -> Vec<Band> {
        let end = to.min(self.xs.len());
        let mut out = Vec::new();
        let mut mixed: Option<(f32, f32)> = None;
        let mut i = from;
        while i < end {
            let assumed = self.assumed.get(i).copied().unwrap_or(false);
            let j = if self.any_assumed {
                let mut j = i + 1;
                while j < end && !assumed && self.ys[j].to_bits() == self.ys[i].to_bits() && !self.assumed[j] {
                    j += 1;
                }
                j
            } else {
                self.run_end(i, end)
            };
            let start = to_px(self.xs[i]).max(left);
            let stop = if j < self.xs.len() { to_px(self.xs[j]) } else { right }.min(right);
            if start >= right {
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
                out.push(Band::Value { from: start, to: stop, y: self.ys[i], assumed });
            }
            i = j;
        }
        if let Some((a, b)) = mixed {
            out.push(Band::Mixed { from: a, to: b.max(a + 1.0) });
        }
        out
    }
}

/// [`lanes`] of several signals `(name, definition, events)`, in parallel.
pub fn lanes_of_all(
    signals: &[(&str, &SignalDef, &SignalSeries)],
    file: &dyn LogSource,
    clock: &Clock,
    clock_series: Option<&SignalSeries>,
) -> Vec<Vec<Lane>> {
    signals.par_iter().map(|(name, def, series)| lanes(name, def, series, file, clock, clock_series)).collect()
}

/// The lanes of signal `name` (`def`, its events `series`) on `clock`:
/// a point per event at its time (or row), and for a signal bound to a
/// lifecycle a point where each lifecycle starts, its default (assumed) or
/// NaN (unset). The times are looked up in parallel.
pub fn lanes(
    name: &str,
    def: &SignalDef,
    series: &SignalSeries,
    file: &dyn LogSource,
    clock: &Clock,
    clock_series: Option<&SignalSeries>,
) -> Vec<Lane> {
    let mut ids: HashMap<Vec<u8>, usize> = HashMap::new();
    let mut labels = Vec::new();
    let mut y_of = |v: ValueRef<'_>| match v {
        ValueRef::Num(n) => n,
        ValueRef::Enum(t) => *ids.entry(t.to_vec()).or_insert_with(|| {
            labels.push(String::from_utf8_lossy(t).into_owned());
            labels.len() - 1
        }) as f64,
    };
    // The points in row order (y, row, assumed); x comes after.
    let n = series.len();
    let (mut ys, mut rows, mut assumed) = (Vec::with_capacity(n), Vec::with_capacity(n), Vec::with_capacity(n));
    let mut starts: Vec<LineNo> = match series.lifecycles() {
        Some(life) => life.lives.iter().rev().map(|l| l.first).collect(),
        None => Vec::new(),
    };
    let reset = def.default_value().map_or(f64::NAN, |d| y_of(d.as_ref()));
    let mut push = |y: f64, row: LineNo, a: bool| {
        ys.push(y);
        rows.push(row);
        assumed.push(a);
    };
    for (row, v) in series.iter() {
        while starts.last().is_some_and(|&s| s <= row) {
            let s = starts.pop().expect("checked");
            if s < row {
                push(reset, s, true);
            }
        }
        push(y_of(v), row, false);
    }
    for s in starts.into_iter().rev() {
        push(reset, s, true);
    }
    // NaN: no time (left out below).
    let mut xs: Vec<f64> = rows.par_iter().map(|&r| clock.at(file, clock_series, r).unwrap_or(f64::NAN)).collect();
    if xs.iter().any(|x| x.is_nan()) {
        let keep: Vec<bool> = xs.iter().map(|x| !x.is_nan()).collect();
        let mut k = keep.iter();
        xs.retain(|_| *k.next().expect("one each"));
        let mut k = keep.iter();
        ys.retain(|_| *k.next().expect("one each"));
        let mut k = keep.iter();
        rows.retain(|_| *k.next().expect("one each"));
        let mut k = keep.iter();
        assumed.retain(|_| *k.next().expect("one each"));
    }
    // Clocks may step back (a logger's time jumps): keep x sorted (a
    // stable sort: ties keep row order).
    if xs.windows(2).any(|w| w[1] < w[0]) {
        let mut order: Vec<usize> = (0..xs.len()).collect();
        order.par_sort_by(|&a, &b| xs[a].total_cmp(&xs[b]));
        xs = order.iter().map(|&i| xs[i]).collect();
        ys = order.iter().map(|&i| ys[i]).collect();
        rows = order.iter().map(|&i| rows[i]).collect();
        assumed = order.iter().map(|&i| assumed[i]).collect();
    }
    vec![Lane::new(name.to_owned(), name.to_owned(), def.kind(), xs, ys, rows, assumed, labels)]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lane(kind: SignalKind, xs: Vec<f64>, ys: Vec<f64>, assumed: Vec<bool>) -> Lane {
        let rows = (0..xs.len() as LineNo).collect();
        Lane::new("s".into(), "s".into(), kind, xs, ys, rows, assumed, vec!["a".into(), "b".into(), "c".into()])
    }

    /// The steps as the GUI drew them before the summary: point by point.
    fn steps_by_point(
        l: &Lane,
        from: usize,
        to: usize,
        (left, right): (f32, f32),
        to_px: impl Fn(f64) -> f32,
        to_py: impl Fn(f64) -> f32,
    ) -> Steps {
        let mut out = Steps::default();
        let (mut col, mut lo, mut hi) = (f32::NAN, f32::NAN, f32::NAN);
        let flush = |out: &mut Steps, col: f32, lo: f32, hi: f32| {
            if col.is_finite() && hi - lo >= 1.0 {
                out.strokes.push([col, lo, hi]);
            }
        };
        for i in from..to.min(l.xs.len()) {
            let px = to_px(l.xs[i]).max(left).floor();
            let py = to_py(l.ys[i]);
            if px == col {
                (lo, hi) = (lo.min(py), hi.max(py));
                out.line.last_mut().unwrap().1 = py;
                continue;
            }
            flush(&mut out, col, lo, hi);
            if let Some(&(_, y)) = out.line.last() {
                out.line.push((px, y));
            }
            out.line.push((px, py));
            (col, lo, hi) = (px, py, py);
        }
        flush(&mut out, col, lo, hi);
        if let Some(&(_, y)) = out.line.last() {
            let end = if to < l.xs.len() { to_px(l.xs[to]) } else { right };
            out.line.push((end.min(right), y));
        }
        out
    }

    /// Random walks with bursts (many points per pixel) and gaps; views
    /// from far out to close in, at the edges too.
    #[test]
    fn steps_ticks_and_ranges_are_the_same_as_point_by_point() {
        let mut rng = 0x2545_F491_4F6C_DD1Du64;
        let mut next = || {
            rng ^= rng << 13;
            rng ^= rng >> 7;
            rng ^= rng << 17;
            rng
        };
        for n in [1usize, 2, 63, 64, 65, 5000, 70_000] {
            let (mut xs, mut ys) = (Vec::with_capacity(n), Vec::with_capacity(n));
            let (mut x, mut y) = (0.0f64, 0.0f64);
            for _ in 0..n {
                x += if next() % 50 == 0 { 300.0 } else { (next() % 3) as f64 * 0.01 };
                y += (next() % 7) as f64 - 3.0;
                xs.push(x);
                ys.push(y);
            }
            let l = lane(SignalKind::Num, xs.clone(), ys.clone(), vec![false; n]);
            let full = (xs[0], xs[n - 1].max(xs[0] + 1.0));
            for zoom in [1.0, 0.3, 0.01, 1e-4] {
                let (x0, x1) = (full.0 + (full.1 - full.0) * 0.1 * (1.0 - zoom), full.0 + (full.1 - full.0) * zoom);
                let (x0, x1) = if x1 > x0 { (x0, x1) } else { (full.0, full.1) };
                let (left, right) = (40.0f32, 840.0f32);
                let to_px = |v: f64| left + ((v - x0) / (x1 - x0)) as f32 * (right - left);
                let (lo, hi) = l.y_range;
                let to_py = |v: f64| (90.0 - ((v - lo) / (hi - lo).max(1e-9)) as f32 * 80.0).clamp(10.0, 90.0);
                let i0 = xs.partition_point(|&v| v < x0);
                let i1 = xs.partition_point(|&v| v <= x1);
                let from = i0.saturating_sub(1);
                let what = format!("{n} points, zoom {zoom}");
                assert_eq!(
                    l.steps(from, i1, (left, right), to_px, to_py),
                    steps_by_point(&l, from, i1, (left, right), to_px, to_py),
                    "{what}"
                );
                let mut ticks = Vec::new();
                for &x in &xs[i0..i1] {
                    let px = to_px(x).floor();
                    if ticks.last() != Some(&px) {
                        ticks.push(px);
                    }
                }
                assert_eq!(l.ticks(i0, i1, to_px), ticks, "{what}");
                let want = ys[from..i1.max(from + 1).min(n)].iter().fold(NONE, |m, &v| join(m, (v, v)));
                assert_eq!(l.min_max(from, i1.max(from + 1)), want, "{what}");
            }
        }
    }

    /// Runs of one value are passed block by block; the bands are the same
    /// as point by point, with long runs, flapping values and a NaN-free
    /// enum; with assumed points they are walked point by point.
    #[test]
    fn bands_pass_runs_whole() {
        let n = 20_000;
        let xs: Vec<f64> = (0..n).map(|i| i as f64 * 0.01).collect();
        // Long runs, then flapping, then long runs again.
        let ys: Vec<f64> = (0..n)
            .map(|i| match i {
                0..5000 => 0.0,
                5000..9000 => 1.0,
                9000..9400 => (i % 2) as f64,
                _ => 2.0,
            })
            .collect();
        let fast = lane(SignalKind::Enum, xs.clone(), ys.clone(), vec![false; n]);
        // The same with one assumed point far out of view: point by point.
        let mut assumed = vec![false; n + 1];
        assumed[n] = true;
        let (mut xs2, mut ys2) = (xs.clone(), ys.clone());
        xs2.push(1e9);
        ys2.push(0.0);
        let slow = lane(SignalKind::Enum, xs2, ys2, assumed);
        for (x0, x1) in [(0.0, 200.0), (40.0, 120.0), (89.0, 95.0), (91.0, 92.0)] {
            let to_px = |v: f64| ((v - x0) / (x1 - x0)) as f32 * 1000.0;
            let i0 = xs.partition_point(|&v| v < x0).saturating_sub(1);
            let i1 = xs.partition_point(|&v| v <= x1);
            let a = fast.bands((0.0, 1000.0), i0, i1, to_px);
            let b = slow.bands((0.0, 1000.0), i0, i1, to_px);
            assert!(!a.is_empty());
            assert_eq!(a, b, "{x0}..{x1}");
        }
    }

    /// Rows before the clock's first time have no x and are left out; a
    /// clock that steps back keeps the points sorted by x, ties in row
    /// order.
    #[test]
    fn lanes_sort_by_time_and_leave_out_rows_without_one() {
        let text = "v=1\n00:00:05 v=2\n00:00:03 v=3\n00:00:04 v=4\n00:00:04 v=5\n";
        let file = crate::TextFile::from_bytes("t", text.as_bytes().to_vec());
        let mut s = crate::Signals::default();
        s.set(SignalDef::parse_spec(r"t:time=/^(?<t>\d\d:\d\d:\d\d)/").unwrap());
        s.set(SignalDef::parse_spec(r"v=/v=(?<v>\d+)/").unwrap());
        s.collect(&file, &crate::Progress::new()).unwrap();
        let clock = Clock::of(&file, &s);
        assert!(matches!(clock, Clock::Signal(_)));
        let t = s.get("t").unwrap().cached_series(&file).unwrap();
        let v = s.get("v").unwrap();
        let l = lanes("v", v, &v.cached_series(&file).unwrap(), &file, &clock, Some(&t)).remove(0);
        assert_eq!(l.xs, [3.0, 4.0, 4.0, 5.0]);
        assert_eq!(l.ys, [3.0, 4.0, 5.0, 2.0]);
        assert_eq!(l.rows, [2, 3, 4, 1]);
        assert_eq!(l.y_range, (2.0, 5.0));
    }

    /// A signal bound to a lifecycle of two ECUs: values changing within a
    /// pixel are one grey band, not the colour of the first of them (bug,
    /// owner, 2026-11-07).
    #[test]
    fn values_changing_within_a_pixel_are_mixed() {
        // a, b, a, b at x 0.0, 0.1, 0.2, 0.3, then a from 1 to 5, b from 5.
        let l = lane(
            SignalKind::Enum,
            vec![0.0, 0.1, 0.2, 0.3, 1.0, 5.0],
            vec![0.0, 1.0, 0.0, 1.0, 0.0, 1.0],
            vec![false, false, false, false, true, false],
        );
        // One pixel per x unit.
        let got = l.bands((0.0, 10.0), 0, l.xs.len(), |x| x as f32);
        assert_eq!(
            got,
            [
                Band::Mixed { from: 0.0, to: 1.0 },
                Band::Value { from: 1.0, to: 5.0, y: 0.0, assumed: true },
                Band::Value { from: 5.0, to: 10.0, y: 1.0, assumed: false },
            ]
        );
        // Zoomed in, each is its own band.
        let got = l.bands((0.0, 10.0), 0, 4, |x| x as f32 * 100.0);
        assert!(got.iter().all(|b| matches!(b, Band::Value { .. })), "{got:?}");
        // The same without the assumed point: runs passed whole.
        let l = lane(SignalKind::Enum, vec![0.0, 0.1, 0.2, 0.3, 5.0], vec![0.0, 1.0, 0.0, 1.0, 1.0], vec![false; 5]);
        let got = l.bands((0.0, 10.0), 0, l.xs.len(), |x| x as f32);
        assert_eq!(
            got,
            [Band::Mixed { from: 0.0, to: 1.0 }, Band::Value { from: 0.3, to: 10.0, y: 1.0, assumed: false }]
        );
    }
}
