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
//! no value there (it only satisfies `!=` and `!~`). A capture adds
//! [`NET_FIELDS`], where each message's datagram came from ([`Origins`]).

pub mod ksy;
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
    fn name(&self) -> &str;

    /// Columns after `time`, as [`Self::render`] writes them; the last one
    /// is the payload.
    fn columns(&self) -> &[ColumnDef];

    /// The fields [`Self::field`] answers, by index.
    fn schema(&self) -> &[FieldDef];

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

    /// The writer's uptime in seconds, if the message records it
    /// ([`LogSource::uptime`]).
    fn uptime(&self, _msg: &[u8], _at: &Origin<'_>) -> Option<f64> {
        None
    }
}

/// A protocol given at runtime (a definition, [`ksy`]) and its default
/// rules: which UDP datagrams are its messages
/// ([`crate::input::capture::route`]).
#[derive(Clone)]
pub struct LoadedProtocol {
    pub protocol: Arc<dyn Protocol>,
    pub routes: Vec<crate::input::capture::route::Match>,
}

impl LoadedProtocol {
    /// A definition with the rules it gives itself (`-nerka: match:`).
    pub fn new(protocol: Arc<ksy::KsyProtocol>) -> LoadedProtocol {
        let routes = protocol.routes().to_vec();
        LoadedProtocol { protocol, routes }
    }
}

impl std::fmt::Debug for LoadedProtocol {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LoadedProtocol").field("name", &self.protocol.name()).field("routes", &self.routes).finish()
    }
}

/// The same reader (not an equal one) with the same rules.
impl PartialEq for LoadedProtocol {
    fn eq(&self, other: &Self) -> bool {
        std::ptr::addr_eq(Arc::as_ptr(&self.protocol), Arc::as_ptr(&other.protocol)) && self.routes == other.routes
    }
}

impl Eq for LoadedProtocol {}

/// Index of a protocol in a [`Records`] source.
pub type ProtoId = usize;

/// Protocols a source can hold (4 bits of [`Framed`]).
pub const MAX_PROTOCOLS: usize = 16;

/// Channels a capture can name: the first origins ([`Origins`]).
pub const MAX_CHANNELS: usize = 1 << 11;

/// Origins a capture can name (19 bits of [`Framed`]).
pub const MAX_ORIGINS: usize = 1 << 19;

/// Senders of UDP datagrams a capture keeps ([`Origins`]); messages from
/// more get no `net.*` fields. A capture with spoofed or garbled addresses
/// can't grow the table further (owner, 2026-11-08).
pub const MAX_SENDERS: usize = 1 << 16;

/// Largest offset of a message in a file or the side buffer (1 TiB).
pub const MAX_OFFSET: u64 = (1 << 40) - 1;

/// A message inside a container: where it starts, its protocol, where it
/// came from (an [`Origins`] id: the channel, and the sender of its
/// datagram) and the time the container gives it. 16 bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Framed {
    /// Bits 0–39: offset of the message, in the file or (bit 63) in the
    /// side buffer; bits 40–58: origin; bits 59–62: protocol.
    at: u64,
    secs: u32,
    usecs: i32,
}

const IN_SIDE: u64 = 1 << 63;
const ORIGIN_SHIFT: u32 = 40;
const PROTO_SHIFT: u32 = 59;

impl Framed {
    /// A message of protocol `proto` at `offset` (in the side buffer if
    /// `side`), from `origin` (an [`Origins`] id; a channel alone is its
    /// index into the source's channel ids), stored at `secs` + `usecs`.
    /// `None` if something doesn't fit.
    pub fn new(offset: u64, side: bool, proto: ProtoId, origin: usize, secs: u32, usecs: i32) -> Option<Framed> {
        if offset > MAX_OFFSET || origin >= MAX_ORIGINS || proto >= MAX_PROTOCOLS {
            return None;
        }
        let at = offset
            | ((origin as u64) << ORIGIN_SHIFT)
            | ((proto as u64) << PROTO_SHIFT)
            | if side { IN_SIDE } else { 0 };
        Some(Framed { at, secs, usecs })
    }

    pub(crate) fn offset(&self) -> usize {
        (self.at & MAX_OFFSET) as usize
    }

    pub(crate) fn in_side(&self) -> bool {
        self.at & IN_SIDE != 0
    }

    pub(crate) fn origin(&self) -> usize {
        ((self.at >> ORIGIN_SHIFT) as usize) & (MAX_ORIGINS - 1)
    }

    pub(crate) fn proto(&self) -> ProtoId {
        ((self.at >> PROTO_SHIFT) as usize) & (MAX_PROTOCOLS - 1)
    }
}

/// The fields a capture adds: where each message's UDP datagram came from
/// and went to (no value for messages without one: MF4 `DLT_Frame`, or a
/// sender past [`MAX_SENDERS`]). Not columns, and not in `raw`.
pub const NET_FIELDS: [FieldDef; 5] = [
    FieldDef::new("net.src", FieldKind::Str, false),
    FieldDef::new("net.dst", FieldKind::Str, false),
    FieldDef::new("net.sport", FieldKind::U64, false),
    FieldDef::new("net.port", FieldKind::U64, false),
    FieldDef::new("net.vlan", FieldKind::U64, false),
];

/// Where a capture's messages came from, by `Framed::origin`: an id below
/// [`MAX_CHANNELS`] is a channel alone (MF4 `DLT_Frame`, or a sender not
/// kept); the others are a channel and the sender of a UDP datagram (its
/// addresses, ports and VLAN), each pair and each sender kept once.
#[derive(Default)]
pub struct Origins {
    /// ECU id per channel, for messages that don't carry one.
    channels: Vec<[u8; 4]>,
    /// Origins from `MAX_CHANNELS` on: a channel and an index into `senders`.
    pairs: Vec<(u16, u32)>,
    senders: Vec<crate::input::capture::route::Datagram>,
    /// Each sender's source and destination address as text.
    texts: Vec<[String; 2]>,
}

impl Origins {
    pub(crate) fn new(
        channels: Vec<[u8; 4]>,
        pairs: Vec<(u16, u32)>,
        senders: Vec<crate::input::capture::route::Datagram>,
    ) -> Origins {
        let texts = senders.iter().map(|d| [addr_text(&d.src, d.v6), addr_text(&d.dst, d.v6)]).collect();
        Origins { channels, pairs, senders, texts }
    }

    /// The ECU id of `origin`'s channel.
    fn channel(&self, origin: usize) -> &[u8; 4] {
        let channel = match origin.checked_sub(MAX_CHANNELS) {
            None => Some(origin),
            Some(i) => self.pairs.get(i).map(|p| usize::from(p.0)),
        };
        channel.and_then(|c| self.channels.get(c)).unwrap_or(NO_CHANNEL)
    }

    /// `origin`'s sender, if one was kept.
    fn sender(&self, origin: usize) -> Option<usize> {
        let i = origin.checked_sub(MAX_CHANNELS)?;
        self.pairs.get(i).map(|p| p.1 as usize)
    }

    fn memory_bytes(&self) -> usize {
        self.channels.capacity() * 4
            + self.pairs.capacity() * 8
            + self.senders.capacity() * size_of::<crate::input::capture::route::Datagram>()
            + self.texts.iter().map(|t| t[0].capacity() + t[1].capacity() + 48).sum::<usize>()
    }
}

/// An IPv4 address (in the first 4 bytes) or an IPv6 one, as text.
fn addr_text(a: &[u8; 16], v6: bool) -> String {
    if v6 {
        std::net::Ipv6Addr::from(*a).to_string()
    } else {
        std::net::Ipv4Addr::new(a[0], a[1], a[2], a[3]).to_string()
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
    /// The id of the first of [`NET_FIELDS`], when the source has them.
    net: Option<FieldId>,
    /// Per protocol, per field of the source: the protocol's own field id.
    fields: Vec<Vec<Option<FieldId>>>,
}

/// The shared columns of a source with several protocols.
pub const MIXED_COLUMNS: [ColumnDef; 4] =
    [ColumnDef::new("time", 26), ColumnDef::new("proto", 6), ColumnDef::new("info", 46), ColumnDef::new("payload", 0)];

const TIME: ColumnDef = ColumnDef::new("time", 26);
/// The protocol's name; compared without case (`proto == syslog`, `SYSLOG`).
pub(crate) const PROTO_FIELD: FieldDef = FieldDef::new("proto", FieldKind::Name, false);

impl View {
    /// The view for the protocols `present` (indexes into `protos`).
    fn new(protos: &[Arc<dyn Protocol>], present: &[ProtoId], net: bool) -> View {
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
        let net = net.then(|| {
            let first = schema.len();
            schema.extend(NET_FIELDS);
            first
        });
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
        View { mixed, columns, schema, proto_field, net, fields }
    }
}

/// Messages of one or more protocols in a file: a `.dlt` file, or a
/// capture. See the module documentation.
pub struct Records {
    name: String,
    path: Option<PathBuf>,
    data: Bytes,
    index: Index,
    /// Channels and senders, by [`Framed::origin`].
    origins: Origins,
    protos: Vec<Arc<dyn Protocol>>,
    view: View,
    skipped: u64,
    /// What reading it dropped (a capture's: [`super::capture::CaptureStats::notes`]),
    /// and how many messages have no sender.
    notes: Vec<String>,
    no_sender: u64,
    /// The protocols its messages are of, found once when it is read.
    present: Vec<ProtoId>,
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
        let view = View::new(&protos, &[0], false);
        let index = Index::Stored { offsets, header };
        Records {
            name,
            path,
            data,
            index,
            origins: Origins::default(),
            protos,
            view,
            skipped,
            notes: Vec::new(),
            no_sender: 0,
            present: vec![0],
            data_id: new_data_id(),
        }
    }

    /// Messages found inside a container (`data`, e.g. a mapped pcap):
    /// `msgs` in display order, `side` holding those placed there,
    /// `origins` the channels and senders by [`Framed::origin`], `protos` by
    /// [`Framed::proto`]. `skipped` counts bytes of routed datagrams that
    /// weren't messages.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn framed(
        name: String,
        path: Option<PathBuf>,
        data: Bytes,
        msgs: Vec<Framed>,
        side: Vec<u8>,
        origins: Origins,
        protos: Vec<Arc<dyn Protocol>>,
        skipped: u64,
    ) -> Records {
        let mut seen = [false; MAX_PROTOCOLS];
        msgs.iter().for_each(|m| seen[m.proto()] = true);
        let present: Vec<ProtoId> = (0..protos.len()).filter(|&p| seen[p]).collect();
        let view = View::new(&protos, &present, !origins.senders.is_empty());
        let index = Index::Framed { msgs, side };
        let (notes, no_sender, data_id) = (Vec::new(), 0, new_data_id());
        Records { name, path, data, index, origins, protos, view, skipped, notes, no_sender, present, data_id }
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

    /// Bytes copied out of the file (reassembled or split messages).
    #[cfg(test)]
    pub(crate) fn side_bytes(&self) -> usize {
        match &self.index {
            Index::Framed { side, .. } => side.len(),
            Index::Stored { .. } => 0,
        }
    }

    /// Bytes that were not part of any well-formed message.
    pub fn skipped_bytes(&self) -> u64 {
        self.skipped
    }

    /// With what reading it dropped (`notes`), and how many messages have no
    /// sender.
    pub(crate) fn with_notes(mut self, notes: Vec<String>, no_sender: u64) -> Records {
        self.notes = notes;
        self.no_sender = no_sender;
        self
    }

    /// The protocols of the messages, by name, each once.
    pub fn protocols(&self) -> Vec<&str> {
        self.present.iter().map(|&p| self.protos[p].name()).collect()
    }

    /// Field `k` of [`NET_FIELDS`] for message `line`.
    fn net_field(&self, line: LineNo, k: usize) -> Option<FieldValue<'_>> {
        let Index::Framed { msgs, .. } = &self.index else { return None };
        let s = self.origins.sender(msgs.get(usize::try_from(line).ok()?)?.origin())?;
        let d = self.origins.senders.get(s)?;
        Some(match k {
            0 => FieldValue::Str(self.origins.texts[s][0].as_bytes().into()),
            1 => FieldValue::Str(self.origins.texts[s][1].as_bytes().into()),
            2 => FieldValue::U64(d.src_port.into()),
            3 => FieldValue::U64(d.dst_port.into()),
            _ if d.vlan[0] != 0 => FieldValue::U64(d.vlan[0].into()),
            _ => return None,
        })
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
                let channel = self.origins.channel(f.origin());
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
    pub(crate) fn mixed_cells(own: &str) -> (String, &str) {
        match own.rsplit_once('\t') {
            Some((head, payload)) => {
                (head.split('\t').filter(|c| !c.is_empty()).collect::<Vec<_>>().join(" "), payload)
            }
            None => (String::new(), own),
        }
    }
}

/// The kind of capture file `data` is, by its first bytes.
fn container(data: &[u8]) -> &'static str {
    match data.get(..8).unwrap_or(data) {
        [0x0a, 0x0d, 0x0d, 0x0a, ..] => "pcapng",
        [0xd4, 0xc3, 0xb2, 0xa1, ..] | [0xa1, 0xb2, 0xc3, 0xd4, ..] => "pcap",
        [0x4d, 0x3c, 0xb2, 0xa1, ..] | [0xa1, 0xb2, 0x3c, 0x4d, ..] => "pcap",
        b"MDF     " | b"UnFinMF " => "MF4",
        _ => "capture",
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

    fn info(&self) -> super::SourceInfo {
        let mut notes = self.notes.clone();
        // A stored file (`.dlt`): bytes between messages (a capture's are
        // in its notes already).
        if matches!(self.index, Index::Stored { .. }) && self.skipped > 0 {
            notes.push(format!("{} bytes weren't {} messages: skipped", self.skipped, self.protos[0].name()));
        }
        super::SourceInfo {
            protocols: self.protocols().into_iter().map(str::to_owned).collect(),
            notes,
            no_sender: self.no_sender,
        }
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

    fn format(&self) -> String {
        let names =
            |present: &[ProtoId]| present.iter().map(|&p| self.protos[p].name()).collect::<Vec<_>>().join(" + ");
        match &self.index {
            Index::Stored { .. } => format!("{} file", self.protos[0].name()),
            Index::Framed { msgs, .. } => {
                let mut seen = [false; MAX_PROTOCOLS];
                msgs.iter().for_each(|m| seen[m.proto()] = true);
                let present: Vec<ProtoId> = (0..self.protos.len()).filter(|&p| seen[p]).collect();
                let inside = if present.is_empty() { "no messages".to_owned() } else { names(&present) };
                format!("{}: {inside}", container(self.data.bytes()))
            }
        }
    }

    fn index_memory_bytes(&self) -> usize {
        match &self.index {
            Index::Stored { offsets, .. } => offsets.capacity() * size_of::<u64>(),
            Index::Framed { msgs, side } => {
                msgs.capacity() * size_of::<Framed>() + side.capacity() + self.origins.memory_bytes()
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
        if let Some(first) = self.view.net
            && (first..first + NET_FIELDS.len()).contains(&id)
        {
            return self.net_field(line, id - first);
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

    fn uptime(&self, line: LineNo) -> Option<f64> {
        let (p, msg, at) = self.msg(line)?;
        self.protos[p].uptime(msg, &at)
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
