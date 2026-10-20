//! DLT inside captures: pcap / pcapng network captures and MF4 bus logs.
//!
//! The layers of `doc/capture-import.md`: a **container** turns the file
//! into frames ([`pcap`], [`mf4`]), the **transport** turns frames into UDP
//! payloads ([`net`]: VLAN, IPv4 / IPv6 with reassembly), a **router**
//! picks the payloads that carry DLT (UDP destination ports,
//! [`CaptureOptions`]), and the DLT **decoder** indexes every message where
//! it is: in the mapped file, or, for bytes that aren't contiguous there
//! (reassembled datagrams, MF4 records spanning data blocks), in a side
//! buffer ([`DltFile`] with [`Framed`] entries). Nothing is copied to a
//! temporary `.dlt`.
//!
//! A message gets the capture time of the frame that completed its
//! datagram, and keeps the capture's order. Its ECU is its own (WEID), else
//! the one the container knows for its channel (MF4 `DLT_Frame` ECU ids).

pub mod mf4;
pub mod net;
pub mod pcap;

use super::Bytes;
use super::dlt::{DltFile, Framed, MAX_CHANNELS};
use crate::{Error, Progress, Result};
use net::{Net, NetStats, Payload};
use std::borrow::Cow;
use std::path::Path;

/// UDP destination ports that carry DLT by default: 3490, the port the
/// DLT protocol specification (AUTOSAR PRS) assigns. Others are configured
/// ([`CaptureOptions::dlt_udp_ports`]).
pub const DEFAULT_DLT_PORTS: [u16; 1] = [3490];

/// How DLT is found in a capture.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaptureOptions {
    /// UDP destination ports whose payloads are DLT messages.
    pub dlt_udp_ports: Vec<u16>,
}

impl Default for CaptureOptions {
    fn default() -> Self {
        Self { dlt_udp_ports: DEFAULT_DLT_PORTS.to_vec() }
    }
}

/// What importing a capture found.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct CaptureStats {
    pub net: NetStats,
    /// Datagrams on a DLT port, or MF4 DLT frames.
    pub dlt_datagrams: u64,
    pub dlt_messages: u64,
    /// Bytes of DLT datagrams that weren't whole messages.
    pub dlt_skipped: u64,
    /// Bytes of the file that couldn't be read as records.
    pub file_skipped: u64,
}

/// Does `head` (the first bytes of a file) look like a capture this module
/// reads?
pub fn is_capture(head: &[u8]) -> bool {
    pcap::is_pcap(head) || pcap::is_pcapng(head) || mf4::is_mf4(head)
}

/// Open a capture and index the DLT messages in it.
pub fn open(path: impl AsRef<Path>, options: &CaptureOptions, progress: &Progress) -> Result<(DltFile, CaptureStats)> {
    let path = path.as_ref();
    let data = Bytes::map(path)?;
    read(path.display().to_string(), Some(path.to_path_buf()), data, options, progress)
}

/// In-memory capture, handy for tests.
pub fn from_bytes(
    name: impl Into<String>,
    bytes: Vec<u8>,
    options: &CaptureOptions,
) -> Result<(DltFile, CaptureStats)> {
    read(name.into(), None, Bytes::Owned(bytes), options, &Progress::new())
}

fn read(
    name: String,
    path: Option<std::path::PathBuf>,
    data: Bytes,
    options: &CaptureOptions,
    progress: &Progress,
) -> Result<(DltFile, CaptureStats)> {
    let buf = data.bytes();
    let mut sink = Sink {
        net: Net::default(),
        c: Collector { base: buf, msgs: Vec::new(), side: Vec::new(), stats: CaptureStats::default() },
        options,
        channel_ecus: Vec::new(),
        too_far: false,
    };
    let file_skipped = if mf4::is_mf4(buf) {
        mf4::read(buf, progress, &mut sink)?
    } else {
        pcap::frames(buf, progress, &mut |fr| sink.link_frame(fr.link, fr.data, fr.secs, fr.usecs, fr.channel))?
    };
    if sink.too_far {
        return Err(Error::Format("capture too large to index (offsets over 256 TiB)".into()));
    }
    sink.net.finish();
    let Sink { net, c, channel_ecus, .. } = sink;
    let mut stats = c.stats;
    stats.net = net.stats;
    stats.file_skipped = file_skipped;
    let (mut msgs, mut side) = (c.msgs, c.side);
    msgs.shrink_to_fit();
    side.shrink_to_fit();
    let skipped = stats.dlt_skipped;
    Ok((DltFile::from_frames(name, path, data, msgs, side, channel_ecus, skipped), stats))
}

/// Where containers deliver frames: transport, router and DLT decoder.
pub(crate) struct Sink<'a, 'o> {
    net: Net,
    c: Collector<'a>,
    options: &'o CaptureOptions,
    /// ECU id per channel, for messages that don't carry one.
    channel_ecus: Vec<[u8; 4]>,
    too_far: bool,
}

impl<'a> Sink<'a, '_> {
    /// A link-layer frame (pcap): transport, then DLT ports.
    fn link_frame(&mut self, link: u32, data: &'a [u8], secs: u32, usecs: i32, channel: usize) {
        let (c, ports, too_far) = (&mut self.c, &self.options.dlt_udp_ports, &mut self.too_far);
        self.net.frame(link, data, secs, &mut |port, payload| {
            if ports.contains(&port) {
                *too_far |= !c.datagram(&payload, channel, secs, usecs);
            }
        });
    }

    /// An Ethernet frame's payload with its EtherType (MF4 `ETH_Frame`).
    fn ether(&mut self, ethertype: u16, data: Cow<'a, [u8]>, secs: u32, usecs: i32, channel: usize) {
        let (c, ports, too_far) = (&mut self.c, &self.options.dlt_udp_ports, &mut self.too_far);
        match data {
            Cow::Borrowed(d) => self.net.ether_frame(ethertype, d, secs, &mut |port, payload| {
                if ports.contains(&port) {
                    *too_far |= !c.datagram(&payload, channel, secs, usecs);
                }
            }),
            // Not in the file: whatever comes out of it is copied.
            Cow::Owned(v) => self.net.ether_frame(ethertype, &v, secs, &mut |port, payload| {
                if ports.contains(&port) {
                    let p = Payload::Assembled(payload.bytes().to_vec());
                    *too_far |= !c.datagram(&p, channel, secs, usecs);
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
        self.too_far |= !self.c.datagram(&p, channel, secs, usecs);
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

/// The DLT decoder's side: indexes the messages of each DLT datagram.
struct Collector<'a> {
    /// The mapped file, to turn payload slices into file offsets.
    base: &'a [u8],
    msgs: Vec<Framed>,
    side: Vec<u8>,
    stats: CaptureStats,
}

impl Collector<'_> {
    /// Index the DLT messages of one datagram (several back to back; a
    /// storage header in front of one is skipped: the capture's time
    /// wins). Returns false if an offset doesn't fit the index.
    fn datagram(&mut self, payload: &Payload<'_>, channel: usize, secs: u32, usecs: i32) -> bool {
        self.stats.dlt_datagrams += 1;
        let bytes = payload.bytes();
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
        let channel = channel.min(MAX_CHANNELS - 1);
        let mut pos = 0;
        while pos < bytes.len() {
            if bytes[pos..].starts_with(b"DLT\x01") && bytes.len() - pos > 16 {
                pos += 16;
            }
            let Some(len) = super::dlt::message_len(&bytes[pos..]) else { break };
            let Some(f) = Framed::new((base + pos) as u64, in_side, channel, secs, usecs) else { return false };
            self.msgs.push(f);
            self.stats.dlt_messages += 1;
            pos += len;
        }
        self.stats.dlt_skipped += (bytes.len() - pos) as u64;
        true
    }
}

#[cfg(test)]
mod tests;
