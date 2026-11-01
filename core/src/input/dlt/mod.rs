//! DLT (AUTOSAR Diagnostic Log and Trace) files with storage headers.
//!
//! Opening a file makes one sequential pass that records the offset of
//! every message (8 bytes per message, like the text line index). Headers
//! are read in place from the mapping when a field is asked for, so filters
//! on `ecu`, `apid`, `ctid`, `type`, `level`, … never decode a payload.
//! Payloads are decoded only when a line's text is needed (display, regex).
//!
//! Each message is shown as one line of tab-separated columns, see
//! [`COLUMNS`]. Corrupt or truncated data between messages is skipped (the
//! reader resyncs on the next `DLT\x01`) and counted in
//! [`DltFile::skipped_bytes`].
//!
//! DLT is one [`Protocol`] ([`Dlt`]) of the [`Records`] source: a `.dlt`
//! file is a container whose storage headers give each message its time
//! and ECU id; DLT found inside captures (pcap, MF4) is read in place with
//! the capture's time, and a message without an ECU id takes its capture
//! channel's. No temporary `.dlt` is written.

pub mod fibex;
mod format;
mod payload;
pub mod writer;

use super::proto::{Origin, Protocol, Records};
use super::{Bytes, ColumnDef};
use crate::event::{FieldDef, FieldId, FieldKind, FieldValue, Level};
use crate::{Progress, Result};
use fibex::{Fibex, Frame};
use format::{MAGIC, MSTP_APP_TRACE, MSTP_CONTROL, MSTP_LOG, MSTP_NW_TRACE, Message, NO_ECU, STORAGE_LEN, Storage};
use std::io::Write;
use std::path::Path;
use std::sync::Arc;

/// A `.dlt` file, or DLT in a capture: a [`Records`] source.
pub type DltFile = Records;

/// Columns of a rendered line, separated by tabs: `time`, then [`Dlt`]'s.
pub const COLUMNS: [ColumnDef; 7] = [
    ColumnDef { name: "time", width: 26 },
    ColumnDef { name: "tmsp", width: 11 },
    ColumnDef { name: "ecu", width: 4 },
    ColumnDef { name: "apid", width: 4 },
    ColumnDef { name: "ctid", width: 4 },
    ColumnDef { name: "type", width: 9 },
    ColumnDef { name: "payload", width: 0 },
];

const ECU: FieldId = 0;
const APID: FieldId = 1;
const CTID: FieldId = 2;
const TYPE: FieldId = 3;
const MCNT: FieldId = 4;
const TMSP: FieldId = 5;
const SEID: FieldId = 6;
const MID: FieldId = 7;
const MODE: FieldId = 8;
const PAYLOAD: FieldId = 9;

const SCHEMA: &[FieldDef] = &[
    FieldDef { name: "ecu", kind: FieldKind::Str, costly: false },
    FieldDef { name: "apid", kind: FieldKind::Str, costly: false },
    FieldDef { name: "ctid", kind: FieldKind::Str, costly: false },
    // log, app_trace, nw_trace, control
    FieldDef { name: "type", kind: FieldKind::Str, costly: false },
    // message counter
    FieldDef { name: "mcnt", kind: FieldKind::U64, costly: false },
    // ECU uptime in 0.1 ms units
    FieldDef { name: "tmsp", kind: FieldKind::U64, costly: false },
    // session id
    FieldDef { name: "seid", kind: FieldKind::U64, costly: false },
    // message id of non-verbose messages (none on verbose ones: 0 is a valid id)
    FieldDef { name: "mid", kind: FieldKind::U64, costly: false },
    // verbose, non-verbose
    FieldDef { name: "mode", kind: FieldKind::Str, costly: false },
    // the decoded payload, i.e. the `payload` column
    FieldDef { name: "payload", kind: FieldKind::Str, costly: true },
];

/// Length of the well-formed DLT message whose standard header is at
/// `buf[0]` (no storage header), if one is there and fits.
pub(crate) fn message_len(buf: &[u8]) -> Option<usize> {
    Message::parse_std(buf, Storage { secs: 0, usecs: 0, ecu: NO_ECU }).map(|m| m.len())
}

/// Does `head` (the first bytes of a file) look like a DLT file?
pub fn is_dlt(head: &[u8]) -> bool {
    head.starts_with(MAGIC)
}

/// The DLT protocol: messages from their standard header on. Non-verbose
/// messages are decoded with FIBEX descriptions, if given.
#[derive(Default)]
pub struct Dlt {
    fibex: Option<Arc<Fibex>>,
}

impl Dlt {
    pub fn new(fibex: Option<Arc<Fibex>>) -> Dlt {
        Dlt { fibex }
    }

    fn message<'m>(msg: &'m [u8], at: &Origin<'m>) -> Option<Message<'m>> {
        Message::parse_std(msg, Storage { secs: at.secs, usecs: at.usecs, ecu: at.channel })
    }

    /// The FIBEX description of a non-verbose message, if there is one.
    fn frame(&self, m: &Message<'_>) -> Option<&Frame> {
        let fibex = self.fibex.as_ref()?;
        fibex.frame(non_verbose_id(m)?, m.ecu(), m.apid(), m.ctid())
    }
}

impl Protocol for Dlt {
    fn name(&self) -> &'static str {
        "DLT"
    }

    fn columns(&self) -> &'static [ColumnDef] {
        &COLUMNS[1..]
    }

    fn schema(&self) -> &'static [FieldDef] {
        SCHEMA
    }

    /// Several messages back to back; a storage header in front of one is
    /// skipped (the capture's time wins).
    fn split(&self, bytes: &[u8], found: &mut Vec<usize>) -> usize {
        let mut pos = 0;
        while pos < bytes.len() {
            if bytes[pos..].starts_with(MAGIC) && bytes.len() - pos > STORAGE_LEN {
                pos += STORAGE_LEN;
            }
            let Some(len) = message_len(&bytes[pos..]) else { break };
            found.push(pos);
            pos += len;
        }
        bytes.len() - pos
    }

    fn render(&self, msg: &[u8], at: &Origin<'_>, out: &mut Vec<u8>) {
        let Some(m) = Dlt::message(msg, at) else { return };
        let frame = self.frame(&m);
        render_header(&m, frame, out);
        out.push(b'\t');
        payload::render(&m, frame, out);
    }

    fn detail(&self, msg: &[u8], at: &Origin<'_>) -> Vec<String> {
        let Some(m) = Dlt::message(msg, at) else { return Vec::new() };
        let frame = self.frame(&m);
        let mut head = Vec::new();
        render_header(&m, frame, &mut head);
        let mut cells: Vec<String> = String::from_utf8_lossy(&head).split('\t').map(str::to_owned).collect();
        let mut payload = Vec::new();
        payload::render_formatted(&m, frame, &mut payload);
        cells.push(String::from_utf8_lossy(&payload).into_owned());
        cells
    }

    fn field<'m>(&self, msg: &'m [u8], at: &Origin<'m>, id: FieldId) -> Option<FieldValue<'m>> {
        let m = Dlt::message(msg, at)?;
        let s = |b: &'static str| FieldValue::Str(b.as_bytes().into());
        let owned = |b: &str| FieldValue::Str(b.as_bytes().to_vec().into());
        match id {
            ECU => Some(FieldValue::Str(m.ecu().into())),
            // A non-verbose message without them: from its FIBEX description.
            APID => match m.apid() {
                Some(b) => Some(FieldValue::Str(b.into())),
                None => self.frame(&m)?.apid.as_deref().map(owned),
            },
            CTID => match m.ctid() {
                Some(b) => Some(FieldValue::Str(b.into())),
                None => self.frame(&m)?.ctid.as_deref().map(owned),
            },
            TYPE => type_of(&m, if m.mstp().is_none() { self.frame(&m) } else { None }).map(s),
            MCNT => Some(FieldValue::U64(m.mcnt().into())),
            TMSP => m.timestamp().map(|t| FieldValue::U64(t.into())),
            SEID => m.session_id().map(|t| FieldValue::U64(t.into())),
            MID => non_verbose_id(&m).map(|t| FieldValue::U64(t.into())),
            // Without an extended header a message is non-verbose.
            MODE => Some(s(if m.verbose() { "verbose" } else { "non-verbose" })),
            PAYLOAD => {
                let mut out = Vec::new();
                payload::render(&m, self.frame(&m), &mut out);
                Some(FieldValue::Str(out.into()))
            }
            _ => None,
        }
    }

    fn level(&self, msg: &[u8], at: &Origin<'_>) -> Option<Level> {
        let m = Dlt::message(msg, at)?;
        let frame = if m.mstp().is_none() { self.frame(&m) } else { None };
        level_with(&m, frame)
    }

    fn argument(&self, msg: &[u8], at: &Origin<'_>, index: usize) -> Option<crate::signal::SignalValue> {
        let m = Dlt::message(msg, at)?;
        let frame = self.frame(&m)?;
        frame.arg(m.payload().get(4..)?, m.big_endian(), index)
    }
}

/// A `.dlt` file's storage header: the message's time and ECU id, and the
/// message right behind it.
fn storage_header(buf: &[u8]) -> Option<(Origin<'_>, usize)> {
    let s = Storage::parse(buf)?;
    Some((Origin { secs: s.secs, usecs: s.usecs, channel: s.ecu }, STORAGE_LEN))
}

impl Records {
    /// Open a `.dlt` file: one sequential pass records the offset of every
    /// message (8 bytes per message).
    pub fn open(path: impl AsRef<Path>, progress: &Progress) -> Result<Records> {
        let path = path.as_ref();
        let data = Bytes::map(path)?;
        let (offsets, skipped) = build_index(data.bytes(), progress)?;
        let name = path.display().to_string();
        let dlt = Arc::new(Dlt::default());
        Ok(Records::stored(name, Some(path.to_path_buf()), data, offsets, storage_header, dlt, skipped))
    }

    /// An in-memory `.dlt` file, handy for tests.
    pub fn from_bytes(name: impl Into<String>, bytes: Vec<u8>) -> Records {
        let (offsets, skipped) = build_index(&bytes, &Progress::new()).expect("not cancelled");
        let dlt = Arc::new(Dlt::default());
        Records::stored(name.into(), None, Bytes::Owned(bytes), offsets, storage_header, dlt, skipped)
    }

    /// Decode DLT non-verbose messages with `fibex`: their payload as text,
    /// and the APID, CTID, type and level they don't carry themselves.
    pub fn with_fibex(self, fibex: Option<Arc<Fibex>>) -> Records {
        match (0..crate::input::proto::MAX_PROTOCOLS).find(|&i| self.protocol(i).is_some_and(|p| p.name() == "DLT")) {
            Some(i) => self.with_protocol(i, Arc::new(Dlt::new(fibex))),
            None => self,
        }
    }
}

fn build_index(buf: &[u8], progress: &Progress) -> Result<(Vec<u64>, u64)> {
    const REPORT: usize = 4 << 20;
    progress.set_total(buf.len() as u64);
    let finder = memchr::memmem::Finder::new(MAGIC);
    // Typical messages are 60–200 bytes.
    let mut offsets = Vec::with_capacity(buf.len() / 96);
    let (mut pos, mut skipped, mut reported) = (0usize, 0u64, 0usize);
    while pos < buf.len() {
        match Message::parse(&buf[pos..]) {
            Some(m) => {
                offsets.push(pos as u64);
                pos += STORAGE_LEN + m.len();
            }
            None => {
                let next = finder.find(&buf[pos + 1..]).map_or(buf.len(), |i| pos + 1 + i);
                skipped += (next - pos) as u64;
                pos = next;
            }
        }
        if pos - reported >= REPORT {
            if progress.is_cancelled() {
                return Err(crate::Error::Cancelled);
            }
            progress.add((pos - reported) as u64);
            reported = pos;
        }
    }
    progress.add((buf.len() - reported) as u64);
    offsets.shrink_to_fit();
    Ok((offsets, skipped))
}

/// The level, from the message's extended header or its FIBEX description.
fn level_with(m: &Message<'_>, frame: Option<&Frame>) -> Option<Level> {
    match m.mstp() {
        Some(_) => level_of(m),
        None => frame?.level(),
    }
}

fn level_of(m: &Message<'_>) -> Option<Level> {
    if m.mstp() != Some(MSTP_LOG) {
        return None;
    }
    Some(match m.mtin()? {
        1 => Level::Fatal,
        2 => Level::Error,
        3 => Level::Warn,
        4 => Level::Info,
        5 => Level::Debug,
        6 => Level::Trace,
        _ => return None,
    })
}

fn type_of(m: &Message<'_>, frame: Option<&Frame>) -> Option<&'static str> {
    Some(match m.mstp().or(frame.and_then(|f| f.mstp))? {
        MSTP_LOG => "log",
        MSTP_APP_TRACE => "app_trace",
        MSTP_NW_TRACE => "nw_trace",
        MSTP_CONTROL => "control",
        _ => return None,
    })
}

/// The `type` column: the level for log messages, else the message type.
fn type_column(m: &Message<'_>, frame: Option<&Frame>) -> &'static str {
    match level_with(m, frame) {
        Some(Level::Fatal) => "fatal",
        Some(Level::Error) => "error",
        Some(Level::Warn) => "warn",
        Some(Level::Info | Level::Notice) => "info",
        Some(Level::Debug) => "debug",
        Some(Level::Trace) => "verbose",
        None => type_of(m, frame).unwrap_or(""),
    }
}

fn non_verbose_id(m: &Message<'_>) -> Option<u32> {
    if m.verbose() || m.mstp() == Some(MSTP_CONTROL) {
        return None;
    }
    let b: [u8; 4] = m.payload().get(..4)?.try_into().ok()?;
    Some(if m.big_endian() { u32::from_be_bytes(b) } else { u32::from_le_bytes(b) })
}

/// The columns after `time` and before the payload, tab-separated (no
/// trailing tab).
fn render_header(m: &Message<'_>, frame: Option<&Frame>, out: &mut Vec<u8>) {
    if let Some(t) = m.timestamp() {
        let _ = write!(out, "{}.{:04}", t / 10_000, t % 10_000);
    }
    let apid = m.apid().or_else(|| frame?.apid.as_deref().map(str::as_bytes));
    let ctid = m.ctid().or_else(|| frame?.ctid.as_deref().map(str::as_bytes));
    for id in [Some(m.ecu()), apid, ctid] {
        out.push(b'\t');
        out.extend_from_slice(id.unwrap_or_default());
    }
    out.push(b'\t');
    out.extend_from_slice(type_column(m, frame).as_bytes());
}

#[cfg(test)]
mod tests;
