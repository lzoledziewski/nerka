//! Time what a plot of FIBEX argument signals costs on a big file, step by
//! step as the GUI does it: open (index), filters, each signal's events
//! (`SignalDef::series`, one after another as the GUI's background job),
//! building the plot's lanes (a point per event, x from the clock) and one
//! frame's walk over the lanes at full zoom (the per-pixel decimation).
//!
//!     cargo run --release -p nerka-core --example perf_signals -- big.pcap big.xml 30 [--unpack-dir DIR] [--par]
//!
//! Signals: the first N numeric arguments of the catalog, as the Signals
//! panel picks them (`mid == M AND ecu == "E"`, argument i). `--par`
//! collects them on several threads at once (what a batched collection
//! could gain). Prints a table per step; numbers are wall-clock.

use nerka_core::signal::{Clock, SignalDef, Signals};
use nerka_core::{Fibex, Filter, OpenOptions, Progress, Query, ValueRef, search};
use rayon::prelude::*;
use std::sync::Arc;
use std::time::Instant;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut a = std::env::args().skip(1);
    let usage = "usage: perf_signals FILE FIBEX [N] [--unpack-dir DIR] [--par] [--skip-filters] [--reopen]";
    let file = a.next().ok_or(usage)?;
    let fibex = a.next().ok_or(usage)?;
    let mut n = 30usize;
    let (mut unpack, mut par, mut filters, mut reopen) = (None, false, true, false);
    while let Some(x) = a.next() {
        match x.as_str() {
            "--unpack-dir" => unpack = a.next(),
            "--par" => par = true,
            "--skip-filters" => filters = false,
            "--reopen" => reopen = true,
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
    let series: Vec<_> = if par {
        defs.par_iter().map(|d| d.series(&*src, &p)).collect::<Result<_, _>>()?
    } else {
        let mut v = Vec::new();
        for d in &defs {
            let t = Instant::now();
            let s = d.series(&*src, &p)?;
            println!("series\t{:.1} ms\t{} events\t{}", ms(t), s.len(), d.name());
            v.push(s);
        }
        v
    };
    let events: usize = series.iter().map(|s| s.len()).sum();
    println!(
        "series total\t{:.1} ms\t{events} events\t{} signals{}\t{} MiB kept (16 B/event)",
        ms(t),
        defs.len(),
        if par { " in parallel" } else { "" },
        (events * 16) >> 20
    );

    // The plot's lanes: a point per event, x from the clock (ui/src/plot.rs `build`).
    let clock = Clock::of(&*src, &signals);
    let t = Instant::now();
    let mut lanes = Vec::new();
    for s in &series {
        let mut pts: Vec<(f64, f64, u64, bool)> = Vec::new();
        for (row, v) in s.iter() {
            if let Some(x) = clock.at(&*src, None, row) {
                let y = match v {
                    ValueRef::Num(n) => n,
                    ValueRef::Enum(_) => 0.0,
                };
                pts.push((x, y, row, false));
            }
        }
        if pts.windows(2).any(|w| w[1].0 < w[0].0) {
            pts.sort_by(|a, b| a.0.total_cmp(&b.0));
        }
        let xs: Vec<f64> = pts.iter().map(|p| p.0).collect();
        let ys: Vec<f64> = pts.iter().map(|p| p.1).collect();
        let rs: Vec<u64> = pts.iter().map(|p| p.2).collect();
        let assumed: Vec<bool> = pts.iter().map(|p| p.3).collect();
        lanes.push((xs, ys, rs, assumed));
    }
    println!("plot build\t{:.1} ms\t{} MiB of lanes (25 B/point)", ms(t), (events * 25) >> 20);

    // One frame at full zoom: every point of every lane walked once to
    // decimate it to pixel columns (ui/src/plot.rs `draw_steps`).
    let width = 1600.0f64;
    for _ in 0..2 {
        let t = Instant::now();
        let mut segs = 0usize;
        for (xs, ys, _, _) in &lanes {
            if xs.is_empty() {
                continue;
            }
            let (x0, x1) = (xs[0], xs[xs.len() - 1]);
            let (lo, hi) = ys.iter().fold((f64::INFINITY, f64::NEG_INFINITY), |(l, h), &y| (l.min(y), h.max(y)));
            let (mut col, mut l, mut h) = (f32::NAN, f32::NAN, f32::NAN);
            for i in 0..xs.len() {
                let px = (((xs[i] - x0) / (x1 - x0).max(1e-9)) * width).floor() as f32;
                let py = (((ys[i] - lo) / (hi - lo).max(1e-9)) * 80.0) as f32;
                if px == col {
                    (l, h) = (l.min(py), h.max(py));
                    continue;
                }
                if h - l >= 1.0 {
                    segs += 1;
                }
                segs += 2;
                (col, l, h) = (px, py, py);
            }
        }
        println!("frame (full zoom)\t{:.1} ms\t{segs} segments", ms(t));
    }
    Ok(())
}

fn ms(t: Instant) -> f64 {
    t.elapsed().as_secs_f64() * 1000.0
}
