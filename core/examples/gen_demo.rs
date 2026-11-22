//! Generate the demo trace the user guide's screenshots are made from: a
//! synthetic 20-minute drive as a DLT file. Three ECUs: ECU1 (navigation,
//! gear, over-the-air update) boots three times (a planned shutdown, a
//! crash, still running at the end); ECU2 reports its temperature and
//! throttles above 90 °C; the gateway reports the battery voltage (dips at
//! each crank) and network trouble. Pack: `examples/packs/demo.nerka-pack`.
//! A few bookmarks with notes go next to it (`<path>.nerka-marks`).
//!
//!     cargo run -p nerka-core --example gen_demo -- demo.dlt
//!
//! The data is made up and the same on every run.

use nerka_core::Level;
use nerka_core::input::dlt::writer::{Msg, Value};
use std::io::{BufWriter, Write};

/// The drive starts at 2025-10-18 10:00:00 UTC.
const START: u64 = 1_760_781_600;
/// Seconds of the drive.
const LEN: f64 = 1200.0;
/// ECU1's boots: start and end (s), and how each ends.
const BOOTS: [(f64, f64, End); 3] = [(0.0, 400.0, End::Planned), (406.0, 780.0, End::Crash), (790.0, LEN, End::Open)];

#[derive(Clone, Copy, PartialEq)]
enum End {
    Planned,
    Crash,
    Open,
}

struct Line {
    /// Seconds since the start.
    t: f64,
    ecu: &'static str,
    apid: &'static str,
    ctid: &'static str,
    level: Level,
    text: String,
}

/// A small deterministic random source.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    /// 0.0 ≤ x < 1.0
    fn unit(&mut self) -> f64 {
        (self.next() >> 11) as f64 / (1u64 << 53) as f64
    }
}

/// The speed (km/h) `s` seconds into a boot that lasts `len` seconds:
/// out of town, a stop at the lights, the motorway, slowing down at the end.
fn speed(s: f64, len: f64) -> f64 {
    let ramp = |x: f64, a: f64, b: f64| ((x - a) / (b - a)).clamp(0.0, 1.0);
    let u = s / len;
    let town = 50.0 * ramp(u, 0.03, 0.10) * (1.0 - ramp(u, 0.20, 0.24));
    let road = 85.0 * ramp(u, 0.27, 0.33) * (1.0 - ramp(u, 0.45, 0.48));
    let motorway = 128.0 * ramp(u, 0.48, 0.58) * (1.0 - ramp(u, 0.86, 0.95));
    let wobble = 3.0 * (s / 7.0).sin() * ramp(u, 0.05, 0.1);
    (town + road + motorway + wobble).max(0.0)
}

fn main() -> std::io::Result<()> {
    let path = std::env::args().nth(1).unwrap_or_else(|| "demo.dlt".into());
    let mut rng = Rng(0x2545_F491_4F6C_DD1D);
    let mut lines: Vec<Line> = Vec::new();
    let mut add = |t: f64, ecu, apid, ctid, level, text: String| lines.push(Line { t, ecu, apid, ctid, level, text });

    // ECU1: navigation, gear and the update, per boot.
    for (b, &(from, to, end)) in BOOTS.iter().enumerate() {
        let len = to - from;
        add(
            from + 0.2,
            "ECU1",
            "SYS",
            "PWR",
            Level::Info,
            format!("boot #{} started, reason={}", b + 1, ["cold", "warm", "watchdog"][b]),
        );
        let mut gear = "P";
        add(from + 0.4, "ECU1", "NAV", "GEAR", Level::Info, "gear=P".into());
        let mut t = from + 1.0;
        while t < to - 0.1 {
            let v = speed(t - from, len);
            add(t, "ECU1", "NAV", "GPS", Level::Info, format!("speed={v:.0} km/h heading={}", (t * 3.0) as u32 % 360));
            let want = if v > 1.0 {
                "D"
            } else if t - from < len * 0.5 {
                "N"
            } else {
                "P"
            };
            if want != gear && (want != "P" || t > to - 15.0) {
                gear = want;
                add(t + 0.05, "ECU1", "NAV", "GEAR", Level::Info, format!("gear={gear}"));
            }
            if rng.unit() < 0.004 {
                let sats = 2 + rng.next() % 3;
                add(t + 0.2, "ECU1", "NAV", "GPS", Level::Warn, format!("GPS fix lost, {sats} satellites"));
            }
            t += 0.5;
        }
        // The update: downloaded and verified in the first boot, installed
        // at its planned shutdown, reported done after the third boot.
        let ota: &[(f64, &str)] = match b {
            0 => &[(5.0, "idle"), (95.0, "downloading"), (270.0, "verifying"), (330.0, "ready"), (392.0, "installing")],
            1 => &[(5.0, "idle")],
            _ => &[(5.0, "idle"), (40.0, "reporting"), (70.0, "done")],
        };
        for &(s, state) in ota {
            add(from + s, "ECU1", "OTA", "JOB", Level::Info, format!("OTA state={state}"));
        }
        match end {
            End::Planned => {
                add(to - 1.0, "ECU1", "SYS", "PWR", Level::Info, "shutdown requested: update install".into());
            }
            End::Crash => {
                add(to - 0.6, "ECU1", "NAV", "TASK", Level::Error, "route service not responding (2000 ms)".into());
                add(to - 0.1, "ECU1", "NAV", "TASK", Level::Fatal, "panic in NAV task: index out of bounds".into());
            }
            End::Open => {}
        }
    }

    // ECU2: temperature, throttling above 90 °C.
    let mut temp: f64 = 41.0;
    let mut throttling = false;
    let mut t = 0.5;
    while t < LEN {
        let ecu1_speed = BOOTS.iter().find(|b| t >= b.0 && t < b.1).map_or(0.0, |b| speed(t - b.0, b.1 - b.0));
        let target = 45.0 + ecu1_speed * 0.42 + if t > 560.0 && t < 760.0 { 18.0 } else { 0.0 };
        temp += (target - temp) * 0.02 + (rng.unit() - 0.5) * 0.4;
        add(
            t,
            "ECU2",
            "THRM",
            "SOC",
            Level::Info,
            format!("temp={temp:.1} C fan={}%", (temp - 40.0).clamp(0.0, 100.0) as u32),
        );
        if temp > 90.0 {
            if !throttling {
                add(t + 0.01, "ECU2", "THRM", "SOC", Level::Warn, format!("thermal throttling engaged at {temp:.1} C"));
            } else if rng.unit() < 0.2 {
                add(
                    t + 0.01,
                    "ECU2",
                    "THRM",
                    "SOC",
                    Level::Warn,
                    format!("thermal throttling active, clock 1.2 GHz ({temp:.1} C)"),
                );
            }
            throttling = true;
        } else if throttling && temp < 86.0 {
            add(t + 0.01, "ECU2", "THRM", "SOC", Level::Info, "thermal throttling released".into());
            throttling = false;
        }
        t += 1.0;
    }

    // The gateway: battery voltage (a dip at each crank), the network.
    let mut t = 0.3;
    let mut session = 100;
    while t < LEN {
        let crank = BOOTS.iter().any(|b| t >= b.0 && t < b.0 + 2.5);
        let mv = if crank { 9800.0 + rng.unit() * 600.0 } else { 13900.0 + (rng.unit() - 0.5) * 120.0 };
        add(t, "GW", "PWR", "BAT", Level::Info, format!("voltage={mv:.0} mV"));
        if mv < 11000.0 {
            add(t + 0.01, "GW", "PWR", "BAT", Level::Warn, format!("undervoltage {mv:.0} mV"));
        }
        for k in 0..4 {
            let at = t + 0.25 * k as f64;
            let id = [0x0C9, 0x1A0, 0x2F3, 0x3E8][(rng.next() % 4) as usize];
            add(at + 0.1, "GW", "CAN", "RX", Level::Debug, format!("frame id=0x{id:03X} dlc=8"));
        }
        if rng.unit() < 0.05 {
            add(t + 0.05, "GW", "COM", "TCP", Level::Warn, format!("retransmit seq={}", rng.next() % 90_000));
        }
        if rng.unit() < 0.06 {
            session += 1;
            let ecu = ["ECU1", "ECU2"][(rng.next() % 2) as usize];
            add(t + 0.07, "GW", "COM", "TCP", Level::Info, format!("session {session} to {ecu} established"));
            if rng.unit() < 0.3 {
                let ms = 200 + rng.next() % 800;
                add(
                    t + 0.6,
                    "GW",
                    "COM",
                    "TCP",
                    Level::Error,
                    format!("session {session} to {ecu} timeout after {ms} ms"),
                );
            }
        }
        if rng.unit() < 0.03 {
            add(t + 0.15, "GW", "DIAG", "UDS", Level::Info, "request 0x22 did=F190 (VIN)".into());
        }
        t += 1.0;
    }

    lines.sort_by(|a, b| a.t.total_cmp(&b.t));
    let mut out = BufWriter::new(std::fs::File::create(&path)?);
    let mut buf = Vec::new();
    for (n, l) in lines.iter().enumerate() {
        let mut m = Msg::log(l.level, l.ecu, l.apid, l.ctid, vec![Value::Str(l.text.clone()).into()]);
        let us = (l.t * 1e6) as u64;
        m.secs = (START + us / 1_000_000) as u32;
        m.usecs = (us % 1_000_000) as i32;
        // Uptime in 0.1 ms: ECU1 since its boot, the others since the start.
        let boot = if l.ecu == "ECU1" { BOOTS.iter().rev().find(|b| l.t >= b.0).map_or(0.0, |b| b.0) } else { 0.0 };
        m.timestamp = Some(((l.t - boot) * 1e4) as u32);
        m.mcnt = n as u8;
        buf.clear();
        m.encode(&mut buf);
        out.write_all(&buf)?;
    }
    out.flush()?;

    // Bookmarks with notes, by line (1-based) and anchored to nothing: nerka
    // finds them again by line.
    let line_of = |pred: &dyn Fn(&Line) -> bool| lines.iter().position(pred).map(|i| i + 1);
    let marks = [
        (line_of(&|l| l.text.starts_with("OTA state=downloading")), "update download starts", "B1"),
        (line_of(&|l| l.text.starts_with("thermal throttling engaged")), "first throttling", "B2"),
        (line_of(&|l| l.text.starts_with("panic")), "NAV crash", "B3"),
    ];
    let mut text = String::new();
    for (line, note, name) in marks {
        if let Some(line) = line {
            text.push_str(&format!("{line}\t{note}\t{name}\n"));
        }
    }
    std::fs::write(format!("{path}.nerka-marks"), text)?;
    println!("wrote {} messages to {path} and 3 bookmarks to {path}.nerka-marks", lines.len());
    Ok(())
}
