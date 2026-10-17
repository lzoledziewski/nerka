//! Minimal DLT encoder for synthetic test files and benchmarks
//! (`examples/gen_dlt.rs`). It writes what the reader understands; it is
//! not a full DLT logging library.

use super::format::*;
use crate::Level;

/// A 4-byte DLT id (ECU, APID, CTID), NUL-padded.
pub fn id(s: &str) -> [u8; 4] {
    let mut out = [0u8; 4];
    for (o, b) in out.iter_mut().zip(s.bytes()) {
        *o = b;
    }
    out
}

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Bool(bool),
    /// Unsigned value and its size in bytes (1, 2, 4 or 8).
    U(u64, u8),
    /// Signed value and its size in bytes (1, 2, 4 or 8).
    I(i64, u8),
    F32(f32),
    F64(f64),
    Str(String),
    Raw(Vec<u8>),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Arg {
    pub value: Value,
    /// Written with VARI (name, and unit for numbers) if set.
    pub name: Option<(String, String)>,
}

impl From<Value> for Arg {
    fn from(value: Value) -> Self {
        Arg { value, name: None }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Payload {
    Verbose(Vec<Arg>),
    NonVerbose {
        id: u32,
        data: Vec<u8>,
    },
    /// Bytes written as they are (control messages, malformed payloads).
    Bytes(Vec<u8>),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Extended {
    /// `MSTP`: 0 log, 1 app trace, 2 network trace, 3 control.
    pub mstp: u8,
    /// `MTIN`: the level for log messages (1 fatal … 6 verbose).
    pub mtin: u8,
    pub apid: [u8; 4],
    pub ctid: [u8; 4],
}

#[derive(Debug, Clone, PartialEq)]
pub struct Msg {
    pub secs: u32,
    pub usecs: i32,
    pub storage_ecu: [u8; 4],
    /// Standard-header ECU id (WEID).
    pub ecu: Option<[u8; 4]>,
    pub session_id: Option<u32>,
    /// 0.1 ms units (WTMS).
    pub timestamp: Option<u32>,
    pub mcnt: u8,
    pub big_endian: bool,
    pub ext: Option<Extended>,
    pub payload: Payload,
}

/// MTIN for a log level.
pub fn mtin(level: Level) -> u8 {
    match level {
        Level::Fatal => 1,
        Level::Error => 2,
        Level::Warn => 3,
        Level::Info | Level::Notice => 4,
        Level::Debug => 5,
        Level::Trace => 6,
    }
}

impl Msg {
    /// A verbose log message with the usual headers (ECU, timestamp).
    pub fn log(level: Level, ecu: &str, apid: &str, ctid: &str, args: Vec<Arg>) -> Msg {
        Msg {
            secs: 1_760_745_600, // 2025-10-18 00:00:00 UTC
            usecs: 0,
            storage_ecu: id(ecu),
            ecu: Some(id(ecu)),
            session_id: None,
            timestamp: Some(0),
            mcnt: 0,
            big_endian: false,
            ext: Some(Extended { mstp: MSTP_LOG, mtin: mtin(level), apid: id(apid), ctid: id(ctid) }),
            payload: Payload::Verbose(args),
        }
    }

    /// Append the stored message (storage header included) to `out`.
    pub fn encode(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(MAGIC);
        out.extend_from_slice(&self.secs.to_le_bytes());
        out.extend_from_slice(&self.usecs.to_le_bytes());
        out.extend_from_slice(&self.storage_ecu);

        let start = out.len();
        let mut htyp = HTYP_VERSION_1;
        for (on, flag) in [
            (self.ext.is_some(), HTYP_UEH),
            (self.big_endian, HTYP_MSBF),
            (self.ecu.is_some(), HTYP_WEID),
            (self.session_id.is_some(), HTYP_WSID),
            (self.timestamp.is_some(), HTYP_WTMS),
        ] {
            if on {
                htyp |= flag;
            }
        }
        out.extend_from_slice(&[htyp, self.mcnt, 0, 0]);
        if let Some(e) = self.ecu {
            out.extend_from_slice(&e);
        }
        if let Some(s) = self.session_id {
            out.extend_from_slice(&s.to_be_bytes());
        }
        if let Some(t) = self.timestamp {
            out.extend_from_slice(&t.to_be_bytes());
        }
        if let Some(x) = &self.ext {
            let (verb, noar) = match &self.payload {
                Payload::Verbose(a) => (MSIN_VERB, a.len() as u8),
                _ => (0, 0),
            };
            out.extend_from_slice(&[verb | (x.mstp & 7) << 1 | x.mtin << 4, noar]);
            out.extend_from_slice(&x.apid);
            out.extend_from_slice(&x.ctid);
        }
        let mut w = Wr { out, be: self.big_endian };
        match &self.payload {
            Payload::Verbose(args) => args.iter().for_each(|a| w.arg(a)),
            Payload::NonVerbose { id, data } => {
                w.u32(*id);
                w.out.extend_from_slice(data);
            }
            Payload::Bytes(b) => w.out.extend_from_slice(b),
        }
        let len = (out.len() - start) as u16;
        out[start + 2..start + 4].copy_from_slice(&len.to_be_bytes());
    }
}

struct Wr<'a> {
    out: &'a mut Vec<u8>,
    be: bool,
}

impl Wr<'_> {
    fn u16(&mut self, v: u16) {
        self.out.extend_from_slice(&if self.be { v.to_be_bytes() } else { v.to_le_bytes() });
    }

    fn u32(&mut self, v: u32) {
        self.out.extend_from_slice(&if self.be { v.to_be_bytes() } else { v.to_le_bytes() });
    }

    /// `v`'s low `size` bytes in payload byte order.
    fn uint(&mut self, v: u64, size: u8) {
        let bytes = v.to_le_bytes();
        let bytes = &bytes[..size as usize];
        if self.be {
            self.out.extend(bytes.iter().rev());
        } else {
            self.out.extend_from_slice(bytes);
        }
    }

    fn cstr(&mut self, s: &str) {
        self.u16(s.len() as u16 + 1);
        self.out.extend_from_slice(s.as_bytes());
        self.out.push(0);
    }

    fn arg(&mut self, a: &Arg) {
        const VARI: u32 = 0x800;
        let tyle = |size: u8| size.trailing_zeros() + 1;
        let vari = if a.name.is_some() { VARI } else { 0 };
        let (name, unit) = a.name.clone().unwrap_or_default();
        match &a.value {
            Value::Bool(b) => {
                self.u32(0x10 | 1 | vari);
                if vari != 0 {
                    self.cstr(&name);
                }
                self.out.push(*b as u8);
            }
            Value::U(v, size) => {
                self.u32(0x40 | tyle(*size) | vari);
                self.name_unit(vari, &name, &unit);
                self.uint(*v, *size);
            }
            Value::I(v, size) => {
                self.u32(0x20 | tyle(*size) | vari);
                self.name_unit(vari, &name, &unit);
                self.uint(*v as u64, *size);
            }
            Value::F32(v) => {
                self.u32(0x80 | 3 | vari);
                self.name_unit(vari, &name, &unit);
                self.uint(v.to_bits() as u64, 4);
            }
            Value::F64(v) => {
                self.u32(0x80 | 4 | vari);
                self.name_unit(vari, &name, &unit);
                self.uint(v.to_bits(), 8);
            }
            Value::Str(s) => {
                self.u32(0x200 | (1 << 15) | vari);
                self.u16(s.len() as u16 + 1);
                if vari != 0 {
                    self.cstr(&name);
                }
                self.out.extend_from_slice(s.as_bytes());
                self.out.push(0);
            }
            Value::Raw(b) => {
                self.u32(0x400 | vari);
                self.u16(b.len() as u16);
                if vari != 0 {
                    self.cstr(&name);
                }
                self.out.extend_from_slice(b);
            }
        }
    }

    fn name_unit(&mut self, vari: u32, name: &str, unit: &str) {
        if vari != 0 {
            self.u16(name.len() as u16 + 1);
            self.u16(unit.len() as u16 + 1);
            self.out.extend_from_slice(name.as_bytes());
            self.out.push(0);
            self.out.extend_from_slice(unit.as_bytes());
            self.out.push(0);
        }
    }
}
