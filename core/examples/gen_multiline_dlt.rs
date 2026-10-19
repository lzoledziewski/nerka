//! Generate a small DLT file whose payloads hold text split by `\n`,
//! `\r\n` (and a few lone `\r` and tabs), mixed with one-line messages,
//! to check how multi-line payloads are shown and copied.
//!
//!     cargo run -p nerka-core --example gen_multiline_dlt -- multiline.dlt

use nerka_core::Level;
use nerka_core::input::dlt::writer::{Arg, Msg, Value};

fn text(s: &str) -> Arg {
    Value::Str(s.into()).into()
}

fn main() -> std::io::Result<()> {
    let path = std::env::args().nth(1).unwrap_or_else(|| "multiline.dlt".into());
    let msgs: Vec<(Level, &str, &str, Vec<Arg>)> = vec![
        (Level::Info, "SYS", "BOOT", vec![text("system start")]),
        (Level::Info, "NAV", "POS", vec![text("LF only:\nline two\nline three")]),
        (Level::Warn, "DIAG", "UDS", vec![text("CRLF only:\r\nline two\r\nline three\r\n")]),
        (Level::Info, "SYS", "PWR", vec![text("voltage ok"), Value::U(12_400, 4).into()]),
        (
            Level::Error,
            "NAV",
            "CRSH",
            vec![text(
                "panic in route planner\r\nstack trace:\n\t#0 planner::step (planner.rs:120)\r\n\t#1 planner::run (planner.rs:88)\n\t#2 main (main.rs:12)",
            )],
        ),
        (Level::Info, "COM", "TCP", vec![text("mixed: LF\nthen CRLF\r\nthen lone CR\rthen end")]),
        (
            Level::Debug,
            "MEDI",
            "PLAY",
            vec![text("first arg\nwith a break"), Value::U(42, 2).into(), text("second arg\r\nwith a break")],
        ),
        (Level::Info, "CAN", "RX", vec![text("frame received id 0x1A0")]),
        (
            Level::Warn,
            "COM",
            "JSON",
            vec![text("{\r\n  \"state\": \"degraded\",\r\n  \"retries\": 3,\r\n  \"peers\": [\"gw\", \"ecu2\"]\r\n}")],
        ),
        (Level::Info, "DIAG", "UDS", vec![text("blank lines:\n\n\nafter three breaks\r\n\r\nand two CRLF")]),
        (Level::Info, "SYS", "BOOT", vec![text("system ready")]),
    ];
    // Long lines split by both kinds of break: they wrap only with Wrap on.
    let long = |k: &str| (0..60).map(|i| format!("{k}{i}=value{i}")).collect::<Vec<_>>().join(" ");
    let mut msgs = msgs;
    let config = format!("config dump:\r\n{}\n{}\r\nend of dump", long("key"), long("opt"));
    msgs.push((Level::Info, "SYS", "CONF", vec![text(&config)]));
    let mut buf = Vec::new();
    for (i, (level, apid, ctid, args)) in msgs.into_iter().enumerate() {
        let mut m = Msg::log(level, "ECU1", apid, ctid, args);
        m.secs += i as u32;
        m.usecs = 250_000 * (i as i32 % 4);
        m.timestamp = Some(10_000 * i as u32 + 1_234);
        m.mcnt = i as u8;
        m.encode(&mut buf);
    }
    std::fs::write(&path, &buf)?;
    println!("wrote {path} ({} bytes)", buf.len());
    Ok(())
}
