//! Protocols: what a message is, apart from the file it came in.
//!
//! The layers of `doc/sources-and-signals.md`: a **container** (a `.dlt`
//! storage file, a pcap / pcapng capture, an MF4 bus log) yields messages
//! with what it knows about them ([`Origin`]: when they were stored or
//! captured, the channel they came on); a **router** picks the protocol of
//! each datagram (UDP ports, MF4 channel names); a [`Protocol`] splits
//! datagrams into messages and reads them: their columns, fields and level.
//! [`Records`] is the one source for all of them: an index of where each
//! message is ([`Framed`], with its protocol), and per protocol the reader.
//! DLT is a protocol like the others ([`super::dlt::Dlt`]); so are SOME/IP
//! ([`someip`]) and syslog ([`syslog`]).
//!
//! One protocol in a source: its own columns after `time`, and its fields
//! (plus `proto`). Several: shared columns `time`, `proto`, `info` (the
//! protocol's columns before its payload, in one cell) and `payload`, and
//! every protocol's fields; a field a message's protocol doesn't have has
//! no value there (it only satisfies `!=` and `!~`).

pub mod someip;
pub mod syslog;

use super::{ANCHOR_BYTES, Anchor, Bytes, ColumnDef, LogSource, new_data_id};
use crate::LineNo;
use crate::event::{FieldDef, FieldId, FieldKind, FieldValue, Level};
use std::borrow::Cow;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// What a container knows about a message: when it was stored or captured
/// (seconds and microseconds since the Unix epoch, UTC) and the 4-byte id
/// of the channel it came on (NUL-padded: a DLT storage header's ECU id,
/// an MF4 channel's ECU id; all zero for none).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Origin<'a> {
    pub secs: u32,
    pub usecs: i32,
    pub channel: &'a [u8; 4],
}

/// No channel id.
pub const NO_CHANNEL: &[u8; 4] = &[0; 4];

/// A protocol's reader. `msg` is a message from its first byte to (at
/// least) its end, as [`Self::split`] found it; every method reads it in
/// place, so filters on header fields never decode a payload.
pub trait Protocol: Send + Sync {
    /// Short name: the `proto` field and column (`DLT`, `SOMEIP`, `syslog`).
    fn name(&self) -> &'static str;

    /// Columns after `time`, as [`Self::render`] writes them; the last one
    /// is the payload.
    fn columns(&self) -> &'static [ColumnDef];

    /// The fields [`Self::field`] answers, by index.
    fn schema(&self) -> &'static [FieldDef];

    /// The messages in one datagram (several back to back are fine): adds
    /// the offset of each to `found` and returns the bytes that weren't one.
    fn split(&self, datagram: &[u8], found: &mut Vec<usize>) -> usize;

    /// Whether a message says where it ends (a length in its header). If
    /// not (syslog), a capture keeps a copy of each message behind its
    /// length, and the methods get exactly the message: from where `split`
    /// found it to the next one or the datagram's end.
    fn delimited(&self) -> bool {
        true
    }

    /// The [`Self::columns`], tab-separated, on one line: the payload's own
    /// line breaks and tabs as spaces.
    fn render(&self, msg: &[u8], at: &Origin<'_>, out: &mut Vec<u8>);

    /// The same cells for a detailed view: the payload keeps its line
    /// breaks.
    fn detail(&self, msg: &[u8], at: &Origin<'_>) -> Vec<String>;

    /// Field `id` (an index into [`Self::schema`]); `None` if the message
    /// has no value for it.
    fn field<'m>(&self, msg: &'m [u8], at: &Origin<'m>, id: FieldId) -> Option<FieldValue<'m>>;

    /// The level the protocol records; `None` if the message has none.
    fn level(&self, msg: &[u8], at: &Origin<'_>) -> Option<Level>;

    /// Argument `index` of the message, if the protocol knows its
    /// arguments (DLT non-verbose messages described by FIBEX).
    fn argument(&self, _msg: &[u8], _at: &Origin<'_>, _index: usize) -> Option<crate::signal::SignalValue> {
        None
    }
}

/// Index of a protocol in a [`Records`] source.
pub type ProtoId = usize;

/// Protocols a source can hold (4 bits of [`Framed`]).
pub const MAX_PROTOCOLS: usize = 16;

/// Channels a capture can name (11 bits of [`Framed`]).
pub const MAX_CHANNELS: usize = 1 << 11;

/// A message inside a container: where it starts, its protocol, the channel
/// it came from and the time the container gives it. 16 bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Framed {
    /// Bits 0–47: offset of the message, in the file or (bit 63) in the
    /// side buffer; bits 48–58: channel; bits 59–62: protocol.
    at: u64,
    secs: u32,
    usecs: i32,
}

const OFFSET_MASK: u64 = (1 << 48) - 1;
const IN_SIDE: u64 = 1 << 63;
const CHANNEL_SHIFT: u32 = 48;
const PROTO_SHIFT: u32 = 59;

impl Framed {
    /// A message of protocol `proto` at `offset` (in the side buffer if
    /// `side`), from channel `channel` (an index into the source's channel
    /// ids), stored at `secs` + `usecs`. `None` if something doesn't fit.
    pub fn new(offset: u64, side: bool, proto: ProtoId, channel: usize, secs: u32, usecs: i32) -> Option<Framed> {
        if offset > OFFSET_MASK || channel >= MAX_CHANNELS || proto >= MAX_PROTOCOLS {
            return None;
        }
        let at = offset
            | ((channel as u64) << CHANNEL_SHIFT)
            | ((proto as u64) << PROTO_SHIFT)
            | if side { IN_SIDE } else { 0 };
        Some(Framed { at, secs, usecs })
    }

    pub(crate) fn offset(&self) -> usize {
        (self.at & OFFSET_MASK) as usize
    }

    pub(crate) fn in_side(&self) -> bool {
        self.at & IN_SIDE != 0
    }

    pub(crate) fn channel(&self) -> usize {
        ((self.at >> CHANNEL_SHIFT) as usize) & (MAX_CHANNELS - 1)
    }

    pub(crate) fn proto(&self) -> ProtoId {
        ((self.at >> PROTO_SHIFT) as usize) & (MAX_PROTOCOLS - 1)
    }
}

/// Reads the header a stored file puts in front of each message (a `.dlt`
/// file's storage header): what it says, and where the message starts.
pub type StoredHeader = for<'a> fn(&'a [u8]) -> Option<(Origin<'a>, usize)>;

/// Where the messages are.
enum Index {
    /// A file of messages of protocol 0, each behind a header `header`
    /// reads: the offset of each header (8 bytes per message).
    Stored { offsets: Vec<u64>, header: StoredHeader },
    /// Messages inside a container: each one's place, protocol, channel
    /// and time; `side` holds the bytes that aren't contiguous in the file.
    Framed { msgs: Vec<Framed>, side: Vec<u8> },
}

/// The columns and fields a source shows: one protocol's own, or shared
/// ones for several.
struct View {
    /// Several protocols: the shared columns.
    mixed: bool,
    columns: Vec<ColumnDef>,
    schema: Vec<FieldDef>,
    /// The `proto` field's id.
    proto_field: FieldId,
    /// Per protocol, per field of the source: the protocol's own field id.
    fields: Vec<Vec<Option<FieldId>>>,
}

/// The shared columns of a source with several protocols.
pub const MIXED_COLUMNS: [ColumnDef; 4] = [
    ColumnDef { name: "time", width: 26 },
    ColumnDef { name: "proto", width: 6 },
    ColumnDef { name: "info", width: 46 },
    ColumnDef { name: "payload", width: 0 },
];

const TIME: ColumnDef = ColumnDef { name: "time", width: 26 };
/// The protocol's name; compared without case (`proto == syslog`, `SYSLOG`).
const PROTO_FIELD: FieldDef = FieldDef { name: "proto", kind: FieldKind::Name, costly: false };

impl View {
    /// The view for the protocols `present` (indexes into `protos`).
    fn new(protos: &[Arc<dyn Protocol>], present: &[ProtoId]) -> View {
        let present: &[ProtoId] = if present.is_empty() { &[0] } else { present };
        let mut schema: Vec<FieldDef> = Vec::new();
        let mixed = present.len() > 1;
        if mixed {
            schema.push(PROTO_FIELD);
        }
        // Fields of the same name are one field (all protocols have a
        // `payload`); a later protocol's of another kind is left out.
        for &p in present {
            for def in protos[p].schema() {
                if !schema.iter().any(|d| d.name == def.name) {
                    schema.push(def.clone());
                }
            }
        }
        if !mixed {
            schema.push(PROTO_FIELD);
        }
        let fields = protos
            .iter()
            .map(|p| {
                schema.iter().map(|d| p.schema().iter().position(|o| o.name == d.name && o.kind == d.kind)).collect()
            })
            .collect();
        let columns = if mixed {
            MIXED_COLUMNS.to_vec()
        } else {
            std::iter::once(TIME).chain(protos[present[0]].columns().iter().cloned()).collect()
        };
        let proto_field = schema.iter().position(|d| d.name == PROTO_FIELD.name).expect("added above");
        View { mixed, columns, schema, proto_field, fields }
    }
}

/// Messages of one or more protocols in a file: a `.dlt` file, or a
/// capture. See the module documentation.
pub struct Records {
    name: String,
    path: Option<PathBuf>,
    data: Bytes,
    index: Index,
    /// Channel ids, by [`Framed::channel`].
    channels: Vec<[u8; 4]>,
    protos: Vec<Arc<dyn Protocol>>,
    view: View,
    skipped: u64,
    data_id: usize,
}

impl Records {
    /// A stored file of protocol-0 messages, each behind a header.
    pub(crate) fn stored(
        name: String,
        path: Option<PathBuf>,
        data: Bytes,
        offsets: Vec<u64>,
        header: StoredHeader,
        proto: Arc<dyn Protocol>,
        skipped: u64,
    ) -> Records {
        let protos = vec![proto];
        let view = View::new(&protos, &[0]);
        let index = Index::Stored { offsets, header };
        Records { name, path, data, index, channels: Vec::new(), protos, view, skipped, data_id: new_data_id() }
    }

    /// Messages found inside a container (`data`, e.g. a mapped pcap):
    /// `msgs` in display order, `side` holding those placed there,
    /// `channels` the channel ids by [`Framed::channel`], `protos` by
    /// [`Framed::proto`]. `skipped` counts bytes of routed datagrams that
    /// weren't messages.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn framed(
        name: String,
        path: Option<PathBuf>,
        data: Bytes,
        msgs: Vec<Framed>,
        side: Vec<u8>,
        channels: Vec<[u8; 4]>,
        protos: Vec<Arc<dyn Protocol>>,
        skipped: u64,
    ) -> Records {
        let mut seen = [false; MAX_PROTOCOLS];
        msgs.iter().for_each(|m| seen[m.proto()] = true);
        let present: Vec<ProtoId> = (0..protos.len()).filter(|&p| seen[p]).collect();
        let view = View::new(&protos, &present);
        let index = Index::Framed { msgs, side };
        Records { name, path, data, index, channels, protos, view, skipped, data_id: new_data_id() }
    }

    /// The same messages with protocol `id`'s reader replaced (DLT with
    /// FIBEX descriptions).
    pub(crate) fn with_protocol(mut self, id: ProtoId, proto: Arc<dyn Protocol>) -> Records {
        if let Some(p) = self.protos.get_mut(id) {
            *p = proto;
        }
        self
    }

    pub(crate) fn protocol(&self, id: ProtoId) -> Option<&Arc<dyn Protocol>> {
        self.protos.get(id)
    }

    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    /// The file's bytes.
    #[cfg(test)]
    pub(crate) fn file_bytes(&self) -> &[u8] {
        self.data.bytes()
    }

    /// Bytes that were not part of any well-formed message.
    pub fn skipped_bytes(&self) -> u64 {
        self.skipped
    }

    /// The protocols of the messages, by name, each once.
    pub fn protocols(&self) -> Vec<&'static str> {
        match &self.index {
            Index::Stored { .. } => vec![self.protos[0].name()],
            Index::Framed { msgs, .. } => {
                let mut seen = [false; MAX_PROTOCOLS];
                msgs.iter().for_each(|m| seen[m.proto()] = true);
                (0..self.protos.len()).filter(|&p| seen[p]).map(|p| self.protos[p].name()).collect()
            }
        }
    }

    /// Message `line`: its protocol, bytes (to the end of its buffer) and
    /// origin.
    fn msg(&self, line: LineNo) -> Option<(ProtoId, &[u8], Origin<'_>)> {
        let i = usize::try_from(line).ok()?;
        match &self.index {
            Index::Stored { offsets, header } => {
                let buf = self.data.bytes().get(*offsets.get(i)? as usize..)?;
                let (origin, start) = header(buf)?;
                Some((0, buf.get(start..)?, origin))
            }
            Index::Framed { msgs, side } => {
                let f = msgs.get(i)?;
                let buf = if f.in_side() { side.as_slice() } else { self.data.bytes() };
                let channel = self.channels.get(f.channel()).unwrap_or(NO_CHANNEL);
                let mut msg = buf.get(f.offset()..)?;
                if !self.protos.get(f.proto())?.delimited() {
                    let (len, rest) = msg.split_first_chunk::<4>()?;
                    msg = rest.get(..u32::from_le_bytes(*len) as usize)?;
                }
                Some((f.proto(), msg, Origin { secs: f.secs, usecs: f.usecs, channel }))
            }
        }
    }

    /// The `info` and `payload` cells of a mixed view, from the protocol's
    /// own cells: the ones before the payload joined by spaces.
    fn mixed_cells(own: &str) -> (String, &str) {
        match own.rsplit_once('\t') {
            Some((head, payload)) => {
                (head.split('\t').filter(|c| !c.is_empty()).collect::<Vec<_>>().join(" "), payload)
            }
            None => (String::new(), own),
        }
    }
}

/// `YYYY-MM-DD HH:MM:SS.uuuuuu`, UTC or local time (`crate::time::set_local`).
pub(crate) fn write_time(out: &mut Vec<u8>, secs: u32, usecs: i32) {
    use std::io::Write;
    // Offsets are whole seconds.
    let secs = crate::time::shown(f64::from(secs)).max(0.0) as i64;
    let (y, m, d) = crate::time::civil_from_days(secs / 86_400);
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

impl LogSource for Records {
    fn data_id(&self) -> usize {
        self.data_id
    }

    fn name(&self) -> &str {
        &self.name
    }

    fn line_count(&self) -> LineNo {
        match &self.index {
            Index::Stored { offsets, .. } => offsets.len() as LineNo,
            Index::Framed { msgs, .. } => msgs.len() as LineNo,
        }
    }

    fn line_bytes(&self, line: LineNo) -> Option<Cow<'_, [u8]>> {
        let (p, msg, at) = self.msg(line)?;
        let proto = &self.protos[p];
        let mut out = Vec::with_capacity(80 + msg.len().min(4096) * 3);
        write_time(&mut out, at.secs, at.usecs);
        out.push(b'\t');
        if !self.view.mixed {
            proto.render(msg, &at, &mut out);
            return Some(Cow::Owned(out));
        }
        let mut own = Vec::new();
        proto.render(msg, &at, &mut own);
        let own = String::from_utf8_lossy(&own);
        let (info, payload) = Records::mixed_cells(&own);
        out.extend_from_slice(proto.name().as_bytes());
        out.push(b'\t');
        out.extend_from_slice(info.as_bytes());
        out.push(b'\t');
        out.extend_from_slice(payload.as_bytes());
        Some(Cow::Owned(out))
    }

    fn line_detail(&self, line: LineNo) -> Option<Vec<String>> {
        let (p, msg, at) = self.msg(line)?;
        let proto = &self.protos[p];
        let mut time = Vec::new();
        write_time(&mut time, at.secs, at.usecs);
        let time = String::from_utf8_lossy(&time).into_owned();
        let mut cells = proto.detail(msg, &at);
        if !self.view.mixed {
            cells.insert(0, time);
            return Some(cells);
        }
        let payload = cells.pop().unwrap_or_default();
        let info = cells.into_iter().filter(|c| !c.is_empty()).collect::<Vec<_>>().join(" ");
        Some(vec![time, proto.name().to_owned(), info, payload])
    }

    fn size_bytes(&self) -> u64 {
        self.data.bytes().len() as u64
    }

    fn index_memory_bytes(&self) -> usize {
        match &self.index {
            Index::Stored { offsets, .. } => offsets.capacity() * size_of::<u64>(),
            Index::Framed { msgs, side } => {
                msgs.capacity() * size_of::<Framed>() + side.capacity() + self.channels.capacity() * 4
            }
        }
    }

    fn columns(&self) -> &[ColumnDef] {
        &self.view.columns
    }

    fn schema(&self) -> &[FieldDef] {
        &self.view.schema
    }

    fn field(&self, line: LineNo, id: FieldId) -> Option<FieldValue<'_>> {
        let (p, msg, at) = self.msg(line)?;
        let proto = &self.protos[p];
        if id == self.view.proto_field {
            return Some(FieldValue::Str(proto.name().as_bytes().into()));
        }
        let own = (*self.view.fields.get(p)?.get(id)?)?;
        proto.field(msg, &at, own)
    }

    fn supplied_level(&self, line: LineNo) -> Option<Option<Level>> {
        let (p, msg, at) = self.msg(line)?;
        Some(self.protos[p].level(msg, &at))
    }

    fn time(&self, line: LineNo) -> Option<f64> {
        let (_, _, at) = self.msg(line)?;
        Some(f64::from(at.secs) + f64::from(at.usecs) / 1e6)
    }

    fn argument(&self, line: LineNo, index: usize) -> Option<crate::signal::SignalValue> {
        let (p, msg, at) = self.msg(line)?;
        self.protos[p].argument(msg, &at, index)
    }

    /// A `.dlt` file: the storage header's offset, the hash from there;
    /// a capture: the message's offset in the file (none in the side
    /// buffer), the hash of the message.
    fn anchor(&self, line: LineNo) -> Option<Anchor> {
        let i = usize::try_from(line).ok()?;
        match &self.index {
            Index::Stored { offsets, .. } => {
                let at = *offsets.get(i)?;
                let bytes = self.data.bytes().get(at as usize..)?;
                Some(Anchor::of(Some(at), &bytes[..bytes.len().min(ANCHOR_BYTES)]))
            }
            Index::Framed { msgs, .. } => {
                let f = msgs.get(i)?;
                let (_, msg, _) = self.msg(line)?;
                Some(Anchor::of((!f.in_side()).then_some(f.offset() as u64), msg))
            }
        }
    }

    fn lines_at(&self, offsets: &[u64]) -> Vec<Option<LineNo>> {
        match &self.index {
            Index::Stored { offsets: all, .. } => {
                offsets.iter().map(|o| all.binary_search(o).ok().map(|i| i as LineNo)).collect()
            }
            Index::Framed { msgs, .. } => {
                let mut want: std::collections::HashMap<u64, Option<LineNo>> =
                    offsets.iter().map(|&o| (o, None)).collect();
                for (i, f) in msgs.iter().enumerate().filter(|(_, f)| !f.in_side()) {
                    if let Some(slot) = want.get_mut(&(f.offset() as u64)) {
                        slot.get_or_insert(i as LineNo);
                    }
                }
                offsets.iter().map(|o| want[o]).collect()
            }
        }
    }
}

#[cfg(test)]
mod tests;
