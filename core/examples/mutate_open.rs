//! Robustness check for the readers: open mutated copies of seed files and
//! touch every line the way a viewer and a filter do. A panic is a bug (the
//! readers take untrusted files); an `Err` from `open` is fine.
//!
//!     cargo run --release -p nerka-core --example mutate_open -- SEEDS_DIR [ROUNDS] [SEED]
//!
//! SEEDS_DIR holds any files nerka opens (`.dlt`, `.pcap`, `.pcapng`, `.mf4`,
//! `.log`, `.json`); `doc/review/README.md` says how to make them. Each round
//! picks a seed and one mutation (flip bytes, overwrite with 0x00 / 0xff,
//! truncate, duplicate a slice, splice two seeds) and opens the result with
//! the default options. Failing inputs are kept as `SEEDS_DIR/crash-*.bin`
//! with the panic message on stderr; the exit status is 1 if any failed.
//! Deterministic for a given SEED (default 1), so a crash can be replayed.

use nerka_core::{FieldId, LogSource, OpenOptions, Progress};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: usize) -> usize {
        if n == 0 { 0 } else { (self.next() % n as u64) as usize }
    }
}

/// Touch what a viewer, a filter and the line window would.
fn exercise(src: &dyn LogSource) {
    let n = src.line_count().min(20_000);
    let schema: Vec<FieldId> = (0..src.schema().len()).collect();
    for l in 0..n {
        let _ = src.line_bytes(l);
        let _ = src.line_text(l);
        let _ = src.line_detail(l);
        let _ = src.supplied_level(l);
        let _ = src.time(l);
        let _ = src.uptime(l);
        let _ = src.anchor(l);
        for &id in &schema {
            let _ = src.field(l, id);
        }
        for i in 0..4 {
            let _ = src.argument(l, i);
        }
    }
    let _ = src.format();
    // A filter over every line: the engine's per-line and fast paths.
    for q in ["level >= WARN", "raw ~ /e/", "NOT raw ~ /^$/"] {
        if let Ok(q) = nerka_core::Query::compile_for(q, src.schema()) {
            let _ = nerka_core::search(src, &nerka_core::Filter::Query(q), &Progress::new());
        }
    }
}

fn mutate(rng: &mut Rng, data: &[u8], other: &[u8]) -> Vec<u8> {
    let mut d = data.to_vec();
    if d.is_empty() {
        return d;
    }
    match rng.below(6) {
        0 => {
            for _ in 0..1 + rng.below(16) {
                let i = rng.below(d.len());
                d[i] ^= 1 << rng.below(8);
            }
        }
        1 => {
            let i = rng.below(d.len());
            let len = (1 + rng.below(16)).min(d.len() - i);
            let v = if rng.below(2) == 0 { 0 } else { 0xff };
            d[i..i + len].fill(v);
        }
        2 => d.truncate(rng.below(d.len())),
        3 => {
            let i = rng.below(d.len());
            let len = rng.below(256).min(d.len() - i);
            let slice = d[i..i + len].to_vec();
            let at = rng.below(d.len());
            d.splice(at..at, slice);
        }
        4 => {
            // Header kept, body from another seed.
            let cut = rng.below(d.len().min(4096));
            d.truncate(cut);
            d.extend_from_slice(&other[rng.below(other.len())..]);
        }
        _ => {
            // Length-like fields: bump a 16 / 32-bit value.
            let i = rng.below(d.len().saturating_sub(4));
            let v = [0xff, 0xff, 0xff, 0x7f];
            let len = (2 + 2 * rng.below(2)).min(d.len() - i);
            d[i..i + len].copy_from_slice(&v[..len]);
        }
    }
    d
}

fn main() {
    let mut args = std::env::args().skip(1);
    let dir = PathBuf::from(args.next().expect("usage: mutate_open SEEDS_DIR [ROUNDS] [SEED]"));
    let rounds: usize = args.next().and_then(|s| s.parse().ok()).unwrap_or(500);
    let mut rng = Rng(args.next().and_then(|s| s.parse().ok()).unwrap_or(1).max(1));
    let seeds: Vec<(PathBuf, Vec<u8>)> = std::fs::read_dir(&dir)
        .expect("read SEEDS_DIR")
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_file() && !p.file_name().is_some_and(|n| n.to_string_lossy().starts_with("crash-")))
        .filter(|p| !p.extension().is_some_and(|e| e == "tmp"))
        .map(|p| {
            let d = std::fs::read(&p).expect("read seed");
            (p, d)
        })
        .collect();
    assert!(!seeds.is_empty(), "no seed files in {}", dir.display());
    std::panic::set_hook(Box::new(|_| {}));
    let tmp = std::env::temp_dir().join(format!("nerka-mutate-{}.tmp", std::process::id()));
    let mut failed = 0;
    for round in 0..rounds {
        let (name, data) = &seeds[rng.below(seeds.len())];
        let other = &seeds[rng.below(seeds.len())].1;
        let bytes = mutate(&mut rng, data, other);
        // Keep the seed's extension: some readers are picked by it.
        let path = tmp.with_extension(name.extension().unwrap_or_default());
        std::fs::write(&path, &bytes).expect("write the mutated file");
        let r = catch_unwind(AssertUnwindSafe(|| {
            if let Ok(src) = nerka_core::open_with(&path, &OpenOptions::default(), &Progress::new()) {
                exercise(&*src);
            }
        }));
        if let Err(p) = r {
            failed += 1;
            let msg = p
                .downcast_ref::<String>()
                .cloned()
                .or_else(|| p.downcast_ref::<&str>().map(|s| s.to_string()))
                .unwrap_or_default();
            let keep = dir.join(format!("crash-{round}-{}.bin", file_name(name)));
            let _ = std::fs::write(&keep, &bytes);
            eprintln!("round {round}: {} -> panic: {msg} (kept {})", name.display(), keep.display());
        }
        let _ = std::fs::remove_file(&path);
    }
    println!("{rounds} rounds over {} seeds, {failed} panics", seeds.len());
    std::process::exit(i32::from(failed > 0));
}

fn file_name(p: &Path) -> String {
    p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
}
