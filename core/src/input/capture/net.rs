//! Transport: link-layer frames to UDP payloads.
//!
//! Ethernet (with stacked VLAN tags), Linux cooked
//! captures, raw IP and loopback; IPv4 and IPv6 (extension headers); UDP.
//! IP fragments are reassembled in any order, interleaved datagrams
//! included, with bounded memory: incomplete datagrams are dropped after
//! [`FRAGMENT_TIMEOUT_SECS`] of capture time, or when more than
//! [`MAX_PENDING`] are waiting, and counted.

use std::collections::HashMap;

/// Seconds of capture time an incomplete datagram may wait for fragments.
pub const FRAGMENT_TIMEOUT_SECS: u32 = 30;
/// Incomplete datagrams kept at most.
pub const MAX_PENDING: usize = 4096;

/// A UDP payload: still in the frame, or reassembled from fragments.
pub enum Payload<'a> {
    InFrame(&'a [u8]),
    Assembled(Vec<u8>),
}

impl Payload<'_> {
    pub fn bytes(&self) -> &[u8] {
        match self {
            Payload::InFrame(b) => b,
            Payload::Assembled(v) => v,
        }
    }
}

/// What the transport saw, for reporting.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct NetStats {
    pub frames: u64,
    pub udp: u64,
    /// Datagrams reassembled from fragments.
    pub reassembled: u64,
    /// Datagrams whose fragments never all arrived.
    pub incomplete: u64,
    /// Frames that weren't IP / UDP, or were too short.
    pub other: u64,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct FragKey {
    src: [u8; 16],
    dst: [u8; 16],
    id: u32,
    v6: bool,
}

struct Partial {
    /// (offset, bytes) of the IP payload pieces received.
    parts: Vec<(usize, Vec<u8>)>,
    /// Payload length, known once the last fragment arrived.
    total: Option<usize>,
    /// Capture time of the last fragment.
    last: u32,
}

/// The transport state (pending fragments) of one capture.
#[derive(Default)]
pub struct Net {
    pending: HashMap<FragKey, Partial>,
    pub stats: NetStats,
    now: u32,
}

impl Net {
    /// One frame of link type `link` captured at `secs`: calls `out` with
    /// the destination port and payload of every UDP datagram it completes.
    pub fn frame<'a>(&mut self, link: u32, data: &'a [u8], secs: u32, out: &mut dyn FnMut(u16, Payload<'a>)) {
        self.begin(secs);
        let ok = match link {
            1 => data.len() >= 14 && self.ether_payload(be16(data, 12), &data[14..], out),
            113 if data.len() >= 16 => self.ether_payload(be16(data, 14), &data[16..], out),
            276 if data.len() >= 20 => self.ether_payload(be16(data, 0), &data[20..], out),
            101 | 12 | 14 => self.raw_ip(data, out),
            228 => self.ipv4(data, out),
            229 => self.ipv6(data, out),
            0 | 108 if data.len() >= 4 => {
                // BSD loopback: the address family, in the capturing host's
                // byte order.
                let fam = u32::from_le_bytes(data[..4].try_into().unwrap());
                let fam = if fam > 0xFFFF { fam.swap_bytes() } else { fam };
                match fam {
                    2 => self.ipv4(&data[4..], out),
                    24 | 28 | 30 => self.ipv6(&data[4..], out),
                    _ => false,
                }
            }
            _ => false,
        };
        if !ok {
            self.stats.other += 1;
        }
    }

    /// An Ethernet frame given as its EtherType and the bytes after it (a
    /// bus logger's `ETH_Frame` record), captured at `secs`.
    pub fn ether_frame<'a>(
        &mut self,
        ethertype: u16,
        data: &'a [u8],
        secs: u32,
        out: &mut dyn FnMut(u16, Payload<'a>),
    ) {
        self.begin(secs);
        if !self.ether_payload(ethertype, data, out) {
            self.stats.other += 1;
        }
    }

    fn begin(&mut self, secs: u32) {
        self.stats.frames += 1;
        if secs > self.now {
            self.now = secs;
            self.expire();
        }
    }

    /// Drop the datagrams still incomplete (end of capture).
    pub fn finish(&mut self) {
        self.stats.incomplete += self.pending.len() as u64;
        self.pending.clear();
    }

    fn expire(&mut self) {
        let now = self.now;
        let before = self.pending.len();
        self.pending.retain(|_, p| now.saturating_sub(p.last) <= FRAGMENT_TIMEOUT_SECS);
        self.stats.incomplete += (before - self.pending.len()) as u64;
    }

    fn ether_payload<'a>(&mut self, mut ty: u16, mut d: &'a [u8], out: &mut dyn FnMut(u16, Payload<'a>)) -> bool {
        while matches!(ty, 0x8100 | 0x88A8 | 0x9100 | 0x9101 | 0x9200) && d.len() >= 4 {
            ty = be16(d, 2);
            d = &d[4..];
        }
        match ty {
            0x0800 => self.ipv4(d, out),
            0x86DD => self.ipv6(d, out),
            _ => false,
        }
    }

    fn raw_ip<'a>(&mut self, d: &'a [u8], out: &mut dyn FnMut(u16, Payload<'a>)) -> bool {
        match d.first().map(|b| b >> 4) {
            Some(4) => self.ipv4(d, out),
            Some(6) => self.ipv6(d, out),
            _ => false,
        }
    }

    fn ipv4<'a>(&mut self, d: &'a [u8], out: &mut dyn FnMut(u16, Payload<'a>)) -> bool {
        if d.len() < 20 || d[0] >> 4 != 4 {
            return false;
        }
        let ihl = (d[0] & 0x0F) as usize * 4;
        let total = (be16(d, 2) as usize).min(d.len());
        if ihl < 20 || total < ihl {
            return false;
        }
        let frag = be16(d, 6);
        let (more, offset) = (frag & 0x2000 != 0, (frag & 0x1FFF) as usize * 8);
        if d[9] != 17 {
            return true; // IP, not UDP
        }
        let body = &d[ihl..total];
        if !more && offset == 0 {
            return self.udp(body, out);
        }
        let mut key = FragKey { src: [0; 16], dst: [0; 16], id: be16(d, 4) as u32, v6: false };
        key.src[..4].copy_from_slice(&d[12..16]);
        key.dst[..4].copy_from_slice(&d[16..20]);
        self.fragment(key, offset, more, body, out)
    }

    fn ipv6<'a>(&mut self, d: &'a [u8], out: &mut dyn FnMut(u16, Payload<'a>)) -> bool {
        if d.len() < 40 || d[0] >> 4 != 6 {
            return false;
        }
        let end = (40 + be16(d, 4) as usize).min(d.len());
        let (mut next, mut at) = (d[6], 40);
        let mut frag = None;
        loop {
            match next {
                0 | 43 | 60 if at + 8 <= end => {
                    next = d[at];
                    at += (d[at + 1] as usize + 1) * 8;
                }
                44 if at + 8 <= end => {
                    let fo = be16(d, at + 2);
                    let id = u32::from_be_bytes(d[at + 4..at + 8].try_into().unwrap());
                    frag = Some(((fo >> 3) as usize * 8, fo & 1 != 0, id));
                    next = d[at];
                    at += 8;
                }
                _ => break,
            }
        }
        if next != 17 || at > end {
            return next != 17;
        }
        let body = &d[at..end];
        match frag {
            None | Some((0, false, _)) => self.udp(body, out),
            Some((offset, more, id)) => {
                let key =
                    FragKey { src: d[8..24].try_into().unwrap(), dst: d[24..40].try_into().unwrap(), id, v6: true };
                self.fragment(key, offset, more, body, out)
            }
        }
    }

    fn udp<'a>(&mut self, d: &'a [u8], out: &mut dyn FnMut(u16, Payload<'a>)) -> bool {
        if d.len() < 8 {
            return false;
        }
        let len = (be16(d, 4) as usize).clamp(8, d.len());
        self.stats.udp += 1;
        out(be16(d, 2), Payload::InFrame(&d[8..len]));
        true
    }

    /// Store a fragment of a UDP datagram; when it completes the datagram,
    /// pass the datagram on.
    fn fragment<'a>(
        &mut self,
        key: FragKey,
        offset: usize,
        more: bool,
        body: &[u8],
        out: &mut dyn FnMut(u16, Payload<'a>),
    ) -> bool {
        if self.pending.len() >= MAX_PENDING && !self.pending.contains_key(&key) {
            // Make room: drop the stalest datagram.
            if let Some(k) = self.pending.iter().min_by_key(|(_, p)| p.last).map(|(k, _)| *k) {
                self.pending.remove(&k);
                self.stats.incomplete += 1;
            }
        }
        let now = self.now;
        let p = self.pending.entry(key).or_insert_with(|| Partial { parts: Vec::new(), total: None, last: now });
        p.last = now;
        if !more {
            p.total = Some(offset + body.len());
        }
        if !p.parts.iter().any(|(o, _)| *o == offset) {
            p.parts.push((offset, body.to_vec()));
        }
        let Some(total) = p.total else { return true };
        p.parts.sort_by_key(|(o, _)| *o);
        let mut covered = 0;
        for (o, b) in &p.parts {
            if *o > covered {
                return true; // a hole: wait for more
            }
            covered = covered.max(o + b.len());
        }
        if covered < total {
            return true;
        }
        let p = self.pending.remove(&key).expect("present");
        let mut whole = vec![0u8; total];
        for (o, b) in &p.parts {
            let n = b.len().min(total.saturating_sub(*o));
            whole[*o..*o + n].copy_from_slice(&b[..n]);
        }
        self.stats.reassembled += 1;
        if whole.len() < 8 {
            return false;
        }
        let len = (be16(&whole, 4) as usize).clamp(8, whole.len());
        let port = be16(&whole, 2);
        whole.truncate(len);
        whole.drain(..8);
        self.stats.udp += 1;
        out(port, Payload::Assembled(whole));
        true
    }
}

fn be16(b: &[u8], at: usize) -> u16 {
    u16::from_be_bytes([b[at], b[at + 1]])
}
