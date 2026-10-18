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

mod format;
mod payload;
pub mod writer;

use super::{Bytes, ColumnDef};
use crate::event::{FieldDef, FieldId, FieldKind, FieldValue, Level};
use crate::{LineNo, LogSource, Progress, Result};
use format::{MAGIC, MSTP_APP_TRACE, MSTP_CONTROL, MSTP_LOG, MSTP_NW_TRACE, Message};
use std::borrow::Cow;
use std::io::Write;
use std::path::{Path, PathBuf};

/// Columns of a rendered line, separated by tabs.
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

/// Does `head` (the first bytes of a file) look like a DLT file?
pub fn is_dlt(head: &[u8]) -> bool {
    head.starts_with(MAGIC)
}

pub struct DltFile {
    name: String,
    path: Option<PathBuf>,
    data: Bytes,
    /// Offset of each message's storage header.
    offsets: Vec<u64>,
    skipped: u64,
}

impl DltFile {
    pub fn open(path: impl AsRef<Path>, progress: &Progress) -> Result<Self> {
        let path = path.as_ref();
        let data = Bytes::map(path)?;
        let (offsets, skipped) = build_index(data.bytes(), progress)?;
        Ok(Self { name: path.display().to_string(), path: Some(path.to_path_buf()), data, offsets, skipped })
    }

    /// In-memory source, handy for tests.
    pub fn from_bytes(name: impl Into<String>, bytes: Vec<u8>) -> Self {
        let (offsets, skipped) = build_index(&bytes, &Progress::new()).expect("not cancelled");
        Self { name: name.into(), path: None, data: Bytes::Owned(bytes), offsets, skipped }
    }

    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    /// Bytes that were not part of any well-formed message.
    pub fn skipped_bytes(&self) -> u64 {
        self.skipped
    }

    fn msg(&self, line: LineNo) -> Option<Message<'_>> {
        let off = *self.offsets.get(usize::try_from(line).ok()?)? as usize;
        Message::parse(&self.data.bytes()[off..])
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
                pos += m.len();
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

fn type_of(m: &Message<'_>) -> Option<&'static str> {
    Some(match m.mstp()? {
        MSTP_LOG => "log",
        MSTP_APP_TRACE => "app_trace",
        MSTP_NW_TRACE => "nw_trace",
        MSTP_CONTROL => "control",
        _ => return None,
    })
}

/// The `type` column: the level for log messages, else the message type.
fn type_column(m: &Message<'_>) -> &'static str {
    match level_of(m) {
        Some(Level::Fatal) => "fatal",
        Some(Level::Error) => "error",
        Some(Level::Warn) => "warn",
        Some(Level::Info | Level::Notice) => "info",
        Some(Level::Debug) => "debug",
        Some(Level::Trace) => "verbose",
        None => type_of(m).unwrap_or(""),
    }
}

fn non_verbose_id(m: &Message<'_>) -> Option<u32> {
    if m.verbose() || m.mstp() == Some(MSTP_CONTROL) {
        return None;
    }
    let b: [u8; 4] = m.payload().get(..4)?.try_into().ok()?;
    Some(if m.big_endian() { u32::from_be_bytes(b) } else { u32::from_le_bytes(b) })
}

/// `YYYY-MM-DD HH:MM:SS.uuuuuu`, UTC.
fn write_time(out: &mut Vec<u8>, secs: u32, usecs: i32) {
    // Days to civil date (H. Hinnant's algorithm).
    let z = (secs / 86_400) as i64 + 719_468;
    let (era, doe) = (z.div_euclid(146_097), z.rem_euclid(146_097));
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    let t = secs % 86_400;
    let _ = write!(
        out,
        "{y:04}-{m:02}-{d:02} {:02}:{:02}:{:02}.{:06}",
        t / 3600,
        t / 60 % 60,
        t % 60,
        usecs.clamp(0, 999_999)
    );
}

fn render_line(m: &Message<'_>) -> Vec<u8> {
    let mut out = Vec::with_capacity(80 + m.payload().len() * 3);
    let (secs, usecs) = m.storage_time();
    write_time(&mut out, secs, usecs);
    out.push(b'\t');
    if let Some(t) = m.timestamp() {
        let _ = write!(out, "{}.{:04}", t / 10_000, t % 10_000);
    }
    for id in [Some(m.ecu()), m.apid(), m.ctid()] {
        out.push(b'\t');
        out.extend_from_slice(id.unwrap_or_default());
    }
    out.push(b'\t');
    out.extend_from_slice(type_column(m).as_bytes());
    out.push(b'\t');
    payload::render(m, &mut out);
    out
}

impl LogSource for DltFile {
    fn name(&self) -> &str {
        &self.name
    }

    fn line_count(&self) -> LineNo {
        self.offsets.len() as LineNo
    }

    fn line_bytes(&self, line: LineNo) -> Option<Cow<'_, [u8]>> {
        self.msg(line).map(|m| Cow::Owned(render_line(&m)))
    }

    fn size_bytes(&self) -> u64 {
        self.data.bytes().len() as u64
    }

    fn index_memory_bytes(&self) -> usize {
        self.offsets.capacity() * size_of::<u64>()
    }

    fn columns(&self) -> &[ColumnDef] {
        &COLUMNS
    }

    fn schema(&self) -> &[FieldDef] {
        SCHEMA
    }

    fn field(&self, line: LineNo, id: FieldId) -> Option<FieldValue<'_>> {
        let m = self.msg(line)?;
        let s = |b: &'static str| FieldValue::Str(b.as_bytes().into());
        match id {
            ECU => Some(FieldValue::Str(m.ecu().into())),
            APID => m.apid().map(|b| FieldValue::Str(b.into())),
            CTID => m.ctid().map(|b| FieldValue::Str(b.into())),
            TYPE => type_of(&m).map(s),
            MCNT => Some(FieldValue::U64(m.mcnt().into())),
            TMSP => m.timestamp().map(|t| FieldValue::U64(t.into())),
            SEID => m.session_id().map(|t| FieldValue::U64(t.into())),
            MID => non_verbose_id(&m).map(|t| FieldValue::U64(t.into())),
            // Without an extended header a message is non-verbose.
            MODE => Some(s(if m.verbose() { "verbose" } else { "non-verbose" })),
            PAYLOAD => {
                let mut out = Vec::new();
                payload::render(&m, &mut out);
                Some(FieldValue::Str(out.into()))
            }
            _ => None,
        }
    }

    fn supplied_level(&self, line: LineNo) -> Option<Option<Level>> {
        Some(self.msg(line).as_ref().and_then(level_of))
    }
}

#[cfg(test)]
mod tests;
