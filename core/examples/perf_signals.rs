//! Time what a plot of FIBEX argument signals costs on a big file, step by
//! step as the GUI does it: open (index), filters, the signals' events
//! (`Signals::collect`, as the GUI's background job; `--one-by-one`: each
//! signal's own `SignalDef::series`),
//! building the plot's lanes (a point per event, x from the clock) and one
//! frame's walk over the lanes at full zoom (the per-pixel decimation).
//!
//!     cargo run --release -p nerka-core --example perf_signals -- big.pcap big.xml 30 [--unpack-dir DIR] [--par]
//!
//! Signals: the first N numeric arguments of the catalog, as the Signals
//! panel picks them (`mid == M AND ecu == "E"`, argument i). `--par`
//! collects them on several threads at once (what a batched collection
//! could gain). Prints a table per step; numbers are wall-clock.

use nerka_core::plot::lane::lanes_of_all;
use nerka_core::signal::{Clock, SignalDef, Signals};
use nerka_core::{Fibex, Filter, OpenOptions, Progress, Query, search};
use rayon::prelude::*;
use std::sync::Arc;
use std::time::Instant;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut a = std::env::args().skip(1);
    let usage =
        "usage: perf_signals FILE FIBEX [N] [--unpack-dir DIR] [--par] [--skip-filters] [--reopen] [--one-by-one]";
    let file = a.next().ok_or(usage)?;
    let fibex = a.next().ok_or(usage)?;
    let mut n = 30usize;
    let (mut unpack, mut par, mut filters, mut reopen, mut one_by_one) = (None, false, true, false, false);
    while let Some(x) = a.next() {
        match x.as_str() {
            "--unpack-dir" => unpack = a.next(),
            "--par" => par = true,
            "--skip-filters" => filters = false,
            "--reopen" => reopen = true,
            "--one-by-one" => one_by_one = true,
            s => n = s.parse()?,
        }
    }

    let t = Instant::now();
    let cat = Arc::new(Fibex::load(&[std::path::PathBuf::from(&fibex)])?);
    println!("fibex load\t{:.1} ms\t{} messages", ms(t), cat.len());
    let mut options = OpenOptions { fibex: Some(cat.clone()), ..Default::default() };
    options.capture.unpack.dir = unpack.map(Into::into).or_else(nerka_core::cache_dir);
    let p = Progress::new();
    let t = Instant::now();
    let src = nerka_core::open_with(&file, &options, &p)?;
    let rows = src.line_count();
    println!("open\t{:.1} ms\t{rows} rows\t{}", ms(t), src.format());
    if reopen {
        // Again, with the file's pages already in this process.
        let t = Instant::now();
        let again = nerka_core::open_with(&file, &options, &p)?;
        println!("open again\t{:.1} ms\t{} rows", ms(t), again.line_count());
    }

    if filters {
        for q in [
            "mid == 1000",
            "mid == 1039 AND ecu == \"ECU1\"",
            "apid == ENG AND level >= WARN",
            "ecu == ECU2",
            "payload ~ /message 3:/",
        ] {
            let f = Filter::Query(Query::compile_for(q, src.schema())?);
            let t = Instant::now();
            let r = search(&*src, &f, &p)?;
            println!("filter\t{:.1} ms\t{} hits\t{q}", ms(t), r.lines.len());
        }
    }

    // The first N numeric arguments, as the panel makes them.
    // By message id: with gen_fibex_dlt the lowest ids are the most frequent.
    let mut args: Vec<_> = cat.arguments().into_iter().filter(|a| a.ty.kind() == nerka_core::SignalKind::Num).collect();
    args.sort_by_key(|a| (a.mid, a.index));
    args.truncate(n);
    let mut signals = Signals::default().for_source(&*src);
    let mut defs = Vec::new();
    for a in &args {
        let q = match &a.ecu {
            Some(e) => format!("mid == {} AND ecu == \"{e}\"", a.mid),
            None => format!("mid == {}", a.mid),
        };
        let d = SignalDef::argument(&a.name, a.ty.kind(), &q, a.index, "bench")?;
        signals.set(d);
        defs.push(signals.get(&a.name.to_ascii_lowercase()).expect("just set").clone());
    }

    // Events.
    let t = Instant::now();
    let how = if par {
        defs.par_iter().map(|d| d.series(&*src, &p).map(|_| ())).collect::<Result<(), _>>()?;
        "each in parallel"
    } else if one_by_one {
        for d in &defs {
            let t = Instant::now();
            let s = d.series(&*src, &p)?;
            println!("series\t{:.1} ms\t{} events\t{}", ms(t), s.len(), d.name());
        }
        "one by one"
    } else {
        // As the GUI collects a tab's signals.
        signals.collect(&*src, &p)?;
        "together"
    };
    let series: Vec<_> = defs.iter().map(|d| d.series(&*src, &p)).collect::<Result<_, _>>()?;
    let events: usize = series.iter().map(|s| s.len()).sum();
    println!(
        "series total\t{:.1} ms\t{events} events\t{} signals {how}\t{} MiB kept (16 B/event)",
        ms(t),
        defs.len(),
        (events * 16) >> 20
    );

    // The plot's lanes (`nerka_core::plot::lane`, as the GUI builds them).
    let clock = Clock::of(&*src, &signals);
    let t = Instant::now();
    let todo: Vec<(&str, &SignalDef, &nerka_core::SignalSeries)> =
        defs.iter().zip(&series).map(|(d, s)| (d.name(), &**d, s)).collect();
    let lanes: Vec<_> = lanes_of_all(&todo, &*src, &clock, None).into_iter().flatten().collect();
    println!("plot build\t{:.1} ms\t{} MiB of lanes (25 B/point)", ms(t), (events * 25) >> 20);

    // One frame: each lane's steps across 1600 pixels, at full zoom and
    // zoomed in (to 1 % in the middle).
    let (left, right) = (0.0f32, 1600.0f32);
    for zoom in [1.0, 1.0, 0.01] {
        let t = Instant::now();
        let (mut strokes, mut points) = (0, 0);
        for l in lanes.iter().filter(|l| !l.xs.is_empty()) {
            let (a, b) = (l.xs[0], l.xs[l.xs.len() - 1]);
            let mid = (a + b) / 2.0;
            let (x0, x1) = (mid - (mid - a) * zoom, mid + (b - mid) * zoom);
            let i0 = l.xs.partition_point(|&x| x < x0).saturating_sub(1);
            let i1 = l.xs.partition_point(|&x| x <= x1);
            let (lo, hi) = l.y_range;
            let to_px = |x: f64| left + ((x - x0) / (x1 - x0).max(1e-9)) as f32 * (right - left);
            let to_py = |y: f64| 90.0 - ((y - lo) / (hi - lo).max(1e-9)) as f32 * 80.0;
            let s = l.steps(i0, i1, (left, right), to_px, to_py);
            strokes += s.strokes.len();
            points += s.line.len();
        }
        println!("frame (zoom {zoom})\t{:.1} ms\t{strokes} strokes, {points} line points", ms(t));
    }
    Ok(())
}

fn ms(t: Instant) -> f64 {
    t.elapsed().as_secs_f64() * 1000.0
}
