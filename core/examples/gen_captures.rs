//! Write two small synthetic captures (pcap, Ethernet) to try protocols in
//! captures with:
//!
//! - `syslog.pcap`: 400 syslog messages over UDP 514, RFC 5424 (some with
//!   structured data) and RFC 3164 by turns, every 10th over IPv6; four
//!   hosts / apps, severities from err to debug.
//! - `dlt-someip.pcap`: two minutes of DLT (UDP 3490: three ECUs, four
//!   applications, levels error to debug) and SOME/IP: service discovery
//!   (UDP 30490, OfferService of service 0x1234) and services on UDP 30501,
//!   not a default port (`--someip-port 30501`, or ⚙ → Captures: UDP ports):
//!   speed notifications (0x1234 / 0x8001), request / response pairs
//!   (0x1234 / 0x0001, about one in ten an ERROR with E_TIMEOUT) and
//!   fire-and-forget calls (0x5678 / 0x0002).
//!
//! Write them, then read one:
//!
//!     cargo run -p nerka-core --example gen_captures -- OUT_DIR
//!     cargo run -p nerka-cli -- --someip-port 30490 --someip-port 30501 filter 'proto == someip AND type == ERROR' OUT_DIR/dlt-someip.pcap

use nerka_core::Level;
use nerka_core::input::dlt::writer::{Msg, Value};
use nerka_core::input::proto::someip::encode as someip;
use std::io::{BufWriter, Write};
use std::path::Path;

/// 2025-10-18 02:00:00 UTC.
const T0: u32 = 1_760_752_800;

/// A fixed-seed xorshift: the same files every run.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    /// In `0..n`.
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }

    /// In `[0, 1)`.
    fn unit(&mut self) -> f64 {
        (self.next() >> 11) as f64 / (1u64 << 53) as f64
    }

    fn pick<'a, T>(&mut self, items: &'a [T]) -> &'a T {
        &items[self.below(items.len())]
    }
}

/// A pcap file of Ethernet frames carrying UDP datagrams.
struct Pcap {
    out: BufWriter<std::fs::File>,
    ip_id: u16,
}

impl Pcap {
    fn create(path: &Path) -> std::io::Result<Pcap> {
        let mut out = BufWriter::new(std::fs::File::create(path)?);
        out.write_all(&0xA1B2_C3D4u32.to_le_bytes())?;
        for x in [2u16, 4] {
            out.write_all(&x.to_le_bytes())?;
        }
        for x in [0u32, 0, 65_535, 1] {
            out.write_all(&x.to_le_bytes())?;
        }
        Ok(Pcap { out, ip_id: 0 })
    }

    /// A datagram to `port` at `t` seconds after [`T0`], from host `src`
    /// (10.0.0.`src`, or `fd00::src` over IPv6) to host 200.
    fn udp(&mut self, t: f64, port: u16, payload: &[u8], v6: bool, src: u8) -> std::io::Result<()> {
        let mut udp = Vec::with_capacity(8 + payload.len());
        udp.extend_from_slice(&(40_000 + u16::from(src)).to_be_bytes());
        udp.extend_from_slice(&port.to_be_bytes());
        udp.extend_from_slice(&(8 + payload.len() as u16).to_be_bytes());
        udp.extend_from_slice(&[0, 0]);
        udp.extend_from_slice(payload);
        let mut frame = vec![0x02; 6];
        frame.extend_from_slice(&[0x04; 6]);
        if v6 {
            frame.extend_from_slice(&[0x86, 0xdd, 0x60, 0, 0, 0]);
            frame.extend_from_slice(&(udp.len() as u16).to_be_bytes());
            frame.extend_from_slice(&[17, 64]);
            for host in [src, 200] {
                frame.extend_from_slice(&[0xfd, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, host]);
            }
        } else {
            self.ip_id = self.ip_id.wrapping_add(1);
            frame.extend_from_slice(&[0x08, 0x00, 0x45, 0]);
            frame.extend_from_slice(&(20 + udp.len() as u16).to_be_bytes());
            frame.extend_from_slice(&self.ip_id.to_be_bytes());
            frame.extend_from_slice(&[0, 0, 64, 17, 0, 0, 10, 0, 0, src, 10, 0, 0, 200]);
        }
        frame.extend_from_slice(&udp);
        let secs = t as u32;
        let usecs = ((t - f64::from(secs)) * 1e6) as u32;
        for x in [T0 + secs, usecs, frame.len() as u32, frame.len() as u32] {
            self.out.write_all(&x.to_le_bytes())?;
        }
        self.out.write_all(&frame)
    }
}

fn syslog(path: &Path) -> std::io::Result<()> {
    let mut p = Pcap::create(path)?;
    let mut rng = Rng(0x5EED_0F51_5106);
    // (host, app, pid, facility, messages with {} for a number)
    let apps: [(&str, &str, u32, u8, [&str; 3]); 4] = [
        ("gw", "routed", 1201, 3, ["route to 192.168.{} added", "link eth{} up", "link eth{} down, rerouting"]),
        (
            "gw",
            "sshd",
            777,
            4,
            [
                "Accepted publickey for root from 10.0.0.{}",
                "Failed password for admin from 10.0.0.{}",
                "session closed for user {}",
            ],
        ),
        ("ivi", "audio-mgr", 4321, 1, ["stream {} started", "xrun on stream {}", "volume set to {}"]),
        ("tcu", "modem", 88, 3, ["signal -{} dBm", "registration lost, retry {}", "registered on cell {}"]),
    ];
    let mut t = 0.0;
    for i in 0..400 {
        t += 0.05 + 0.55 * rng.unit();
        let k = rng.below(apps.len());
        let (host, app, pid, facility, texts) = apps[k];
        let severity = *rng.pick(&[3u8, 4, 4, 5, 5, 6, 6, 6, 6, 6, 6, 7, 7, 7]);
        let text = rng.pick(&texts).replace("{}", &(1 + rng.below(250)).to_string());
        let pri = facility * 8 + severity;
        let (m, s, ms) = ((t as u32) / 60 % 60, t as u32 % 60, ((t % 1.0) * 1000.0) as u32);
        let msg = if i % 2 == 0 {
            let sd = if i % 6 == 0 { format!("[origin ip=\"10.0.0.{}\"]", k + 1) } else { "-".into() };
            format!("<{pri}>1 2025-10-18T02:{m:02}:{s:02}.{ms:03}Z {host} {app} {pid} ID{severity} {sd} {text}")
        } else {
            format!("<{pri}>Oct 18 02:{m:02}:{s:02} {host} {app}[{pid}]: {text}")
        };
        p.udp(t, 514, msg.as_bytes(), i % 10 == 9, k as u8 + 1)?;
    }
    p.out.flush()
}

fn dlt_someip(path: &Path) -> std::io::Result<()> {
    let mut p = Pcap::create(path)?;
    let mut rng = Rng(0x0D17_503E_1F00);
    let levels = [
        (Level::Error, "failed"),
        (Level::Warn, "slow"),
        (Level::Info, "ok"),
        (Level::Info, "ok"),
        (Level::Info, "ok"),
        (Level::Debug, "detail"),
    ];
    let senders = [("ECU1", "NAV"), ("ECU1", "POS"), ("GW", "COM"), ("ECU2", "HMI")];
    let (mut t, mut mcnt, mut session) = (0.0f64, 0u8, 0u16);
    let mut buf = Vec::new();
    while t < 120.0 {
        t += 0.01 + 0.14 * rng.unit();
        let r = rng.unit();
        if r < 0.5 {
            let (level, word) = *rng.pick(&levels);
            let (ecu, apid) = *rng.pick(&senders);
            mcnt = mcnt.wrapping_add(1);
            let text = format!("{} step {} {word}", apid.to_lowercase(), (t * 10.0) as u32);
            let mut m = Msg::log(level, ecu, apid, "CTX", vec![Value::Str(text).into()]);
            m.mcnt = mcnt;
            m.timestamp = Some((t * 10_000.0) as u32);
            buf.clear();
            m.encode(&mut buf);
            // Without the storage header: the capture's time is the message's.
            p.udp(t, 3490, &buf[16..], false, 2)?;
            continue;
        }
        session = session.wrapping_add(1);
        if r < 0.55 {
            // Service discovery: flags, entries (an OfferService of 0x1234), no options.
            let mut entry = vec![0x01, 0, 0, 0x10];
            entry.extend_from_slice(&0x1234u16.to_be_bytes());
            entry.extend_from_slice(&1u16.to_be_bytes());
            entry.extend_from_slice(&[1, 0, 0, 3, 0, 0, 0, 0]);
            let mut sd = vec![0xc0, 0, 0, 0];
            sd.extend_from_slice(&(entry.len() as u32).to_be_bytes());
            sd.extend_from_slice(&entry);
            sd.extend_from_slice(&0u32.to_be_bytes());
            p.udp(t, 30490, &someip(0xFFFF, 0x8100, 0, session, 0x02, 0, &sd), false, 3)?;
            continue;
        }
        let kind = rng.unit();
        if kind < 0.45 {
            let speed = 50 + rng.below(40) as u16;
            p.udp(t, 30501, &someip(0x1234, 0x8001, 0, session, 0x02, 0, &speed.to_be_bytes()), false, 3)?;
        } else if kind < 0.9 {
            p.udp(t, 30501, &someip(0x1234, 0x0001, 0x0010, session, 0x00, 0, &[1]), false, 4)?;
            let response = if rng.unit() < 0.1 {
                someip(0x1234, 0x0001, 0x0010, session, 0x81, 0x06, &[])
            } else {
                someip(0x1234, 0x0001, 0x0010, session, 0x80, 0, &[0x00, 0x2a])
            };
            p.udp(t + 0.002, 30501, &response, false, 3)?;
        } else {
            p.udp(t, 30501, &someip(0x5678, 0x0002, 0x0020, session, 0x01, 0, &[0xde, 0xad]), false, 4)?;
        }
    }
    p.out.flush()
}

fn main() -> std::io::Result<()> {
    let Some(dir) = std::env::args().nth(1) else {
        eprintln!("usage: gen_captures OUT_DIR");
        std::process::exit(2);
    };
    let dir = Path::new(&dir);
    std::fs::create_dir_all(dir)?;
    syslog(&dir.join("syslog.pcap"))?;
    dlt_someip(&dir.join("dlt-someip.pcap"))?;
    println!("wrote {} and {}", dir.join("syslog.pcap").display(), dir.join("dlt-someip.pcap").display());
    Ok(())
}
