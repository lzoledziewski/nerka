//! SOME/IP (AUTOSAR PRS_SOMEIP) message headers.
//!
//! ```text
//! message id   4 B  service id u16, method / event id u16      (big endian)
//! length       4 B  bytes after this field: 8 + payload
//! request id   4 B  client id u16, session id u16
//!              4 B  protocol version (1), interface version,
//!                   message type, return code
//! payload
//! ```
//!
//! A UDP datagram can hold several messages back to back. Payloads aren't
//! interpreted (that needs the service's description: FIBEX or ARXML,
//! later); they're shown as hex. Service discovery (service `0xFFFF`,
//! method `0x8100`) is named `SD`. TCP and SOME/IP-TP reassembly aren't
//! done: segments show as they are (`TP_…` types).

use super::{Origin, Protocol};
use crate::event::{FieldDef, FieldId, FieldKind, FieldValue, Level};
use crate::input::ColumnDef;
use std::io::Write;

const HEADER_LEN: usize = 16;
const PROTOCOL_VERSION: u8 = 1;

const COLUMNS: [ColumnDef; 7] = [
    ColumnDef { name: "service", width: 6 },
    ColumnDef { name: "method", width: 6 },
    ColumnDef { name: "client", width: 6 },
    ColumnDef { name: "session", width: 7 },
    ColumnDef { name: "type", width: 17 },
    ColumnDef { name: "rc", width: 10 },
    ColumnDef { name: "payload", width: 0 },
];

const SERVICE: FieldId = 0;
const METHOD: FieldId = 1;
const CLIENT: FieldId = 2;
const SESSION: FieldId = 3;
const IFACE: FieldId = 4;
const TYPE: FieldId = 5;
const RC: FieldId = 6;
const LEN: FieldId = 7;
const PAYLOAD: FieldId = 8;

const SCHEMA: &[FieldDef] = &[
    FieldDef { name: "service", kind: FieldKind::U64, costly: false },
    // method or event id
    FieldDef { name: "method", kind: FieldKind::U64, costly: false },
    FieldDef { name: "client", kind: FieldKind::U64, costly: false },
    FieldDef { name: "session", kind: FieldKind::U64, costly: false },
    // interface version
    FieldDef { name: "iface", kind: FieldKind::U64, costly: false },
    // REQUEST, NOTIFICATION, RESPONSE, ERROR, …
    FieldDef { name: "type", kind: FieldKind::Str, costly: false },
    // return code: E_OK, E_NOT_OK, …
    FieldDef { name: "rc", kind: FieldKind::Str, costly: false },
    // payload length in bytes
    FieldDef { name: "len", kind: FieldKind::U64, costly: false },
    // the payload as hex, i.e. the `payload` column
    FieldDef { name: "payload", kind: FieldKind::Str, costly: true },
];

/// The SOME/IP protocol.
pub struct SomeIp;

/// The header of the message at `buf[0]`, if it is well formed and fits.
struct Header<'a> {
    bytes: &'a [u8],
}

impl<'a> Header<'a> {
    fn parse(buf: &'a [u8]) -> Option<Header<'a>> {
        if buf.len() < HEADER_LEN || buf[12] != PROTOCOL_VERSION {
            return None;
        }
        let len = u32::from_be_bytes(buf[4..8].try_into().unwrap()) as usize;
        if len < 8 || buf.len() - 8 < len {
            return None;
        }
        Some(Header { bytes: &buf[..8 + len] })
    }

    fn u16_at(&self, at: usize) -> u16 {
        u16::from_be_bytes([self.bytes[at], self.bytes[at + 1]])
    }

    fn service(&self) -> u16 {
        self.u16_at(0)
    }

    fn method(&self) -> u16 {
        self.u16_at(2)
    }

    fn client(&self) -> u16 {
        self.u16_at(8)
    }

    fn session(&self) -> u16 {
        self.u16_at(10)
    }

    fn iface(&self) -> u8 {
        self.bytes[13]
    }

    fn msg_type(&self) -> u8 {
        self.bytes[14]
    }

    fn rc(&self) -> u8 {
        self.bytes[15]
    }

    fn payload(&self) -> &'a [u8] {
        &self.bytes[HEADER_LEN..]
    }

    fn is_sd(&self) -> bool {
        self.service() == 0xFFFF && self.method() == 0x8100
    }

    fn is_error(&self) -> bool {
        matches!(self.msg_type(), 0x81 | 0xA1) || self.rc() != 0
    }
}

/// A message type's name; an unknown one in hex.
fn type_name(t: u8) -> Option<&'static str> {
    Some(match t {
        0x00 => "REQUEST",
        0x01 => "REQUEST_NO_RETURN",
        0x02 => "NOTIFICATION",
        0x80 => "RESPONSE",
        0x81 => "ERROR",
        0x20 => "TP_REQUEST",
        0x21 => "TP_REQUEST_NO_RETURN",
        0x22 => "TP_NOTIFICATION",
        0xA0 => "TP_RESPONSE",
        0xA1 => "TP_ERROR",
        _ => return None,
    })
}

/// A return code's name; an unknown one in hex.
fn rc_name(rc: u8) -> Option<&'static str> {
    Some(match rc {
        0x00 => "E_OK",
        0x01 => "E_NOT_OK",
        0x02 => "E_UNKNOWN_SERVICE",
        0x03 => "E_UNKNOWN_METHOD",
        0x04 => "E_NOT_READY",
        0x05 => "E_NOT_REACHABLE",
        0x06 => "E_TIMEOUT",
        0x07 => "E_WRONG_PROTOCOL_VERSION",
        0x08 => "E_WRONG_INTERFACE_VERSION",
        0x09 => "E_MALFORMED_MESSAGE",
        0x0A => "E_WRONG_MESSAGE_TYPE",
        _ => return None,
    })
}

fn named(name: Option<&'static str>, value: u8, out: &mut Vec<u8>) {
    match name {
        Some(n) => out.extend_from_slice(n.as_bytes()),
        None => {
            let _ = write!(out, "0x{value:02x}");
        }
    }
}

/// `[len] 01 02 …`: the payload in hex, with its length (`SD` first for
/// service discovery).
fn render_payload(h: &Header<'_>, out: &mut Vec<u8>) {
    if h.is_sd() {
        out.extend_from_slice(b"SD ");
    }
    let p = h.payload();
    let _ = write!(out, "[{}]", p.len());
    for b in p {
        let _ = write!(out, " {b:02x}");
    }
}

impl Protocol for SomeIp {
    fn name(&self) -> &'static str {
        "SOMEIP"
    }

    fn columns(&self) -> &'static [ColumnDef] {
        &COLUMNS
    }

    fn schema(&self) -> &'static [FieldDef] {
        SCHEMA
    }

    fn split(&self, datagram: &[u8], found: &mut dyn FnMut(usize)) -> usize {
        let mut pos = 0;
        while let Some(h) = Header::parse(&datagram[pos..]) {
            found(pos);
            pos += h.bytes.len();
        }
        datagram.len() - pos
    }

    fn render(&self, msg: &[u8], _at: &Origin<'_>, out: &mut Vec<u8>) {
        let Some(h) = Header::parse(msg) else { return };
        let _ =
            write!(out, "0x{:04x}\t0x{:04x}\t0x{:04x}\t0x{:04x}\t", h.service(), h.method(), h.client(), h.session());
        named(type_name(h.msg_type()), h.msg_type(), out);
        out.push(b'\t');
        named(rc_name(h.rc()), h.rc(), out);
        out.push(b'\t');
        render_payload(&h, out);
    }

    fn detail(&self, msg: &[u8], at: &Origin<'_>) -> Vec<String> {
        let mut out = Vec::new();
        self.render(msg, at, &mut out);
        String::from_utf8_lossy(&out).split('\t').map(str::to_owned).collect()
    }

    fn field<'m>(&self, msg: &'m [u8], _at: &Origin<'m>, id: FieldId) -> Option<FieldValue<'m>> {
        let h = Header::parse(msg)?;
        let n = |v: u16| Some(FieldValue::U64(v.into()));
        let s = |name: Option<&'static str>, v: u8| {
            let mut out = Vec::new();
            named(name, v, &mut out);
            Some(FieldValue::Str(out.into()))
        };
        match id {
            SERVICE => n(h.service()),
            METHOD => n(h.method()),
            CLIENT => n(h.client()),
            SESSION => n(h.session()),
            IFACE => Some(FieldValue::U64(h.iface().into())),
            TYPE => s(type_name(h.msg_type()), h.msg_type()),
            RC => s(rc_name(h.rc()), h.rc()),
            LEN => Some(FieldValue::U64(h.payload().len() as u64)),
            PAYLOAD => {
                let mut out = Vec::new();
                render_payload(&h, &mut out);
                Some(FieldValue::Str(out.into()))
            }
            _ => None,
        }
    }

    /// Errors (an `ERROR` type or a return code other than `E_OK`) are
    /// errors; everything else is info.
    fn level(&self, msg: &[u8], _at: &Origin<'_>) -> Option<Level> {
        let h = Header::parse(msg)?;
        Some(if h.is_error() { Level::Error } else { Level::Info })
    }
}

/// One SOME/IP message (for tests and examples).
pub fn encode(service: u16, method: u16, client: u16, session: u16, msg_type: u8, rc: u8, payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(HEADER_LEN + payload.len());
    out.extend_from_slice(&service.to_be_bytes());
    out.extend_from_slice(&method.to_be_bytes());
    out.extend_from_slice(&(8 + payload.len() as u32).to_be_bytes());
    out.extend_from_slice(&client.to_be_bytes());
    out.extend_from_slice(&session.to_be_bytes());
    out.extend_from_slice(&[PROTOCOL_VERSION, 1, msg_type, rc]);
    out.extend_from_slice(payload);
    out
}
