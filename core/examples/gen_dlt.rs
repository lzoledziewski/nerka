//! Generate a synthetic DLT file (storage headers, verbose and non-verbose
//! messages, several ECUs) for testing and benchmarking. ECUs reboot now
//! and then (their `tmsp` uptime starts again), some after a
//! "shutdown requested" message: lifecycles to find
//! (`--lifecycle 'boot(ecu): reset @uptime; planned payload ~ /shutdown requested/'`).
//!
//!     cargo run --release -p nerka-core --example gen_dlt -- big.dlt 1024
//!
//! Second argument is the target size in MiB (default 100).

use nerka_core::Level;
use nerka_core::input::dlt::writer::{Arg, Msg, Payload, Value};
use std::io::{BufWriter, Write};

fn main() -> std::io::Result<()> {
    let mut args = std::env::args().skip(1);
    let path = args.next().unwrap_or_else(|| "sample.dlt".into());
    let mib: u64 = args.next().and_then(|s| s.parse().ok()).unwrap_or(100);
    let target = mib * 1024 * 1024;

    let ecus = ["ECU1", "ECU2", "GW"];
    let apps = [("NAV", "POS"), ("DIAG", "UDS"), ("SYS", "PWR"), ("MEDI", "PLAY"), ("CAN", "RX"), ("COM", "TCP")];
    let levels = [Level::Debug, Level::Info, Level::Info, Level::Info, Level::Warn, Level::Error];
    let msgs = [
        "connection established peer",
        "write completed bytes",
        "frame received id",
        "session timeout after ms",
        "frame drawn in us",
        "voltage mV",
    ];

    let mut out = BufWriter::with_capacity(1 << 20, std::fs::File::create(&path)?);
    let mut buf = Vec::with_capacity(512);
    let (mut written, mut count) = (0u64, 0u64);
    let mut rng: u64 = 0x9E37_79B9_7F4A_7C15;
    let mut t_100us: u64 = 0;
    // When each ECU last booted (its uptime counts from there).
    let mut booted = [0u64; 3];
    while written < target {
        rng ^= rng << 13;
        rng ^= rng >> 7;
        rng ^= rng << 17;
        t_100us += rng % 70;
        let (apid, ctid) = apps[((rng >> 8) % apps.len() as u64) as usize];
        let e = ((rng >> 4) % ecus.len() as u64) as usize;
        let ecu = ecus[e];
        let mut level = levels[(rng % levels.len() as u64) as usize];
        // About one reboot per ECU in 600 000 messages, after this message;
        // every other one planned (this message says so).
        let reboot = (rng >> 20) % 200_000 == 7;
        let planned = reboot && (rng >> 44) % 2 == 0;
        if planned {
            level = Level::Info;
        }
        let (a, b) = ((rng >> 16) % 255, (rng >> 24) % 9999);
        let mut m = Msg::log(
            level,
            ecu,
            apid,
            ctid,
            vec![
                Value::Str(
                    if planned { "shutdown requested" } else { msgs[((rng >> 32) % msgs.len() as u64) as usize] }
                        .into(),
                )
                .into(),
                Value::U(a, 4).into(),
                Arg { value: Value::U(b, 4), name: Some(("n".into(), String::new())) },
            ],
        );
        m.secs = 1_760_745_600 + (t_100us / 10_000) as u32;
        m.usecs = (t_100us % 10_000) as i32 * 100;
        m.timestamp = Some((t_100us - booted[e]) as u32);
        m.mcnt = count as u8;
        if rng % 20 == 0 {
            m.payload = Payload::NonVerbose { id: (rng >> 40) as u32 % 1000, data: (0..(a % 32) as u8).collect() };
        }
        buf.clear();
        m.encode(&mut buf);
        out.write_all(&buf)?;
        written += buf.len() as u64;
        if reboot {
            booted[e] = t_100us;
        }
        count += 1;
    }
    out.flush()?;
    println!("wrote {count} messages ({} MiB) to {path}", written >> 20);
    Ok(())
}
