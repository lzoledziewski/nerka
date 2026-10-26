//! Container: MF4 (ASAM MDF 4) bus-logging files to Ethernet frames and
//! DLT frames, zero-copy over the mapped file where the bytes are
//! contiguous.
//!
//! Read: the block tree (HD → DG → CG → CN, TX names, CC conversions);
//! unsorted data groups (record ids of 1, 2, 4 or 8 bytes) and sorted ones;
//! data in DT blocks, DL lists or HL headers (records may span data blocks:
//! those are copied); variable-length data in VLSD channel groups (paired
//! with their frame group by order) or SD blocks (by offset). Compressed
//! data (DZ blocks, deflate, transposed or not) is unpacked first
//! ([`super::unpack`]): the tree is read from the file, the data from the
//! unpacked bytes.
//!
//! Channel groups are recognised by their channels' names (ASAM bus
//! logging: `ETH_Frame.EtherType`, `ETH_Frame.DataBytes`,
//! `DLT_Frame.ECU_ID`, …), else by shape: an Ethernet bus-event group with
//! one 16-bit field and a byte-array data channel. Times are the header's
//! start time plus the group's master channel, through its linear
//! conversion; without one, the raw value is the time in seconds.

use super::Sink;
use super::unpack::{Layout, Leaf, dz_header};
use crate::{Error, Progress, Result};
use std::borrow::Cow;
use std::collections::{HashMap, VecDeque};

pub fn is_mf4(head: &[u8]) -> bool {
    head.starts_with(b"MDF     4")
}

/// One block: its id (`DG`, `CG`, …), links and data, borrowed from the file.
struct Block<'a> {
    id: [u8; 2],
    links: &'a [u8],
    data: &'a [u8],
    /// Offset of the data section in the file.
    data_at: usize,
}

impl Block<'_> {
    fn link(&self, i: usize) -> u64 {
        self.links.get(i * 8..i * 8 + 8).map_or(0, |b| u64::from_le_bytes(b.try_into().unwrap()))
    }

    fn nlinks(&self) -> usize {
        self.links.len() / 8
    }
}

fn block(buf: &[u8], at: u64) -> Option<Block<'_>> {
    let at = usize::try_from(at).ok()?;
    if at == 0 || at.checked_add(24)? > buf.len() || &buf[at..at + 2] != b"##" {
        return None;
    }
    let len = usize::try_from(u64::from_le_bytes(buf[at + 8..at + 16].try_into().unwrap())).ok()?;
    let nlinks = usize::try_from(u64::from_le_bytes(buf[at + 16..at + 24].try_into().unwrap())).ok()?;
    let data_at = at.checked_add(24)?.checked_add(nlinks.checked_mul(8)?)?;
    if len < 24 || data_at > at + len || at.checked_add(len)? > buf.len() {
        return None;
    }
    Some(Block {
        id: [buf[at + 2], buf[at + 3]],
        links: &buf[at + 24..data_at],
        data: &buf[data_at..at + len],
        data_at,
    })
}

/// The text of a TX block (empty for none or another block type).
fn text(buf: &[u8], at: u64) -> &str {
    match block(buf, at) {
        Some(b) if &b.id == b"TX" => {
            let end = b.data.iter().position(|&c| c == 0).unwrap_or(b.data.len());
            std::str::from_utf8(&b.data[..end]).unwrap_or("")
        }
        _ => "",
    }
}

/// The header block's start time in ns since the epoch, UTC. With the
/// "local time" flag (`hd_time_flags` bit 0) the stored value is the
/// recorder's wall clock, its zone unknown: read in this computer's zone
/// when `local` (as other MF4 tools do), else taken as UTC.
fn start_time(hd: &[u8], local: bool) -> u64 {
    let ns = le_u64(hd, 0);
    if !local || hd.get(12).is_none_or(|f| f & 1 == 0) {
        return ns;
    }
    let secs = (ns / 1_000_000_000) as f64;
    let utc = crate::time::local_to_utc(secs);
    (ns as i128 + ((utc - secs) as i128) * 1_000_000_000).max(0) as u64
}

fn le_u64(b: &[u8], at: usize) -> u64 {
    b.get(at..at + 8).map_or(0, |x| u64::from_le_bytes(x.try_into().unwrap()))
}

fn le_u32(b: &[u8], at: usize) -> u32 {
    b.get(at..at + 4).map_or(0, |x| u32::from_le_bytes(x.try_into().unwrap()))
}

/// A channel's place in a record and how to read it.
#[derive(Clone, Copy, Debug, Default)]
struct Field {
    byte: usize,
    bit: u32,
    bits: u32,
    /// cn_data_type: 0/1 uint, 2/3 int, 4/5 float (LE / BE).
    data_type: u8,
    /// Linear conversion `a + b * x`, if any.
    linear: Option<(f64, f64)>,
}

impl Field {
    fn raw(&self, rec: &[u8]) -> Option<u64> {
        let nbytes = (self.bit + self.bits).div_ceil(8) as usize;
        let bytes = rec.get(self.byte..self.byte + nbytes.min(8))?;
        let mut v = 0u64;
        if self.data_type % 2 == 1 {
            for &b in bytes {
                v = (v << 8) | b as u64;
            }
        } else {
            for (i, &b) in bytes.iter().enumerate() {
                v |= (b as u64) << (8 * i);
            }
        }
        v >>= self.bit;
        if self.bits < 64 {
            v &= (1u64 << self.bits) - 1;
        }
        Some(v)
    }

    fn uint(&self, rec: &[u8]) -> Option<u64> {
        self.raw(rec)
    }

    /// The value as seconds (time channels).
    fn seconds(&self, rec: &[u8]) -> Option<f64> {
        let raw = self.raw(rec)?;
        let x = match (self.data_type, self.bits) {
            (4 | 5, 64) => f64::from_bits(raw),
            (4 | 5, 32) => f32::from_bits(raw as u32) as f64,
            (2 | 3, b) if b < 64 && raw >> (b - 1) & 1 == 1 => (raw as i64 - (1i64 << b)) as f64,
            (2 | 3, _) => raw as i64 as f64,
            _ => raw as f64,
        };
        // The physical value; without a conversion, the raw value itself.
        Some(match self.linear {
            Some((a, b)) => a + b * x,
            None => x,
        })
    }
}

/// Where a frame group's data bytes are.
#[derive(Clone, Copy, Debug)]
enum DataRef {
    /// In a VLSD channel group of the same data group (its record id).
    VlsdGroup(u64),
    /// In SD blocks reached from the channel: the record holds the offset.
    Signal { field: Field, stream: usize },
    /// In the record itself.
    Fixed { byte: usize, len: usize },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Ethernet,
    Dlt,
}

/// A frame group: what its records hold and where.
#[derive(Clone, Debug)]
struct FrameGroup {
    kind: Kind,
    time: Option<Field>,
    ethertype: Option<Field>,
    channel: Option<Field>,
    ecu: Option<Field>,
    data: DataRef,
}

/// A channel of a group, flattened from the CN tree.
struct Cn {
    name: String,
    cn_type: u8,
    sync: u8,
    field: Field,
    data_link: u64,
}

fn channels(buf: &[u8], first: u64) -> Vec<Cn> {
    let mut out = Vec::new();
    let mut stack = vec![first];
    let mut guard = 0;
    while let Some(mut at) = stack.pop() {
        while let Some(b) = block(buf, at) {
            guard += 1;
            if &b.id != b"CN" || guard > 100_000 {
                break;
            }
            let d = b.data;
            if d.len() >= 16 {
                let cc = block(buf, b.link(4)).filter(|c| &c.id == b"CC");
                // Linear conversion: cc_type 1, values a and b after the
                // physical range.
                let linear = cc.filter(|c| c.data.first() == Some(&1) && c.data.len() >= 40).map(|c| {
                    (
                        f64::from_le_bytes(c.data[24..32].try_into().unwrap()),
                        f64::from_le_bytes(c.data[32..40].try_into().unwrap()),
                    )
                });
                out.push(Cn {
                    name: text(buf, b.link(2)).to_ascii_lowercase(),
                    cn_type: d[0],
                    sync: d[1],
                    field: Field {
                        byte: le_u32(d, 4) as usize,
                        bit: d[3] as u32,
                        bits: le_u32(d, 8),
                        data_type: d[2],
                        linear,
                    },
                    data_link: b.link(5),
                });
            }
            if block(buf, b.link(1)).is_some_and(|c| &c.id == b"CN") {
                stack.push(b.link(1));
            }
            at = b.link(0);
        }
    }
    out
}

/// The data blocks (DT, SD, RD, DV, DZ) reached from `at`, in order.
fn data_blocks(buf: &[u8], at: u64, out: &mut Vec<u64>, depth: usize) {
    let Some(b) = block(buf, at) else { return };
    if depth > 64 {
        return;
    }
    match &b.id {
        b"DT" | b"SD" | b"RD" | b"DV" | b"DZ" => out.push(at),
        b"DL" => {
            for i in 1..b.nlinks() {
                data_blocks(buf, b.link(i), out, depth + 1);
            }
            data_blocks(buf, b.link(0), out, depth + 1);
        }
        b"HL" => data_blocks(buf, b.link(0), out, depth + 1),
        _ => {}
    }
}

/// Every data block of the file, each once, in the order the block tree
/// reaches them: each data group's records, then its channels' signal data.
// Kept out of `read`'s caller: inlined there, it made indexing an
// uncompressed MF4 4% slower (code layout; the walk itself takes < 1 ms).
#[inline(never)]
pub(crate) fn leaves(buf: &[u8]) -> Result<Vec<Leaf>> {
    let mut at = Vec::new();
    let Some(hd) = block(buf, 64).filter(|b| &b.id == b"HD") else { return Ok(Vec::new()) };
    let (mut dg_at, mut guard) = (hd.link(0), 0);
    while let Some(dg) = block(buf, dg_at).filter(|b| &b.id == b"DG") {
        guard += 1;
        if guard > 100_000 {
            break;
        }
        data_blocks(buf, dg.link(2), &mut at, 0);
        let mut cg_at = dg.link(1);
        while let Some(cg) = block(buf, cg_at).filter(|b| &b.id == b"CG") {
            for cn in channels(buf, cg.link(1)) {
                if block(buf, cn.data_link).is_some_and(|b| &b.id != b"CG") {
                    data_blocks(buf, cn.data_link, &mut at, 0);
                }
            }
            cg_at = cg.link(0);
        }
        dg_at = dg.link(0);
    }
    let mut seen = std::collections::HashSet::new();
    at.retain(|a| seen.insert(*a));
    at.into_iter()
        .map(|a| {
            let b = block(buf, a).expect("walked above");
            let dz = if &b.id == b"DZ" {
                let (zip_type, param, org, _) = dz_header(b.data)?;
                Some((zip_type, param, org))
            } else {
                None
            };
            Ok(Leaf { at: a, data_at: b.data_at, data_len: b.data.len(), dz })
        })
        .collect()
}

/// Data segments (offset, length) of the data blocks reached from `at`, in
/// order: in the file, or where `layout` put them in the unpacked bytes.
fn segments(buf: &[u8], layout: Option<&Layout>, at: u64, out: &mut Vec<(usize, usize)>) -> Result<()> {
    let mut blocks = Vec::new();
    data_blocks(buf, at, &mut blocks, 0);
    for a in blocks {
        match layout.and_then(|l| l.get(&a)) {
            Some(&seg) => out.push(seg),
            None => {
                let b = block(buf, a).expect("walked above");
                if &b.id == b"DZ" {
                    return Err(Error::Format("MF4: compressed data that wasn't unpacked".into()));
                }
                out.push((b.data_at, b.data.len()));
            }
        }
    }
    Ok(())
}

/// A byte stream over data segments: contiguous reads are borrowed from
/// the file, reads across a segment boundary are copied.
struct Stream<'a> {
    buf: &'a [u8],
    segs: Vec<(usize, usize)>,
    seg: usize,
    off: usize,
}

impl<'a> Stream<'a> {
    fn new(buf: &'a [u8], segs: Vec<(usize, usize)>) -> Self {
        Self { buf, segs, seg: 0, off: 0 }
    }

    fn done(&mut self) -> bool {
        while self.seg < self.segs.len() && self.off >= self.segs[self.seg].1 {
            self.seg += 1;
            self.off = 0;
        }
        self.seg >= self.segs.len()
    }

    /// File offset of the read position (for progress).
    fn pos(&self) -> usize {
        self.segs.get(self.seg).map_or(self.buf.len(), |s| s.0 + self.off)
    }

    fn take(&mut self, n: usize) -> Option<Cow<'a, [u8]>> {
        if self.done() {
            return (n == 0).then_some(Cow::Borrowed(&[][..]));
        }
        let (start, len) = self.segs[self.seg];
        if self.off + n <= len {
            let s = &self.buf[start + self.off..start + self.off + n];
            self.off += n;
            return Some(Cow::Borrowed(s));
        }
        let mut v = Vec::with_capacity(n);
        while v.len() < n {
            if self.done() {
                return None;
            }
            let (start, len) = self.segs[self.seg];
            let k = (n - v.len()).min(len - self.off);
            v.extend_from_slice(&self.buf[start + self.off..start + self.off + k]);
            self.off += k;
        }
        Some(Cow::Owned(v))
    }

    /// The bytes at logical offset `at` (a VLSD entry in SD data: a
    /// 4-byte length, then the bytes).
    fn vlsd_at(&mut self, at: u64) -> Option<Cow<'a, [u8]>> {
        let mut at = usize::try_from(at).ok()?;
        self.seg = 0;
        self.off = 0;
        while self.seg < self.segs.len() && at >= self.segs[self.seg].1 {
            at -= self.segs[self.seg].1;
            self.seg += 1;
        }
        self.off = at;
        let len = self.take(4)?;
        let len = u32::from_le_bytes(len.as_ref().try_into().ok()?) as usize;
        self.take(len)
    }
}

/// Recognise a frame group from its channels; `None` for other groups.
fn frame_group(
    buf: &[u8],
    cns: &[Cn],
    acq: &str,
    ethernet_bus: bool,
    by_addr: &HashMap<u64, u64>,
) -> Option<(FrameGroup, Option<u64>)> {
    let ends = |s: &str| cns.iter().find(|c| c.name.ends_with(s));
    let dlt_named = acq.to_ascii_lowercase().contains("dlt") || cns.iter().any(|c| c.name.starts_with("dlt_frame"));
    let eth_named = acq.to_ascii_lowercase().contains("eth") || cns.iter().any(|c| c.name.starts_with("eth_frame"));
    let kind = if dlt_named {
        Kind::Dlt
    } else if eth_named || ethernet_bus {
        Kind::Ethernet
    } else {
        return None;
    };
    let time = cns.iter().find(|c| (c.cn_type == 2 || c.cn_type == 3) && c.sync == 1).map(|c| c.field);
    let data_cn = ends("databytes")
        .or_else(|| cns.iter().find(|c| c.cn_type == 1))
        .or_else(|| cns.iter().filter(|c| c.field.data_type == 10 && c.cn_type == 0).max_by_key(|c| c.field.bits))?;
    let mut sd_link = None;
    let data = if data_cn.cn_type == 1 {
        match block(buf, data_cn.data_link) {
            Some(b) if &b.id == b"CG" => DataRef::VlsdGroup(*by_addr.get(&data_cn.data_link)?),
            Some(_) => {
                sd_link = Some(data_cn.data_link);
                DataRef::Signal { field: data_cn.field, stream: 0 }
            }
            None => return None,
        }
    } else {
        DataRef::Fixed { byte: data_cn.field.byte, len: data_cn.field.bits as usize / 8 }
    };
    let uint16 = |c: &&Cn| c.field.bits == 16 && c.field.data_type <= 1 && c.cn_type == 0;
    let g = FrameGroup {
        kind,
        time,
        ethertype: if kind == Kind::Ethernet {
            Some(ends("ethertype").or_else(|| cns.iter().find(uint16))?.field)
        } else {
            None
        },
        channel: ends("buschannel").map(|c| c.field),
        ecu: ends("ecu_id").or_else(|| ends("ecuid")).map(|c| c.field),
        data,
    };
    Some((g, sd_link))
}

/// Read every DLT frame group of the file `buf` into `sink`. The records
/// are read from `data`: `buf` itself, or its unpacked data blocks where
/// `layout` says. Adds `data`'s length to `progress`. Returns the bytes that
/// couldn't be read (none: MF4 has no slack between records).
pub(crate) fn read<'a>(
    buf: &[u8],
    data: &'a [u8],
    layout: Option<&Layout>,
    progress: &Progress,
    sink: &mut Sink<'a, '_>,
) -> Result<u64> {
    if buf.len() < 64 || !is_mf4(buf) {
        return Err(Error::Format("not an MDF 4 file".into()));
    }
    let hd =
        block(buf, 64).filter(|b| &b.id == b"HD").ok_or_else(|| Error::Format("MF4 without a header block".into()))?;
    let start_ns = start_time(hd.data, sink.options.mf4_local_time);
    let mut reported = 0usize;
    let mut dg_at = hd.link(0);
    let mut guard = 0;
    while let Some(dg) = block(buf, dg_at) {
        guard += 1;
        if &dg.id != b"DG" || guard > 100_000 {
            break;
        }
        let rec_id = dg.data.first().copied().unwrap_or(0) as usize;
        // The groups: record id -> (record size, VLSD, frame group).
        let mut sizes: HashMap<u64, (usize, bool)> = HashMap::new();
        let mut addr_rid: HashMap<u64, u64> = HashMap::new();
        let mut cgs = Vec::new();
        let mut cg_at = dg.link(1);
        while let Some(cg) = block(buf, cg_at) {
            if &cg.id != b"CG" {
                break;
            }
            let d = cg.data;
            let (rid, flags) = (le_u64(d, 0), d.get(16..18).map_or(0, |f| u16::from_le_bytes([f[0], f[1]])));
            let size = le_u32(d, 24) as usize + le_u32(d, 28) as usize;
            sizes.insert(rid, (size, flags & 1 != 0));
            addr_rid.insert(cg_at, rid);
            cgs.push((cg_at, rid, flags));
            cg_at = cg.link(0);
        }
        let mut groups: HashMap<u64, FrameGroup> = HashMap::new();
        let mut sd_streams: Vec<Stream<'a>> = Vec::new();
        for &(at, rid, flags) in &cgs {
            if flags & 1 != 0 {
                continue;
            }
            let cg = block(buf, at).expect("walked above");
            let acq = text(buf, cg.link(2));
            let bus =
                block(buf, cg.link(3)).filter(|s| &s.id == b"SI").map_or(0, |s| s.data.get(1).copied().unwrap_or(0));
            let ethernet_bus = flags & 2 != 0 && bus == 7;
            let cns = channels(buf, cg.link(1));
            let Some((mut g, sd)) = frame_group(buf, &cns, acq, ethernet_bus, &addr_rid) else { continue };
            if let (Some(link), DataRef::Signal { field, .. }) = (sd, g.data) {
                let mut segs = Vec::new();
                segments(buf, layout, link, &mut segs)?;
                g.data = DataRef::Signal { field, stream: sd_streams.len() };
                sd_streams.push(Stream::new(data, segs));
            }
            groups.insert(rid, g);
        }
        if groups.is_empty() {
            dg_at = dg.link(0);
            continue;
        }
        let mut segs = Vec::new();
        segments(buf, layout, dg.link(2), &mut segs)?;
        let mut s = Stream::new(data, segs);
        // Unsorted VLSD: a frame group's records and its VLSD group's come
        // in the same order; pair them by order (either may come first).
        let parent: HashMap<u64, u64> = groups
            .iter()
            .filter_map(|(rid, g)| match g.data {
                DataRef::VlsdGroup(v) => Some((v, *rid)),
                _ => None,
            })
            .collect();
        let mut waiting_frames: HashMap<u64, VecDeque<Cow<'a, [u8]>>> = HashMap::new();
        let mut waiting_data: HashMap<u64, VecDeque<Cow<'a, [u8]>>> = HashMap::new();
        let sorted_rid = (rec_id == 0).then(|| cgs.iter().find(|c| c.2 & 1 == 0).map(|c| c.1)).flatten();
        loop {
            if s.done() {
                break;
            }
            let rid = match (rec_id, sorted_rid) {
                (0, Some(r)) => r,
                (0, None) => break,
                (n, _) => {
                    let Some(id) = s.take(n) else { break };
                    id.iter().rev().fold(0u64, |v, &b| (v << 8) | b as u64)
                }
            };
            let Some(&(size, vlsd)) = sizes.get(&rid) else {
                return Err(Error::Format(format!("MF4: unknown record id {rid} in a data group")));
            };
            if vlsd {
                let Some(len) = s.take(4) else { break };
                let len = u32::from_le_bytes(len.as_ref().try_into().unwrap()) as usize;
                let Some(bytes) = s.take(len) else { break };
                if let Some(&p) = parent.get(&rid) {
                    match waiting_frames.get_mut(&p).and_then(|q| q.pop_front()) {
                        Some(rec) => deliver(&groups[&p], &rec, Some(bytes), start_ns, &mut sd_streams, sink),
                        None => waiting_data.entry(p).or_default().push_back(bytes),
                    }
                }
            } else {
                let Some(rec) = s.take(size) else { break };
                if let Some(g) = groups.get(&rid) {
                    if let DataRef::VlsdGroup(_) = g.data {
                        match waiting_data.get_mut(&rid).and_then(|q| q.pop_front()) {
                            Some(bytes) => deliver(g, &rec, Some(bytes), start_ns, &mut sd_streams, sink),
                            None => waiting_frames.entry(rid).or_default().push_back(rec),
                        }
                    } else {
                        deliver(g, &rec, None, start_ns, &mut sd_streams, sink);
                    }
                }
            }
            let pos = s.pos();
            if pos >= reported + (4 << 20) {
                if progress.is_cancelled() {
                    return Err(Error::Cancelled);
                }
                progress.add((pos - reported) as u64);
                reported = pos;
            }
        }
        dg_at = dg.link(0);
    }
    progress.add((data.len().saturating_sub(reported)) as u64);
    Ok(0)
}

/// One frame of group `g`: its record and, for VLSD groups, its data.
fn deliver<'a>(
    g: &FrameGroup,
    rec: &[u8],
    vlsd: Option<Cow<'a, [u8]>>,
    start_ns: u64,
    sd_streams: &mut [Stream<'a>],
    sink: &mut Sink<'a, '_>,
) {
    let bytes: Cow<'a, [u8]> = match (g.data, vlsd) {
        (DataRef::VlsdGroup(_), Some(b)) => b,
        (DataRef::Signal { field, stream }, _) => {
            match field.uint(rec).and_then(|off| sd_streams.get_mut(stream)?.vlsd_at(off)) {
                Some(b) => b,
                None => return,
            }
        }
        (DataRef::Fixed { byte, len }, _) => match rec.get(byte..byte + len) {
            Some(b) => Cow::Owned(b.to_vec()),
            None => return,
        },
        _ => return,
    };
    let secs_f = g.time.and_then(|t| t.seconds(rec)).unwrap_or(0.0);
    let ns = (start_ns as i128 + (secs_f * 1e9) as i128).max(0) as u128;
    let (secs, usecs) = ((ns / 1_000_000_000).min(u32::MAX as u128) as u32, ((ns % 1_000_000_000) / 1000) as i32);
    match g.kind {
        Kind::Ethernet => {
            let Some(ty) = g.ethertype.and_then(|f| f.uint(rec)) else { return };
            let channel = g.channel.and_then(|f| f.uint(rec)).unwrap_or(0) as usize;
            sink.ether(ty as u16, bytes, secs, usecs, channel);
        }
        Kind::Dlt => {
            let ecu = g.ecu.and_then(|f| rec.get(f.byte..f.byte + 4)).map_or([0; 4], |b| b.try_into().unwrap());
            let channel = sink.channel_for_ecu(ecu);
            sink.dlt(bytes, secs, usecs, channel);
        }
    }
}

#[cfg(test)]
#[path = "mf4_tests.rs"]
mod tests;
