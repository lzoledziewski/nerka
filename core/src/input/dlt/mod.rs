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
//! DLT found inside other files (pcap, MF4) is read in place: those readers
//! index every message's standard header with the capture's time
//! ([`Framed`]), and bytes that aren't contiguous in the file (a reassembled
//! datagram) live in a side buffer. No temporary `.dlt` is written. A
//! message without an ECU id takes its capture channel's.

mod format;
mod payload;
pub mod writer;

use super::{Bytes, ColumnDef};
use crate::event::{FieldDef, FieldId, FieldKind, FieldValue, Level};
use crate::{LineNo, LogSource, Progress, Result};
use format::{MAGIC, MSTP_APP_TRACE, MSTP_CONTROL, MSTP_LOG, MSTP_NW_TRACE, Message, NO_ECU, STORAGE_LEN, Storage};
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

/// Length of the well-formed DLT message whose standard header is at
/// `buf[0]` (no storage header), if one is there and fits.
pub(crate) fn message_len(buf: &[u8]) -> Option<usize> {
    Message::parse_std(buf, Storage { secs: 0, usecs: 0, ecu: NO_ECU }).map(|m| m.len())
}

/// Does `head` (the first bytes of a file) look like a DLT file?
pub fn is_dlt(head: &[u8]) -> bool {
    head.starts_with(MAGIC)
}

/// A DLT message inside another file (a capture): where its standard
/// header is, the capture channel it came from and the storage time the
/// capture gives it. 16 bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Framed {
    /// Bits 0–47: offset of the standard header, in the file or (bit 63)
    /// in the side buffer; bits 48–62: channel.
    at: u64,
    secs: u32,
    usecs: i32,
}

const OFFSET_MASK: u64 = (1 << 48) - 1;
const IN_SIDE: u64 = 1 << 63;
const CHANNEL_SHIFT: u32 = 48;
/// Channels a capture can name (15 bits).
pub const MAX_CHANNELS: usize = 1 << 15;

impl Framed {
    /// A message at `offset` (in the side buffer if `side`), from capture
    /// channel `channel` (an index into the source's channel ECU table),
    /// stored at `secs` + `usecs`. `None` if the offset or channel don't fit.
    pub fn new(offset: u64, side: bool, channel: usize, secs: u32, usecs: i32) -> Option<Framed> {
        if offset > OFFSET_MASK || channel >= MAX_CHANNELS {
            return None;
        }
        let at = offset | ((channel as u64) << CHANNEL_SHIFT) | if side { IN_SIDE } else { 0 };
        Some(Framed { at, secs, usecs })
    }

    fn offset(&self) -> usize {
        (self.at & OFFSET_MASK) as usize
    }

    fn in_side(&self) -> bool {
        self.at & IN_SIDE != 0
    }

    fn channel(&self) -> usize {
        ((self.at & !IN_SIDE) >> CHANNEL_SHIFT) as usize
    }
}

/// Where the messages are.
enum Index {
    /// A `.dlt` file: the offset of each message's storage header.
    Stored(Vec<u64>),
    /// DLT inside a capture: each message's place and capture time, the
    /// bytes that aren't contiguous in the file, and per channel the ECU id
    /// for messages that don't carry one.
    Framed { msgs: Vec<Framed>, side: Vec<u8>, channel_ecus: Vec<[u8; 4]> },
}

pub struct DltFile {
    name: String,
    path: Option<PathBuf>,
    data: Bytes,
    index: Index,
    skipped: u64,
}

impl DltFile {
    pub fn open(path: impl AsRef<Path>, progress: &Progress) -> Result<Self> {
        let path = path.as_ref();
        let data = Bytes::map(path)?;
        let (offsets, skipped) = build_index(data.bytes(), progress)?;
        let name = path.display().to_string();
        Ok(Self { name, path: Some(path.to_path_buf()), data, index: Index::Stored(offsets), skipped })
    }

    /// In-memory source, handy for tests.
    pub fn from_bytes(name: impl Into<String>, bytes: Vec<u8>) -> Self {
        let (offsets, skipped) = build_index(&bytes, &Progress::new()).expect("not cancelled");
        Self { name: name.into(), path: None, data: Bytes::Owned(bytes), index: Index::Stored(offsets), skipped }
    }

    /// DLT messages found inside another file (`data`, e.g. a mapped pcap):
    /// `msgs` in display order, `side` holding the messages placed there,
    /// `channel_ecus` the ECU id per capture channel (for messages without
    /// one; NUL-padded, all zero for none). `skipped` counts bytes of DLT
    /// traffic that weren't messages.
    pub(crate) fn from_frames(
        name: String,
        path: Option<PathBuf>,
        data: Bytes,
        msgs: Vec<Framed>,
        side: Vec<u8>,
        channel_ecus: Vec<[u8; 4]>,
        skipped: u64,
    ) -> Self {
        Self { name, path, data, index: Index::Framed { msgs, side, channel_ecus }, skipped }
    }

    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    /// Bytes that were not part of any well-formed message.
    pub fn skipped_bytes(&self) -> u64 {
        self.skipped
    }

    fn msg(&self, line: LineNo) -> Option<Message<'_>> {
        let i = usize::try_from(line).ok()?;
        match &self.index {
            Index::Stored(offsets) => Message::parse(&self.data.bytes()[*offsets.get(i)? as usize..]),
            Index::Framed { msgs, side, channel_ecus } => {
                let f = msgs.get(i)?;
                let buf = if f.in_side() { side.as_slice() } else { self.data.bytes() };
                let ecu = channel_ecus.get(f.channel()).unwrap_or(NO_ECU);
                let storage = Storage { secs: f.secs, usecs: f.usecs, ecu };
                Message::parse_std(buf.get(f.offset()..)?, storage)
            }
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
    render_header(m, &mut out);
    out.push(b'\t');
    payload::render(m, &mut out);
    out
}

/// The columns before the payload, tab-separated (no trailing tab).
fn render_header(m: &Message<'_>, out: &mut Vec<u8>) {
    let (secs, usecs) = m.storage_time();
    write_time(out, secs, usecs);
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
}

impl LogSource for DltFile {
    fn name(&self) -> &str {
        &self.name
    }

    fn line_count(&self) -> LineNo {
        match &self.index {
            Index::Stored(o) => o.len() as LineNo,
            Index::Framed { msgs, .. } => msgs.len() as LineNo,
        }
    }

    fn line_bytes(&self, line: LineNo) -> Option<Cow<'_, [u8]>> {
        self.msg(line).map(|m| Cow::Owned(render_line(&m)))
    }

    fn line_detail(&self, line: LineNo) -> Option<Vec<String>> {
        let m = self.msg(line)?;
        let mut head = Vec::new();
        render_header(&m, &mut head);
        let mut cells: Vec<String> = String::from_utf8_lossy(&head).split('\t').map(str::to_owned).collect();
        let mut payload = Vec::new();
        payload::render_formatted(&m, &mut payload);
        cells.push(String::from_utf8_lossy(&payload).into_owned());
        Some(cells)
    }

    fn size_bytes(&self) -> u64 {
        self.data.bytes().len() as u64
    }

    fn index_memory_bytes(&self) -> usize {
        match &self.index {
            Index::Stored(o) => o.capacity() * size_of::<u64>(),
            Index::Framed { msgs, side, channel_ecus } => {
                msgs.capacity() * size_of::<Framed>() + side.capacity() + channel_ecus.capacity() * 4
            }
        }
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
