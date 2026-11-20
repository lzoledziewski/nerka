//! DLT inside captures: pcap / pcapng network captures and MF4 bus logs.
//!
//! The layers of `docs/dev/sources-and-signals.md`: a **container** turns the file
//! into frames ([`pcap`], [`mf4`]), the **transport** turns frames into UDP
//! payloads ([`net`]: VLAN, IPv4 / IPv6 with reassembly), a **router**
//! picks each payload's protocol by rules over its ports, addresses and
//! VLAN tags ([`route`], [`CaptureOptions`]: DLT, SOME/IP, syslog and
//! loaded definitions), and the protocol
//! ([`super::proto::Protocol`]) splits it into messages, indexed where they
//! are: in the mapped file, or, for bytes that aren't contiguous there
//! (reassembled datagrams, MF4 records spanning data blocks), in a side
//! buffer ([`Records`] with [`Framed`] entries). Nothing is copied to a
//! temporary file; only compressed MF4 data is unpacked first, into a
//! cache file that is mapped the same way ([`unpack`]).
//!
//! A message gets the capture time of the frame that completed its
//! datagram, and keeps the capture's order. Its ECU is its own (WEID), else
//! the one the container knows for its channel (MF4 `DLT_Frame` ECU ids).

pub mod mf4;
pub mod net;
pub mod pcap;
pub mod route;
pub mod unpack;

use super::Bytes;
use super::dlt::Dlt;
use super::proto::{
    Framed, LoadedProtocol, MAX_CHANNELS, MAX_ORIGINS, MAX_PROTOCOLS, MAX_SENDERS, Origins, ProtoId, Protocol, Records,
    someip::SomeIp, syslog::Syslog,
};
use super::{Skip, SkipKind, Skips};
use crate::{Error, LineNo, Progress, Result};
use net::{Net, NetStats, Payload};
use route::{Datagram, Match, Route};
use std::borrow::Cow;
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
pub use unpack::{UnpackOptions, cache_usage, clear_cache};

/// UDP destination ports that carry DLT by default: 3490, the port the
/// DLT protocol specification (AUTOSAR PRS) assigns. Others are configured
/// ([`CaptureOptions::dlt_udp_ports`]).
pub const DEFAULT_DLT_PORTS: [u16; 1] = [3490];

/// UDP destination ports that carry SOME/IP by default: 30490, SOME/IP
/// service discovery's. Services use ports of their own: configure them
/// ([`CaptureOptions::someip_udp_ports`]).
pub const DEFAULT_SOMEIP_PORTS: [u16; 1] = [30490];

/// UDP destination ports that carry syslog by default: 514 (RFC 5426).
pub const DEFAULT_SYSLOG_PORTS: [u16; 1] = [514];

/// The protocols of a capture, by [`ProtoId`]: DLT, SOME/IP, syslog, then
/// the loaded ones ([`CaptureOptions::protocols`]) in their order.
pub const DLT: ProtoId = 0;
pub const SOMEIP: ProtoId = 1;
pub const SYSLOG: ProtoId = 2;
const NATIVE: usize = 3;

fn protocols(options: &CaptureOptions) -> Result<Vec<Arc<dyn Protocol>>> {
    let mut all: Vec<Arc<dyn Protocol>> = vec![Arc::new(Dlt::default()), Arc::new(SomeIp), Arc::new(Syslog)];
    all.extend(options.protocols.iter().map(|p| p.protocol.clone()));
    if all.len() > MAX_PROTOCOLS {
        return Err(Error::Format(format!(
            "too many protocols: {} loaded, at most {}",
            all.len() - NATIVE,
            MAX_PROTOCOLS - NATIVE
        )));
    }
    Ok(all)
}

/// How messages are found in a capture.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaptureOptions {
    /// UDP destination ports whose payloads are DLT messages.
    pub dlt_udp_ports: Vec<u16>,
    /// UDP destination ports whose payloads are SOME/IP messages.
    pub someip_udp_ports: Vec<u16>,
    /// UDP destination ports whose payloads are syslog messages.
    pub syslog_udp_ports: Vec<u16>,
    /// Protocols given at runtime (definitions), each with its default
    /// rules (its own `-nerka: match:`).
    pub protocols: Vec<LoadedProtocol>,
    /// Rules by protocol name. A protocol named here is matched by these
    /// rules instead of its defaults (the port lists above for the native
    /// ones, a definition's own).
    pub routes: Vec<Route>,
    /// Protocols turned off, by name: their datagrams are not read.
    pub disabled: Vec<String>,
    /// Where compressed MF4 data is unpacked.
    pub unpack: UnpackOptions,
    /// An MF4 file whose header says its start time is local time (the
    /// "local time" flag) is read in this computer's zone; off: as UTC.
    pub mf4_local_time: bool,
}

impl CaptureOptions {
    /// For a report: the ports, the protocol definitions, the rules and
    /// what is turned off.
    pub fn describe(&self) -> String {
        let ports = |p: &[u16]| p.iter().map(u16::to_string).collect::<Vec<_>>().join(",");
        let mut out = format!(
            "DLT ports {}; SOME/IP ports {}; syslog ports {}",
            ports(&self.dlt_udp_ports),
            ports(&self.someip_udp_ports),
            ports(&self.syslog_udp_ports)
        );
        for p in &self.protocols {
            let rules: Vec<String> = p.routes.iter().map(|r| r.to_string()).collect();
            out.push_str(&format!("; definition {} ({})", p.protocol.name(), rules.join(" ")));
        }
        for r in &self.routes {
            out.push_str(&format!("; rule {}:{}", r.protocol, r.rule));
        }
        if !self.disabled.is_empty() {
            out.push_str(&format!("; off: {}", self.disabled.join(",")));
        }
        out
    }
}

impl Default for CaptureOptions {
    fn default() -> Self {
        Self {
            dlt_udp_ports: DEFAULT_DLT_PORTS.to_vec(),
            someip_udp_ports: DEFAULT_SOMEIP_PORTS.to_vec(),
            syslog_udp_ports: DEFAULT_SYSLOG_PORTS.to_vec(),
            protocols: Vec::new(),
            routes: Vec::new(),
            disabled: Vec::new(),
            unpack: UnpackOptions::default(),
            mf4_local_time: true,
        }
    }
}

/// What importing a capture found.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct CaptureStats {
    pub net: NetStats,
    /// Datagrams on a port of a protocol, or MF4 DLT frames.
    pub datagrams: u64,
    /// Messages in them, of every protocol.
    pub messages: u64,
    /// Bytes of those datagrams that weren't whole messages.
    pub msg_skipped: u64,
    /// Bytes of the file that couldn't be read as records.
    pub file_skipped: u64,
    /// Messages whose datagram's sender wasn't kept (more than
    /// [`MAX_SENDERS`] senders): they have no `net.*` fields.
    pub senders_not_kept: u64,
    /// What a pcapng file's interface statistics say the capture dropped
    /// (none: a pcap or MF4 file, which can't say; empty: a pcapng without
    /// statistics).
    pub drops: Option<Vec<pcap::Drops>>,
}

impl CaptureStats {
    /// What was dropped, for people (empty: nothing).
    /// What the capture's own statistics say it dropped, for a report:
    /// whether what is missing was lost before the capture or by it.
    pub fn drops_report(&self) -> String {
        let Some(drops) = &self.drops else {
            return "capture drops: unknown (pcap and MF4 files don't record them)".into();
        };
        if drops.is_empty() {
            return "capture drops: unknown (no interface statistics in this pcapng)".into();
        }
        let n = |v: Option<u64>| v.map_or_else(|| "?".into(), |v| v.to_string());
        let each: Vec<String> = drops
            .iter()
            .map(|d| {
                format!(
                    "interface {}: received {}, dropped {} by the interface, {} by the system",
                    d.interface,
                    n(d.received),
                    n(d.by_interface),
                    n(d.by_system)
                )
            })
            .collect();
        let total: u64 = drops.iter().map(pcap::Drops::total).sum();
        let counted = drops.iter().any(|d| d.by_interface.is_some() || d.by_system.is_some());
        let verdict = if !counted {
            "unknown: the statistics don't count drops"
        } else if total == 0 {
            "none: what is missing was lost before the capture saw it"
        } else {
            "some: what is missing may have been dropped by the capture"
        };
        format!("capture drops: {verdict} ({})", each.join("; "))
    }

    pub fn notes(&self) -> Vec<String> {
        let mut out = Vec::new();
        if self.msg_skipped > 0 {
            out.push(format!("{} bytes of datagrams weren't whole messages: skipped", self.msg_skipped));
        }
        if self.file_skipped > 0 {
            out.push(format!("{} bytes of the file couldn't be read: skipped", self.file_skipped));
        }
        if self.net.incomplete > 0 {
            out.push(format!("{} datagrams lost a fragment: not read", self.net.incomplete));
        }
        let dropped: u64 = self.drops.iter().flatten().map(pcap::Drops::total).sum();
        if dropped > 0 {
            out.push(format!(
                "the capture itself dropped {dropped} packets (its interface statistics): they aren't in the file"
            ));
        }
        if self.senders_not_kept > 0 {
            out.push(format!(
                "{} messages have no net.* fields: more than {MAX_SENDERS} senders",
                self.senders_not_kept
            ));
        }
        out
    }
}

/// Does `head` (the first bytes of a file) look like a capture this module
/// reads?
pub fn is_capture(head: &[u8]) -> bool {
    pcap::is_pcap(head) || pcap::is_pcapng(head) || mf4::is_mf4(head)
}

/// Open a capture and index the messages in it.
pub fn open(path: impl AsRef<Path>, options: &CaptureOptions, progress: &Progress) -> Result<(Records, CaptureStats)> {
    let path = path.as_ref();
    let data = Bytes::map(path)?;
    read(path.display().to_string(), Some(path.to_path_buf()), data, options, progress, Skips::default())
}

/// Read a capture again for its skips only, each into `skips` (an export:
/// none kept); returns what it found.
pub(crate) fn read_skips(
    path: &Path,
    options: &CaptureOptions,
    progress: &Progress,
    skips: Skips,
) -> Result<CaptureStats> {
    let data = Bytes::map(path)?;
    Ok(read(path.display().to_string(), Some(path.to_path_buf()), data, options, progress, skips)?.1)
}

/// In-memory capture, handy for tests.
pub fn from_bytes(
    name: impl Into<String>,
    bytes: Vec<u8>,
    options: &CaptureOptions,
) -> Result<(Records, CaptureStats)> {
    read(name.into(), None, Bytes::Owned(bytes), options, &Progress::new(), Skips::default())
}

fn read(
    name: String,
    path: Option<std::path::PathBuf>,
    data: Bytes,
    options: &CaptureOptions,
    progress: &Progress,
    mut skips: Skips,
) -> Result<(Records, CaptureStats)> {
    let file = data.bytes();
    // Compressed MF4 data: unpacked, and the messages point into that.
    let unpacked = if mf4::is_mf4(file) {
        unpack::unpack(file, path.as_deref(), &mf4::leaves(file)?, &options.unpack, progress)?
    } else {
        None
    };
    let (bytes, layout) = match &unpacked {
        Some((b, l)) => (b.bytes(), Some(l)),
        None => (file, None),
    };
    super::prefault(bytes, progress);
    let protos = protocols(options)?;
    let rules = rules(options, &protos);
    let mut sink = Sink {
        net: Net::with_skips(skips.share()),
        c: Collector {
            base: bytes,
            delimited: protos.iter().map(|p| p.delimited()).collect(),
            protos,
            msgs: Vec::new(),
            side: Vec::new(),
            stats: CaptureStats::default(),
            found: Vec::new(),
            origins: OriginTable::default(),
            sized: false,
            skips: skips.share(),
            frame: None,
            cut: None,
            file_offsets: unpacked.is_none(),
        },
        options,
        rules: &rules,
        channel_ecus: Vec::new(),
        too_far: false,
    };
    let file_skipped = if mf4::is_mf4(file) {
        if layout.is_none() {
            progress.set_total(file.len() as u64);
        }
        mf4::read(file, bytes, layout, progress, &mut sink)?
    } else {
        let buf = file;
        let read = pcap::frames(buf, progress, &mut |fr| {
            sink.c.frame = Some(fr.number);
            sink.c.cut = (fr.wire_len > fr.data.len()).then_some((fr.data.len() as u64, fr.wire_len as u64));
            sink.link_frame(fr.link, fr.data, fr.secs, fr.usecs, fr.channel)
        })?;
        sink.c.stats.drops = read.drops;
        read.skipped
    };
    if sink.too_far {
        return Err(Error::Format("capture too large to index (offsets over 1 TiB)".into()));
    }
    sink.net.finish();
    let Sink { net, c, channel_ecus, .. } = sink;
    let protos = c.protos;
    let mut stats = c.stats;
    stats.net = net.stats;
    stats.file_skipped = file_skipped;
    skips.append(c.skips);
    skips.append(net.skips);
    if file_skipped > 0 {
        // What follows can't be read: the rest of a pcap / pcapng file (an
        // MF4's are blocks here and there).
        let mf4 = mf4::is_mf4(file);
        let at = file.len().saturating_sub(file_skipped as usize);
        skips.push(Skip {
            kind: SkipKind::Unreadable,
            line: None,
            time: None,
            frame: None,
            offset: (!mf4).then_some(at as u64),
            bytes: file_skipped,
            detail: if mf4 { "MF4 blocks".into() } else { "the rest of the file".into() },
            file: String::new(),
            head: if mf4 { Vec::new() } else { Skip::head_of(&file[at..]) },
            cut: None,
        });
    }
    skips.finish();
    let (mut msgs, mut side) = (c.msgs, c.side);
    msgs.shrink_to_fit();
    side.shrink_to_fit();
    let skipped = stats.msg_skipped;
    let origins = Origins::new(channel_ecus, c.origins.pairs, c.origins.senders);
    let data = match unpacked {
        Some((b, _)) => b,
        None => data,
    };
    let records = Records::framed(name, path, data, msgs, side, origins, protos, skipped)
        .with_notes(stats.notes(), stats.senders_not_kept)
        .with_about(vec![stats.drops_report()])
        .with_skips(skips);
    Ok((records, stats))
}

/// Where containers deliver frames: transport, router and protocols.
pub(crate) struct Sink<'a, 'o> {
    net: Net,
    c: Collector<'a>,
    options: &'o CaptureOptions,
    rules: &'o [(ProtoId, Match)],
    /// ECU id per channel, for messages that don't carry one.
    channel_ecus: Vec<[u8; 4]>,
    too_far: bool,
}

impl<'a> Sink<'a, '_> {
    /// A link-layer frame (pcap): transport, then the router.
    fn link_frame(&mut self, link: u32, data: &'a [u8], secs: u32, usecs: i32, channel: usize) {
        self.net.at = (self.c.frame, self.c.msgs.len() as LineNo);
        let (c, rules, too_far) = (&mut self.c, self.rules, &mut self.too_far);
        self.net.frame(link, data, secs, &mut |dg, payload| {
            if let Some(proto) = route(rules, dg) {
                *too_far |= !c.datagram(&payload, proto, channel, Some(dg), secs, usecs);
            }
        });
    }

    /// An Ethernet frame's payload with its EtherType (MF4 `ETH_Frame`).
    fn ether(&mut self, ethertype: u16, data: Cow<'a, [u8]>, secs: u32, usecs: i32, channel: usize) {
        self.net.at = (None, self.c.msgs.len() as LineNo);
        let (c, rules, too_far) = (&mut self.c, self.rules, &mut self.too_far);
        match data {
            Cow::Borrowed(d) => self.net.ether_frame(ethertype, d, secs, &mut |dg, payload| {
                if let Some(proto) = route(rules, dg) {
                    *too_far |= !c.datagram(&payload, proto, channel, Some(dg), secs, usecs);
                }
            }),
            // Not in the file: whatever comes out of it is copied.
            Cow::Owned(v) => self.net.ether_frame(ethertype, &v, secs, &mut |dg, payload| {
                if let Some(proto) = route(rules, dg) {
                    let p = Payload::Assembled(payload.bytes().to_vec());
                    *too_far |= !c.datagram(&p, proto, channel, Some(dg), secs, usecs);
                }
            }),
        }
    }

    /// DLT messages carried directly (MF4 `DLT_Frame`).
    fn dlt(&mut self, data: Cow<'a, [u8]>, secs: u32, usecs: i32, channel: usize) {
        let p = match data {
            Cow::Borrowed(d) => Payload::InFrame(d),
            Cow::Owned(v) => Payload::Assembled(v),
        };
        self.too_far |= !self.c.datagram(&p, DLT, channel, None, secs, usecs);
    }

    /// The channel whose fallback ECU id is `ecu` (made on first use).
    fn channel_for_ecu(&mut self, ecu: [u8; 4]) -> usize {
        if let Some(i) = self.channel_ecus.iter().position(|e| *e == ecu) {
            return i;
        }
        if self.channel_ecus.len() + 1 >= MAX_CHANNELS {
            return MAX_CHANNELS - 1;
        }
        self.channel_ecus.push(ecu);
        self.channel_ecus.len() - 1
    }
}

/// Every enabled protocol's rules, most specific first (ties: the
/// protocol listed first). A protocol's rules are the ones
/// [`CaptureOptions::routes`] gives it, else its defaults.
fn rules(options: &CaptureOptions, protos: &[Arc<dyn Protocol>]) -> Vec<(ProtoId, Match)> {
    let mut out = Vec::new();
    for (id, p) in protos.iter().enumerate() {
        let name = p.name();
        if options.disabled.iter().any(|d| d.eq_ignore_ascii_case(name)) {
            continue;
        }
        let user: Vec<Match> =
            options.routes.iter().filter(|r| r.protocol.eq_ignore_ascii_case(name)).map(|r| r.rule.clone()).collect();
        let given = !user.is_empty();
        let rules = if given {
            user
        } else {
            match id {
                DLT => options.dlt_udp_ports.iter().map(|&p| Match::port(p)).collect(),
                SOMEIP => options.someip_udp_ports.iter().map(|&p| Match::port(p)).collect(),
                SYSLOG => options.syslog_udp_ports.iter().map(|&p| Match::port(p)).collect(),
                _ => options.protocols.get(id - NATIVE).map(|l| l.routes.clone()).unwrap_or_default(),
            }
        };
        out.extend(rules.into_iter().map(|r| (r.specificity(), given, id, r)));
    }
    // More specific first; of equally specific rules, the user's before
    // defaults (a rule someone wrote never loses silently to a default
    // one), then the protocols' order (the sort is stable).
    out.sort_by_key(|(spec, given, _, _)| std::cmp::Reverse((*spec, *given)));
    out.into_iter().map(|(_, _, id, r)| (id, r)).collect()
}

/// The protocol of a UDP datagram: the first rule that matches it.
fn route(rules: &[(ProtoId, Match)], dg: &Datagram) -> Option<ProtoId> {
    rules.iter().find(|(_, r)| r.matches(dg)).map(|(p, _)| *p)
}

/// The protocols' side: indexes the messages of each routed datagram.
struct Collector<'a> {
    /// The mapped file, to turn payload slices into file offsets.
    base: &'a [u8],
    protos: Vec<Arc<dyn Protocol>>,
    msgs: Vec<Framed>,
    side: Vec<u8>,
    stats: CaptureStats,
    /// Per protocol, whether its messages say where they end.
    delimited: Vec<bool>,
    /// Where `split` found the messages of the datagram at hand: one list
    /// for all datagrams (an MF4 log has one per message).
    found: Vec<usize>,
    origins: OriginTable,
    /// `msgs` was sized for the whole file (from its first messages).
    sized: bool,
    /// Bytes of datagrams that weren't messages, one by one.
    skips: Skips,
    /// The number of the frame at hand in a pcap / pcapng file.
    frame: Option<u64>,
    /// The frame at hand was cut by the capture: bytes kept, on the wire.
    cut: Option<(u64, u64)>,
    /// `base` is the file (not unpacked MF4 data): its offsets are the file's.
    file_offsets: bool,
}

/// Builds a capture's [`Origins`]: each sender and each (channel, sender)
/// once. The table is keyed with std's randomly seeded hash, so a crafted
/// capture can't make its lookups collide; at [`MAX_SENDERS`] senders (or
/// [`MAX_ORIGINS`] pairs) new ones aren't kept and their messages carry
/// their channel alone.
#[derive(Default)]
struct OriginTable {
    pairs: Vec<(u16, u32)>,
    senders: Vec<Datagram>,
    sender_ids: HashMap<Datagram, u32>,
    pair_ids: HashMap<(u16, u32), usize>,
    /// The last datagram's channel, sender and origin: consecutive
    /// datagrams mostly come from the same sender.
    last: Option<(usize, Datagram, usize)>,
}

impl OriginTable {
    /// The origin of a datagram from `sender` on `channel`; the channel
    /// alone if it has no sender or the table is full.
    fn get(&mut self, channel: usize, sender: Option<&Datagram>) -> usize {
        let Some(dg) = sender else { return channel };
        if let Some((c, d, o)) = &self.last
            && *c == channel
            && d == dg
        {
            return *o;
        }
        let o = self.find(channel, dg);
        self.last = Some((channel, *dg, o));
        o
    }

    fn find(&mut self, channel: usize, dg: &Datagram) -> usize {
        let s = match self.sender_ids.get(dg) {
            Some(&s) => s,
            None if self.senders.len() >= MAX_SENDERS => return channel,
            None => {
                let s = self.senders.len() as u32;
                self.senders.push(*dg);
                self.sender_ids.insert(*dg, s);
                s
            }
        };
        let key = (channel as u16, s);
        if let Some(&o) = self.pair_ids.get(&key) {
            return o;
        }
        let o = MAX_CHANNELS + self.pairs.len();
        if o >= MAX_ORIGINS {
            return channel;
        }
        self.pairs.push(key);
        self.pair_ids.insert(key, o);
        o
    }
}

impl Collector<'_> {
    /// The last `skipped` bytes of `bytes`, a datagram of `proto` whose
    /// `messages` messages before them were read: one skip, with why.
    #[allow(clippy::too_many_arguments)]
    fn skip(
        &mut self,
        bytes: &[u8],
        skipped: usize,
        proto: ProtoId,
        sender: Option<&Datagram>,
        messages: usize,
        secs: u32,
        usecs: i32,
    ) {
        let start = bytes.len() - skipped;
        let rest = &bytes[start..];
        let name = self.protos[proto].name();
        let mut detail = match sender {
            Some(d) => format!("{name} {}", d.describe()),
            None => name.to_owned(),
        };
        detail.push_str(&format!(", datagram of {} bytes, {messages} messages read before", bytes.len()));
        if proto == DLT {
            detail.push_str(&format!("; {}", crate::input::dlt::why_not_message(rest)));
        }
        if let Some((kept, wire)) = self.cut {
            detail.push_str(&format!("; the capture cut its frame: {kept} of {wire} bytes kept (its snap length)"));
        }
        // Within the file (not copied out of it): where.
        let base = self.base.as_ptr() as usize;
        let at = rest.as_ptr() as usize;
        let offset = (self.file_offsets && at >= base && at < base + self.base.len()).then(|| (at - base) as u64);
        self.skips.push(Skip {
            kind: SkipKind::NotMessages,
            line: Some((self.msgs.len() + messages) as LineNo),
            time: Some(f64::from(secs) + f64::from(usecs) / 1e6),
            frame: self.frame,
            offset,
            bytes: skipped as u64,
            detail,
            file: String::new(),
            head: Skip::head_of(rest),
            cut: self.cut,
        });
    }

    /// Index the messages of one datagram of protocol `proto`, from
    /// `sender` (`None`: not a UDP datagram). Returns false if an offset
    /// doesn't fit the index.
    fn datagram(
        &mut self,
        payload: &Payload<'_>,
        proto: ProtoId,
        channel: usize,
        sender: Option<&Datagram>,
        secs: u32,
        usecs: i32,
    ) -> bool {
        let mut found = std::mem::take(&mut self.found);
        found.clear();
        let ok = self.index(payload, proto, channel, sender, secs, usecs, &mut found);
        self.found = found;
        ok
    }

    #[allow(clippy::too_many_arguments)]
    fn index(
        &mut self,
        payload: &Payload<'_>,
        proto: ProtoId,
        channel: usize,
        sender: Option<&Datagram>,
        secs: u32,
        usecs: i32,
        found: &mut Vec<usize>,
    ) -> bool {
        self.stats.datagrams += 1;
        let bytes = payload.bytes();
        let skipped = self.protos[proto].split(bytes, found);
        self.stats.msg_skipped += skipped as u64;
        if skipped > 0 {
            self.skip(bytes, skipped, proto, sender, found.len(), secs, usecs);
        }
        if found.is_empty() {
            return true;
        }
        // Only datagrams that hold messages get an origin of their own.
        let channel = channel.min(MAX_CHANNELS - 1);
        let origin = self.origins.get(channel, sender);
        if sender.is_some() && origin < MAX_CHANNELS {
            self.stats.senders_not_kept += found.len() as u64;
        }
        if !self.delimited[proto] {
            // Each message behind its length, in the side buffer.
            let ends: Vec<usize> = found.iter().skip(1).copied().chain([bytes.len() - skipped]).collect();
            for (&at, end) in found.iter().zip(ends) {
                let Some(f) = Framed::new(self.side.len() as u64, true, proto, origin, secs, usecs) else {
                    return false;
                };
                self.side.extend_from_slice(&((end - at) as u32).to_le_bytes());
                self.side.extend_from_slice(&bytes[at..end]);
                self.msgs.push(f);
                self.stats.messages += 1;
            }
            return true;
        }
        // Where the payload is: in the file, or appended to the side buffer.
        let start = self.base.as_ptr() as usize;
        let in_file = match payload {
            Payload::InFrame(b) => {
                let p = b.as_ptr() as usize;
                (p >= start && p + b.len() <= start + self.base.len()).then(|| p - start)
            }
            Payload::Assembled(_) => None,
        };
        let (in_side, base) = match in_file {
            Some(off) => {
                // Once a few thousand messages are in, the index is sized
                // for the whole file at their rate (review 2026-11-14, P8:
                // growing it by doubling copied it again and again).
                if !self.sized && self.msgs.len() >= 4096 && off > 0 {
                    self.sized = true;
                    let expected = ((self.msgs.len() as f64 * self.base.len() as f64 / off as f64 * 1.05) as usize)
                        .min(self.base.len() / 16);
                    self.msgs.reserve_exact(expected.saturating_sub(self.msgs.len()));
                }
                (false, off)
            }
            None => {
                let at = self.side.len();
                self.side.extend_from_slice(bytes);
                (true, at)
            }
        };
        for &at in found.iter() {
            let Some(f) = Framed::new((base + at) as u64, in_side, proto, origin, secs, usecs) else { return false };
            self.msgs.push(f);
            self.stats.messages += 1;
        }
        true
    }
}

#[cfg(test)]
mod tests;
