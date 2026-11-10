//! DLT inside captures: pcap / pcapng network captures and MF4 bus logs.
//!
//! The layers of `doc/sources-and-signals.md`: a **container** turns the file
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
use crate::{Error, Progress, Result};
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
}

impl CaptureStats {
    /// What was dropped, for people (empty: nothing).
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
    read(path.display().to_string(), Some(path.to_path_buf()), data, options, progress)
}

/// In-memory capture, handy for tests.
pub fn from_bytes(
    name: impl Into<String>,
    bytes: Vec<u8>,
    options: &CaptureOptions,
) -> Result<(Records, CaptureStats)> {
    read(name.into(), None, Bytes::Owned(bytes), options, &Progress::new())
}

fn read(
    name: String,
    path: Option<std::path::PathBuf>,
    data: Bytes,
    options: &CaptureOptions,
    progress: &Progress,
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
    let protos = protocols(options)?;
    let rules = rules(options, &protos);
    let mut sink = Sink {
        net: Net::default(),
        c: Collector {
            base: bytes,
            delimited: protos.iter().map(|p| p.delimited()).collect(),
            protos,
            msgs: Vec::new(),
            side: Vec::new(),
            stats: CaptureStats::default(),
            found: Vec::new(),
            origins: OriginTable::default(),
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
        pcap::frames(buf, progress, &mut |fr| sink.link_frame(fr.link, fr.data, fr.secs, fr.usecs, fr.channel))?
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
        .with_notes(stats.notes(), stats.senders_not_kept);
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
        let (c, rules, too_far) = (&mut self.c, self.rules, &mut self.too_far);
        self.net.frame(link, data, secs, &mut |dg, payload| {
            if let Some(proto) = route(rules, dg) {
                *too_far |= !c.datagram(&payload, proto, channel, Some(dg), secs, usecs);
            }
        });
    }

    /// An Ethernet frame's payload with its EtherType (MF4 `ETH_Frame`).
    fn ether(&mut self, ethertype: u16, data: Cow<'a, [u8]>, secs: u32, usecs: i32, channel: usize) {
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
            Some(off) => (false, off),
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
