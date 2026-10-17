//! Generate a synthetic log for testing and benchmarking.
//!
//!     cargo run --release -p nerka-core --example gen_log -- big.log 2000
//!
//! Second argument is the target size in MiB (default 100).

use std::io::{BufWriter, Write};

fn main() -> std::io::Result<()> {
    let mut args = std::env::args().skip(1);
    let path = args.next().unwrap_or_else(|| "sample.log".into());
    let mib: u64 = args.next().and_then(|s| s.parse().ok()).unwrap_or(100);
    let target = mib * 1024 * 1024;

    let levels = ["DEBUG", "INFO", "INFO", "INFO", "WARN", "ERROR"];
    let modules = ["net.tcp", "storage.io", "can.bus", "diag.uds", "ui.render", "power.mgr"];
    let msgs = [
        "connection established peer=10.0.%d.%d",
        "write completed bytes=%d latency_us=%d",
        "frame received id=0x%x len=%d",
        "session timeout after %d ms retries=%d",
        "frame drawn in %d us queue=%d",
        "voltage %d mV current %d mA",
    ];

    let mut out = BufWriter::with_capacity(1 << 20, std::fs::File::create(&path)?);
    let mut written = 0u64;
    let mut rng: u64 = 0x9E37_79B9_7F4A_7C15;
    let mut t_ms: u64 = 0;
    let mut i = 0u64;
    while written < target {
        rng ^= rng << 13;
        rng ^= rng >> 7;
        rng ^= rng << 17;
        t_ms += rng % 7;
        let lvl = levels[(rng % levels.len() as u64) as usize];
        let m = ((rng >> 8) % modules.len() as u64) as usize;
        let a = (rng >> 16) % 255;
        let b = (rng >> 24) % 9999;
        let msg = msgs[m].replacen("%d", &a.to_string(), 1).replacen("%x", &format!("{a:x}"), 1).replacen(
            "%d",
            &b.to_string(),
            1,
        );
        let line = format!(
            "2026-10-17 {:02}:{:02}:{:02}.{:03} [{lvl:<5}] {:<11} #{i} {msg}\n",
            (t_ms / 3_600_000) % 24,
            (t_ms / 60_000) % 60,
            (t_ms / 1000) % 60,
            t_ms % 1000,
            modules[m]
        );
        out.write_all(line.as_bytes())?;
        written += line.len() as u64;
        i += 1;
    }
    out.flush()?;
    eprintln!("wrote {i} lines ({} MiB) to {path}", written / 1024 / 1024);
    Ok(())
}
