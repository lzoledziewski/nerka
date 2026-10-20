//! Container: pcap and pcapng files to link-layer frames, zero-copy over
//! the mapped file.
//!
//! pcap: µs or ns timestamps, either byte order. pcapng: several sections
//! (each with its byte order) and interfaces (link type, `if_tsresol`,
//! `if_tsoffset`); enhanced, simple and obsolete packet blocks. Interfaces
//! are numbered across the file and become the frames' channels.

use crate::{Error, Progress, Result};

/// One captured frame.
pub struct Frame<'a> {
    pub link: u32,
    pub data: &'a [u8],
    pub secs: u32,
    pub usecs: i32,
    /// pcapng interface (numbered across sections); 0 for pcap.
    pub channel: usize,
}

pub fn is_pcap(head: &[u8]) -> bool {
    head.len() >= 4
        && matches!(
            u32::from_le_bytes(head[..4].try_into().unwrap()),
            0xA1B2_C3D4 | 0xD4C3_B2A1 | 0xA1B2_3C4D | 0x4D3C_B2A1
        )
}

pub fn is_pcapng(head: &[u8]) -> bool {
    head.starts_with(&[0x0A, 0x0D, 0x0D, 0x0A])
}

/// Bytes between progress reports and cancellation checks.
const REPORT: usize = 4 << 20;

/// Call `f` for every frame of the pcap or pcapng file in `buf`. Returns
/// the bytes that couldn't be read (a truncated last record, corrupt blocks).
pub fn frames<'a>(buf: &'a [u8], progress: &Progress, f: &mut dyn FnMut(Frame<'a>)) -> Result<u64> {
    progress.set_total(buf.len() as u64);
    if is_pcapng(buf) {
        pcapng(buf, progress, f)
    } else if is_pcap(buf) {
        pcap(buf, progress, f)
    } else {
        Err(Error::Format("not a pcap or pcapng file".into()))
    }
}

fn u16_at(b: &[u8], at: usize, be: bool) -> u16 {
    let x = [b[at], b[at + 1]];
    if be { u16::from_be_bytes(x) } else { u16::from_le_bytes(x) }
}

fn u32_at(b: &[u8], at: usize, be: bool) -> u32 {
    let x = b[at..at + 4].try_into().unwrap();
    if be { u32::from_be_bytes(x) } else { u32::from_le_bytes(x) }
}

/// Report progress up to `pos` every [`REPORT`] bytes; errors if cancelled.
fn tick(progress: &Progress, pos: usize, reported: &mut usize) -> Result<()> {
    if pos - *reported >= REPORT {
        if progress.is_cancelled() {
            return Err(Error::Cancelled);
        }
        progress.add((pos - *reported) as u64);
        *reported = pos;
    }
    Ok(())
}

fn pcap<'a>(buf: &'a [u8], progress: &Progress, f: &mut dyn FnMut(Frame<'a>)) -> Result<u64> {
    if buf.len() < 24 {
        return Err(Error::Format("pcap file shorter than its header".into()));
    }
    let magic = u32::from_le_bytes(buf[..4].try_into().unwrap());
    let be = matches!(magic, 0xD4C3_B2A1 | 0x4D3C_B2A1);
    let nanos = matches!(magic, 0xA1B2_3C4D | 0x4D3C_B2A1);
    // The link type's upper bits may carry FCS information.
    let link = u32_at(buf, 20, be) & 0x0FFF_FFFF;
    let (mut pos, mut reported) = (24, 0);
    while pos + 16 <= buf.len() {
        let (sec, frac, incl) = (u32_at(buf, pos, be), u32_at(buf, pos + 4, be), u32_at(buf, pos + 8, be) as usize);
        let start = pos + 16;
        if start + incl > buf.len() {
            break;
        }
        let usecs = if nanos { frac / 1000 } else { frac };
        f(Frame { link, data: &buf[start..start + incl], secs: sec, usecs: usecs as i32, channel: 0 });
        pos = start + incl;
        tick(progress, pos, &mut reported)?;
    }
    progress.add((buf.len() - reported) as u64);
    Ok((buf.len() - pos) as u64)
}

struct Interface {
    link: u32,
    snaplen: u32,
    /// Timestamp units per second.
    per_sec: u64,
    /// Seconds added to every timestamp (`if_tsoffset`).
    offset: i64,
    channel: usize,
}

impl Interface {
    fn time(&self, ts: u64) -> (u32, i32) {
        let secs = (ts / self.per_sec) as i64 + self.offset;
        let frac = ts % self.per_sec;
        let usecs = (frac as u128 * 1_000_000 / self.per_sec as u128) as i32;
        (secs.clamp(0, u32::MAX as i64) as u32, usecs)
    }
}

fn pcapng<'a>(buf: &'a [u8], progress: &Progress, f: &mut dyn FnMut(Frame<'a>)) -> Result<u64> {
    let (mut pos, mut reported, mut skipped) = (0usize, 0usize, 0u64);
    let mut be = false;
    let mut ifaces: Vec<Interface> = Vec::new();
    let mut channels = 0;
    while pos + 12 <= buf.len() {
        let raw_ty = u32::from_le_bytes(buf[pos..pos + 4].try_into().unwrap());
        if raw_ty == 0x0A0D_0D0A {
            // Section header: its byte-order magic sets the endianness.
            if pos + 12 > buf.len() {
                break;
            }
            be = u32::from_le_bytes(buf[pos + 8..pos + 12].try_into().unwrap()) != 0x1A2B_3C4D;
            ifaces.clear();
        }
        let len = u32_at(buf, pos + 4, be) as usize;
        if len < 12 || len % 4 != 0 || pos + len > buf.len() {
            // Corrupt or truncated: nothing after it can be trusted.
            skipped += (buf.len() - pos) as u64;
            pos = buf.len();
            break;
        }
        let body = &buf[pos + 8..pos + len - 4];
        match u32_at(buf, pos, be) {
            1 if body.len() >= 8 => {
                let (mut per_sec, mut offset) = (1_000_000u64, 0i64);
                let mut at = 8;
                while at + 4 <= body.len() {
                    let (code, olen) = (u16_at(body, at, be), u16_at(body, at + 2, be) as usize);
                    if code == 0 || at + 4 + olen > body.len() {
                        break;
                    }
                    let v = &body[at + 4..at + 4 + olen];
                    match (code, olen) {
                        (9, 1..) => {
                            let r = v[0];
                            per_sec = if r & 0x80 != 0 {
                                1u64.checked_shl((r & 0x7F) as u32).unwrap_or(1)
                            } else {
                                10u64.checked_pow(r as u32).unwrap_or(1)
                            };
                        }
                        (14, 8..) => {
                            let x = v[..8].try_into().unwrap();
                            offset = if be { i64::from_be_bytes(x) } else { i64::from_le_bytes(x) };
                        }
                        _ => {}
                    }
                    at += 4 + olen.div_ceil(4) * 4;
                }
                ifaces.push(Interface {
                    link: u16_at(body, 0, be) as u32,
                    snaplen: u32_at(body, 4, be),
                    per_sec: per_sec.max(1),
                    offset,
                    channel: channels,
                });
                channels += 1;
            }
            // Enhanced packet / obsolete packet block.
            ty @ (6 | 2) if body.len() >= 20 => {
                let id = if ty == 6 { u32_at(body, 0, be) as usize } else { u16_at(body, 0, be) as usize };
                let ts = ((u32_at(body, 4, be) as u64) << 32) | u32_at(body, 8, be) as u64;
                let cap = u32_at(body, 12, be) as usize;
                if let Some(i) = ifaces.get(id) {
                    let (secs, usecs) = i.time(ts);
                    let data = &body[20..(20 + cap).min(body.len())];
                    f(Frame { link: i.link, data, secs, usecs, channel: i.channel });
                }
            }
            // Simple packet block: interface 0, no timestamp.
            3 if body.len() >= 4 => {
                if let Some(i) = ifaces.first() {
                    let orig = u32_at(body, 0, be) as usize;
                    let cap = orig.min(i.snaplen.max(1) as usize).min(body.len() - 4);
                    f(Frame { link: i.link, data: &body[4..4 + cap], secs: 0, usecs: 0, channel: i.channel });
                }
            }
            _ => {}
        }
        pos += len;
        tick(progress, pos, &mut reported)?;
    }
    progress.add((buf.len() - reported) as u64);
    Ok(skipped + (buf.len() - pos) as u64)
}
