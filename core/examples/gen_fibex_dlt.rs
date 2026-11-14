//! Generate a FIBEX catalog of non-verbose DLT messages and a `.dlt` trace
//! that uses it, for benchmarking FIBEX decoding, argument signals and
//! plots on big files (convert the trace with `dlt_to_pcap` / `dlt_to_mf4`).
//!
//!     cargo run --release -p nerka-core --example gen_fibex_dlt -- big_nv.dlt big_nv.xml 3072 40
//!
//! Arguments: the trace, the FIBEX file, the trace's size in MiB (default
//! 100) and the number of messages the catalog describes (default 40).
//!
//! The trace is about 70 % non-verbose messages of ECU1 without extended
//! header (APID, CTID and level come from the catalog), at rates that fall
//! off with the message id (message 0 is the most frequent, as in real
//! traces where a few cyclic messages dominate), and 30 % verbose text
//! messages of ECU1 and ECU2. Each catalog message has 1 to 4 arguments
//! (u8, u16, u32, i32, f32, and every eighth a string), named
//! `m<k>_<name>` so they are plain signal names.

use nerka_core::Level;
use nerka_core::input::dlt::writer::{Msg, Payload, Value};
use std::fmt::Write as _;
use std::io::{BufWriter, Write};

#[derive(Clone, Copy)]
enum Ty {
    U8,
    U16,
    U32,
    I32,
    F32,
    Str,
}

impl Ty {
    fn coding(self) -> (&'static str, &'static str, u32) {
        match self {
            Ty::U8 => ("C_U8", "A_UINT8", 8),
            Ty::U16 => ("C_U16", "A_UINT16", 16),
            Ty::U32 => ("C_U32", "A_UINT32", 32),
            Ty::I32 => ("C_I32", "A_INT32", 32),
            Ty::F32 => ("C_F32", "A_FLOAT32", 32),
            Ty::Str => ("C_STR", "A_ASCIISTRING", 0),
        }
    }
}

const NAMES: [&str; 12] =
    ["speed", "temp", "voltage", "current", "rpm", "angle", "pressure", "level", "count", "state", "torque", "load"];
const APPS: [(&str, &str); 6] =
    [("NAV", "POS"), ("PWR", "BAT"), ("ENG", "CTL"), ("CHAS", "ESP"), ("BODY", "DOOR"), ("COM", "ETH")];

struct Message {
    id: u32,
    args: Vec<(String, Ty)>,
}

fn main() -> std::io::Result<()> {
    let mut a = std::env::args().skip(1);
    let dlt = a.next().unwrap_or_else(|| "nv.dlt".into());
    let xml = a.next().unwrap_or_else(|| "nv.xml".into());
    let mib: u64 = a.next().and_then(|s| s.parse().ok()).unwrap_or(100);
    let count: usize = a.next().and_then(|s| s.parse().ok()).unwrap_or(40);

    let tys = [Ty::U16, Ty::U8, Ty::F32, Ty::U32, Ty::I32, Ty::U16, Ty::F32];
    let messages: Vec<Message> = (0..count)
        .map(|k| {
            let n = 1 + k % 4;
            let args = (0..n)
                .map(|j| {
                    let ty = if (k + j) % 8 == 7 { Ty::Str } else { tys[(k * 3 + j) % tys.len()] };
                    (format!("m{k}_{}", NAMES[(k + j * 5) % NAMES.len()]), ty)
                })
                .collect();
            Message { id: 1000 + k as u32, args }
        })
        .collect();
    std::fs::write(&xml, fibex(&messages))?;

    // Message k is drawn with weight 1 / (k + 1) (Zipf-like).
    let weights: Vec<f64> = (0..count).map(|k| 1.0 / (k as f64 + 1.0)).collect();
    let total: f64 = weights.iter().sum();
    let mut cdf = Vec::with_capacity(count);
    let mut acc = 0.0;
    for w in &weights {
        acc += w / total;
        cdf.push(acc);
    }

    let target = mib * 1024 * 1024;
    let mut out = BufWriter::with_capacity(1 << 20, std::fs::File::create(&dlt)?);
    let mut buf = Vec::with_capacity(256);
    let (mut written, mut n) = (0u64, 0u64);
    let mut rng: u64 = 0x9E37_79B9_7F4A_7C15;
    let mut t_100us: u64 = 0;
    // Each argument's current value: a slow walk, so plots have shape.
    let mut state: Vec<Vec<f64>> = messages.iter().map(|m| vec![0.0; m.args.len()]).collect();
    let texts = ["connection established peer", "frame received id", "session timeout after ms", "voltage mV"];
    while written < target {
        rng ^= rng << 13;
        rng ^= rng >> 7;
        rng ^= rng << 17;
        t_100us += 1 + rng % 4;
        let mut m;
        if rng % 10 < 7 {
            let u = (rng >> 11) as f64 / (1u64 << 53) as f64;
            let k = cdf.partition_point(|&c| c < u).min(count - 1);
            let msg = &messages[k];
            let mut data = Vec::with_capacity(16);
            for (j, (_, ty)) in msg.args.iter().enumerate() {
                let step = ((rng >> (j * 8 + 3)) % 7) as f64 - 3.0;
                let v = &mut state[k][j];
                *v = (*v + step).clamp(0.0, 250.0);
                match ty {
                    Ty::U8 => data.push(*v as u8),
                    Ty::U16 => data.extend((*v as u16 * 10).to_le_bytes()),
                    Ty::U32 => data.extend((*v as u32 * 1000).to_le_bytes()),
                    Ty::I32 => data.extend((*v as i32 - 125).to_le_bytes()),
                    Ty::F32 => data.extend((*v as f32 * 0.25).to_le_bytes()),
                    Ty::Str => {
                        let s = if *v > 125.0 { "ON" } else { "OFF" };
                        data.extend((s.len() as u16 + 1).to_le_bytes());
                        data.extend(s.as_bytes());
                        data.push(0);
                    }
                }
            }
            m = Msg::log(Level::Info, "ECU1", "", "", vec![]);
            m.ext = None;
            m.payload = Payload::NonVerbose { id: msg.id, data };
        } else {
            let ecu = if rng.is_multiple_of(3) { "ECU2" } else { "ECU1" };
            let (apid, ctid) = APPS[((rng >> 8) % APPS.len() as u64) as usize];
            let level = [Level::Debug, Level::Info, Level::Info, Level::Warn, Level::Error][(rng % 5) as usize];
            m = Msg::log(
                level,
                ecu,
                apid,
                ctid,
                vec![
                    Value::Str(texts[((rng >> 32) % texts.len() as u64) as usize].into()).into(),
                    Value::U((rng >> 16) % 9999, 4).into(),
                ],
            );
        }
        m.secs = 1_760_745_600 + (t_100us / 10_000) as u32;
        m.usecs = (t_100us % 10_000) as i32 * 100;
        m.timestamp = Some(t_100us as u32);
        m.mcnt = n as u8;
        buf.clear();
        m.encode(&mut buf);
        out.write_all(&buf)?;
        written += buf.len() as u64;
        n += 1;
    }
    out.flush()?;
    let args: usize = messages.iter().map(|m| m.args.len()).sum();
    println!("wrote {n} messages ({} MiB) to {dlt}; {count} messages, {args} arguments to {xml}", written >> 20);
    Ok(())
}

/// The catalog in the common convention: one ECU, a frame per message
/// with APID / CTID / level, a PDU per argument (its SIGNAL named after it)
/// and a text PDU in front.
fn fibex(messages: &[Message]) -> String {
    let mut pdus = String::new();
    let mut frames = String::new();
    let mut signals = String::new();
    for (k, m) in messages.iter().enumerate() {
        let (apid, ctid) = APPS[k % APPS.len()];
        let level = ["DLT_LOG_INFO", "DLT_LOG_WARN", "DLT_LOG_DEBUG"][k % 3];
        let mut inst = String::new();
        let _ = write!(
            pdus,
            r#"<fx:PDU ID="P{k}_t"><ho:SHORT-NAME>P{k}_t</ho:SHORT-NAME><ho:DESC>message {k}:</ho:DESC><fx:BYTE-LENGTH>0</fx:BYTE-LENGTH></fx:PDU>"#
        );
        let _ = write!(
            inst,
            r#"<fx:PDU-INSTANCE ID="I{k}_t"><fx:PDU-REF ID-REF="P{k}_t"/><fx:SEQUENCE-NUMBER>0</fx:SEQUENCE-NUMBER></fx:PDU-INSTANCE>"#
        );
        for (j, (name, ty)) in m.args.iter().enumerate() {
            let (coding, _, bits) = ty.coding();
            let _ = write!(
                pdus,
                r#"<fx:PDU ID="P{k}_{j}"><ho:SHORT-NAME>P{k}_{j}</ho:SHORT-NAME><fx:BYTE-LENGTH>{}</fx:BYTE-LENGTH><fx:SIGNAL-INSTANCES><fx:SIGNAL-INSTANCE ID="SI{k}_{j}"><fx:SEQUENCE-NUMBER>0</fx:SEQUENCE-NUMBER><fx:SIGNAL-REF ID-REF="S_{name}"/></fx:SIGNAL-INSTANCE></fx:SIGNAL-INSTANCES></fx:PDU>"#,
                bits / 8
            );
            let _ = write!(
                signals,
                r#"<fx:SIGNAL ID="S_{name}"><ho:SHORT-NAME>{name}</ho:SHORT-NAME><fx:CODING-REF ID-REF="{coding}"/></fx:SIGNAL>"#
            );
            let _ = write!(
                inst,
                r#"<fx:PDU-INSTANCE ID="I{k}_{j}"><fx:PDU-REF ID-REF="P{k}_{j}"/><fx:SEQUENCE-NUMBER>{}</fx:SEQUENCE-NUMBER></fx:PDU-INSTANCE>"#,
                j + 1
            );
        }
        let _ = writeln!(
            frames,
            r#"<fx:FRAME ID="ID_{}"><ho:SHORT-NAME>ID_{}</ho:SHORT-NAME><fx:FRAME-TYPE>OTHER</fx:FRAME-TYPE><fx:PDU-INSTANCES>{inst}</fx:PDU-INSTANCES><fx:MANUFACTURER-EXTENSION><MESSAGE_TYPE>DLT_TYPE_LOG</MESSAGE_TYPE><MESSAGE_INFO>{level}</MESSAGE_INFO><APPLICATION_ID>{apid}</APPLICATION_ID><CONTEXT_ID>{ctid}</CONTEXT_ID></fx:MANUFACTURER-EXTENSION></fx:FRAME>"#,
            m.id, m.id
        );
    }
    let mut codings = String::new();
    for ty in [Ty::U8, Ty::U16, Ty::U32, Ty::I32, Ty::F32] {
        let (id, base, bits) = ty.coding();
        let _ = write!(
            codings,
            r#"<fx:CODING ID="{id}"><ho:CODED-TYPE ho:BASE-DATA-TYPE="{base}" CATEGORY="STANDARD-LENGTH-TYPE"><ho:BIT-LENGTH>{bits}</ho:BIT-LENGTH></ho:CODED-TYPE></fx:CODING>"#
        );
    }
    let (id, base, _) = Ty::Str.coding();
    let _ = write!(
        codings,
        r#"<fx:CODING ID="{id}"><ho:CODED-TYPE ho:BASE-DATA-TYPE="{base}" CATEGORY="STANDARD-LENGTH-TYPE" TERMINATION="ZERO"/></fx:CODING>"#
    );
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<fx:FIBEX xmlns:fx="http://www.asam.net/xml/fbx" xmlns:ho="http://www.asam.net/xml" VERSION="3.1.0">
<fx:PROJECT ID="p"><ho:SHORT-NAME>bench</ho:SHORT-NAME></fx:PROJECT>
<fx:ELEMENTS>
<fx:ECUS><fx:ECU ID="ECU1"><ho:SHORT-NAME>ECU1</ho:SHORT-NAME></fx:ECU></fx:ECUS>
<fx:PDUS>{pdus}</fx:PDUS>
<fx:FRAMES>
{frames}</fx:FRAMES>
<fx:SIGNALS>{signals}</fx:SIGNALS>
</fx:ELEMENTS>
<fx:PROCESSING-INFORMATION><fx:CODINGS>{codings}</fx:CODINGS></fx:PROCESSING-INFORMATION>
</fx:FIBEX>
"#
    )
}
