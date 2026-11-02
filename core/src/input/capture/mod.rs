//! DLT inside captures: pcap / pcapng network captures and MF4 bus logs.
//!
//! The layers of `doc/sources-and-signals.md`: a **container** turns the file
//! into frames ([`pcap`], [`mf4`]), the **transport** turns frames into UDP
//! payloads ([`net`]: VLAN, IPv4 / IPv6 with reassembly), a **router**
//! picks each payload's protocol by its UDP destination port
//! ([`CaptureOptions`]: DLT, SOME/IP, syslog), and the protocol
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
pub mod unpack;

use super::Bytes;
use super::dlt::Dlt;
use super::proto::{Framed, MAX_CHANNELS, ProtoId, Protocol, Records, someip::SomeIp, syslog::Syslog};
use crate::{Error, Progress, Result};
use net::{Net, NetStats, Payload};
use std::borrow::Cow;
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

/// The protocols of a capture, by [`ProtoId`]: DLT, SOME/IP, syslog.
pub const DLT: ProtoId = 0;
pub const SOMEIP: ProtoId = 1;
pub const SYSLOG: ProtoId = 2;

fn protocols() -> Vec<Arc<dyn Protocol>> {
    vec![Arc::new(Dlt::default()), Arc::new(SomeIp), Arc::new(Syslog)]
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
    let mut sink = Sink {
        net: Net::default(),
        c: Collector {
            base: bytes,
            delimited: protocols().iter().map(|p| p.delimited()).collect(),
            protos: protocols(),
            msgs: Vec::new(),
            side: Vec::new(),
            stats: CaptureStats::default(),
            found: Vec::new(),
        },
        options,
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
        return Err(Error::Format("capture too large to index (offsets over 256 TiB)".into()));
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
    let data = match unpacked {
        Some((b, _)) => b,
        None => data,
    };
    Ok((Records::framed(name, path, data, msgs, side, channel_ecus, protos, skipped), stats))
}

/// Where containers deliver frames: transport, router and protocols.
pub(crate) struct Sink<'a, 'o> {
    net: Net,
    c: Collector<'a>,
    options: &'o CaptureOptions,
    /// ECU id per channel, for messages that don't carry one.
    channel_ecus: Vec<[u8; 4]>,
    too_far: bool,
}

impl<'a> Sink<'a, '_> {
    /// A link-layer frame (pcap): transport, then the router.
    fn link_frame(&mut self, link: u32, data: &'a [u8], secs: u32, usecs: i32, channel: usize) {
        let (c, options, too_far) = (&mut self.c, self.options, &mut self.too_far);
        self.net.frame(link, data, secs, &mut |port, payload| {
            if let Some(proto) = route(options, port) {
                *too_far |= !c.datagram(&payload, proto, channel, secs, usecs);
            }
        });
    }

    /// An Ethernet frame's payload with its EtherType (MF4 `ETH_Frame`).
    fn ether(&mut self, ethertype: u16, data: Cow<'a, [u8]>, secs: u32, usecs: i32, channel: usize) {
        let (c, options, too_far) = (&mut self.c, self.options, &mut self.too_far);
        match data {
            Cow::Borrowed(d) => self.net.ether_frame(ethertype, d, secs, &mut |port, payload| {
                if let Some(proto) = route(options, port) {
                    *too_far |= !c.datagram(&payload, proto, channel, secs, usecs);
                }
            }),
            // Not in the file: whatever comes out of it is copied.
            Cow::Owned(v) => self.net.ether_frame(ethertype, &v, secs, &mut |port, payload| {
                if let Some(proto) = route(options, port) {
                    let p = Payload::Assembled(payload.bytes().to_vec());
                    *too_far |= !c.datagram(&p, proto, channel, secs, usecs);
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
        self.too_far |= !self.c.datagram(&p, DLT, channel, secs, usecs);
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

/// The protocol of a UDP payload by its destination port, if one is
/// configured for it (the first protocol that lists the port).
fn route(options: &CaptureOptions, port: u16) -> Option<ProtoId> {
    [(&options.dlt_udp_ports, DLT), (&options.someip_udp_ports, SOMEIP), (&options.syslog_udp_ports, SYSLOG)]
        .into_iter()
        .find(|(ports, _)| ports.contains(&port))
        .map(|(_, p)| p)
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
}

impl Collector<'_> {
    /// Index the messages of one datagram of protocol `proto`. Returns
    /// false if an offset doesn't fit the index.
    fn datagram(&mut self, payload: &Payload<'_>, proto: ProtoId, channel: usize, secs: u32, usecs: i32) -> bool {
        let mut found = std::mem::take(&mut self.found);
        found.clear();
        let ok = self.index(payload, proto, channel, secs, usecs, &mut found);
        self.found = found;
        ok
    }

    fn index(
        &mut self,
        payload: &Payload<'_>,
        proto: ProtoId,
        channel: usize,
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
        let channel = channel.min(MAX_CHANNELS - 1);
        if !self.delimited[proto] {
            // Each message behind its length, in the side buffer.
            let ends: Vec<usize> = found.iter().skip(1).copied().chain([bytes.len() - skipped]).collect();
            for (&at, end) in found.iter().zip(ends) {
                let Some(f) = Framed::new(self.side.len() as u64, true, proto, channel, secs, usecs) else {
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
            let Some(f) = Framed::new((base + at) as u64, in_side, proto, channel, secs, usecs) else { return false };
            self.msgs.push(f);
            self.stats.messages += 1;
        }
        true
    }
}

#[cfg(test)]
mod tests;
