//! Rendering a message's payload as text: verbose arguments, non-verbose
//! messages (as their FIBEX description says, else message id + hex),
//! control messages.

use super::fibex::Frame;
use super::format::{MSTP_CONTROL, Message};
use std::io::Write;

// Verbose argument type info bits.
const TYLE_MASK: u32 = 0x0F;
const BOOL: u32 = 0x10;
const SINT: u32 = 0x20;
const UINT: u32 = 0x40;
const FLOA: u32 = 0x80;
const STRG: u32 = 0x200;
const RAWD: u32 = 0x400;
const VARI: u32 = 0x800;
const FIXP: u32 = 0x1000;
const TRAI: u32 = 0x2000;
/// Types this decoder doesn't handle yet: array, struct.
const UNSUPPORTED: u32 = 0x100 | 0x4000;

/// Append the payload text of `msg` to `out`. Never fails: whatever can't
/// be decoded is shown as hex. Control characters become spaces so the
/// text stays on one line and in one column.
pub(crate) fn render(msg: &Message<'_>, frame: Option<&Frame>, out: &mut Vec<u8>) {
    render_as(msg, frame, out, false);
}

/// Like [`render`], but the payload's own line breaks (`\n`, `\r\n`, a
/// lone `\r`, all as `\n`) and tabs are kept, as the logging code wrote
/// them. Other control characters still become spaces.
pub(crate) fn render_formatted(msg: &Message<'_>, frame: Option<&Frame>, out: &mut Vec<u8>) {
    render_as(msg, frame, out, true);
}

/// `frame`: the FIBEX description of a non-verbose message.
fn render_as(msg: &Message<'_>, frame: Option<&Frame>, out: &mut Vec<u8>, keep_breaks: bool) {
    let start = out.len();
    let mut r = Rd { b: msg.payload(), be: msg.big_endian() };
    if msg.mstp() == Some(MSTP_CONTROL) {
        control(msg, &mut r, out);
    } else if msg.verbose() {
        for i in 0..msg.arg_count() {
            let (rest, len) = (r.b, out.len());
            if i > 0 {
                out.push(b' ');
            }
            if arg(&mut r, out).is_none() {
                r.b = rest;
                out.truncate(len);
                break;
            }
        }
        if !r.b.is_empty() {
            if out.len() > start {
                out.push(b' ');
            }
            out.extend_from_slice(b"<undecoded> ");
            hex(r.b, out);
        }
    } else {
        // Non-verbose: as FIBEX describes it, else the message id and hex.
        match r.u32() {
            Some(_) if frame.is_some() => frame.expect("checked").render(r.b, r.be, out),
            Some(id) => {
                let _ = write!(out, "[{id}]");
                if !r.b.is_empty() {
                    out.push(b' ');
                    hex(r.b, out);
                }
            }
            None => hex(r.b, out),
        }
    }
    if keep_breaks {
        let text = out.split_off(start);
        let mut it = text.iter().copied().peekable();
        while let Some(b) = it.next() {
            match b {
                b'\r' => {
                    it.next_if_eq(&b'\n');
                    out.push(b'\n');
                }
                b'\n' | b'\t' => out.push(b),
                b if b < 0x20 || b == 0x7F => out.push(b' '),
                b => out.push(b),
            }
        }
        return;
    }
    for b in &mut out[start..] {
        if *b < 0x20 || *b == 0x7F {
            *b = b' ';
        }
    }
}

/// One verbose argument; `None` if it's malformed or of an unsupported type.
fn arg(r: &mut Rd<'_>, out: &mut Vec<u8>) -> Option<()> {
    let ti = r.u32()?;
    if ti & UNSUPPORTED != 0 {
        return None;
    }
    let vari = ti & VARI != 0;
    let tyle = ti & TYLE_MASK;
    if ti & (STRG | TRAI | RAWD) != 0 {
        let len = r.u16()? as usize;
        let name = if vari && ti & TRAI == 0 { r.name()? } else { b"" };
        let data = r.take(len)?;
        label(name, out);
        if ti & RAWD != 0 {
            hex(data, out);
        } else {
            let s = data.strip_suffix(b"\0").unwrap_or(data);
            out.extend_from_slice(String::from_utf8_lossy(s).as_bytes());
        }
        return Some(());
    }
    if ti & BOOL != 0 {
        let name = if vari { r.name()? } else { b"" };
        label(name, out);
        out.extend_from_slice(if r.u8()? != 0 { b"true" } else { b"false" });
        return Some(());
    }
    if ti & (SINT | UINT | FLOA) == 0 || ti & FIXP != 0 {
        return None;
    }
    let (name, unit) = if vari {
        let (nl, ul) = (r.u16()? as usize, r.u16()? as usize);
        (strip_nul(r.take(nl)?), strip_nul(r.take(ul)?))
    } else {
        (&b""[..], &b""[..])
    };
    label(name, out);
    let size = match tyle {
        1..=5 => 1usize << (tyle - 1),
        _ => return None,
    };
    let v = r.take(size)?;
    let mut be = [0u8; 16];
    // Normalize to a big-endian, right-aligned 128-bit buffer.
    if r.be {
        be[16 - size..].copy_from_slice(v);
    } else {
        for (i, &b) in v.iter().enumerate() {
            be[15 - i] = b;
        }
    }
    let u = u128::from_be_bytes(be);
    if ti & FLOA != 0 {
        match size {
            4 => write!(out, "{}", f32::from_bits(u as u32)),
            8 => write!(out, "{}", f64::from_bits(u as u64)),
            _ => return None,
        }
        .ok()?;
    } else if ti & SINT != 0 {
        let shift = 128 - 8 * size as u32;
        let _ = write!(out, "{}", ((u << shift) as i128) >> shift);
    } else {
        let _ = write!(out, "{u}");
    }
    if !unit.is_empty() {
        out.push(b' ');
        out.extend_from_slice(unit);
    }
    Some(())
}

fn label(name: &[u8], out: &mut Vec<u8>) {
    if !name.is_empty() {
        out.extend_from_slice(name);
        out.push(b'=');
    }
}

fn strip_nul(s: &[u8]) -> &[u8] {
    s.strip_suffix(b"\0").unwrap_or(s)
}

fn control(msg: &Message<'_>, r: &mut Rd<'_>, out: &mut Vec<u8>) {
    let Some(sid) = r.u32() else { return hex(r.b, out) };
    let name = match sid {
        0x01 => "set_log_level",
        0x02 => "set_trace_status",
        0x03 => "get_log_info",
        0x04 => "get_default_log_level",
        0x05 => "store_config",
        0x06 => "reset_to_factory_default",
        0x0A => "set_message_filtering",
        0x11 => "set_default_log_level",
        0x12 => "set_default_trace_status",
        0x13 => "get_software_version",
        0x15 => "get_default_trace_status",
        0x17 => "get_log_channel_names",
        0x1F => "get_trace_status",
        0xF01 => "unregister_context",
        0xF02 => "connection_info",
        0xF03 => "timezone",
        0xF04 => "marker",
        _ => "",
    };
    let _ = if name.is_empty() { write!(out, "[service {sid:#x}") } else { write!(out, "[{name}") };
    // Responses (MTIN 2) carry a status byte.
    if msg.mtin() == Some(2) {
        let status = match r.u8() {
            Some(0) => " ok",
            Some(1) => " not_supported",
            Some(2) => " error",
            Some(_) => " status?",
            None => "",
        };
        out.extend_from_slice(status.as_bytes());
        out.push(b']');
        if sid == 0x13 {
            // Version string: u32 length, then text.
            let before = r.b;
            if let Some(s) = r.u32().and_then(|n| r.take(n as usize)) {
                out.push(b' ');
                out.extend_from_slice(String::from_utf8_lossy(strip_nul(s)).as_bytes());
                return;
            }
            r.b = before;
        }
    } else {
        out.push(b']');
    }
    if !r.b.is_empty() {
        out.push(b' ');
        hex(r.b, out);
    }
}

fn hex(b: &[u8], out: &mut Vec<u8>) {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    for (i, &x) in b.iter().enumerate() {
        if i > 0 {
            out.push(b' ');
        }
        out.extend_from_slice(&[DIGITS[(x >> 4) as usize], DIGITS[(x & 15) as usize]]);
    }
}

/// Payload reader honouring the message's byte order.
struct Rd<'a> {
    b: &'a [u8],
    be: bool,
}

impl<'a> Rd<'a> {
    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        if self.b.len() < n {
            return None;
        }
        let (h, t) = self.b.split_at(n);
        self.b = t;
        Some(h)
    }

    fn u8(&mut self) -> Option<u8> {
        self.take(1).map(|b| b[0])
    }

    fn u16(&mut self) -> Option<u16> {
        let b: [u8; 2] = self.take(2)?.try_into().ok()?;
        Some(if self.be { u16::from_be_bytes(b) } else { u16::from_le_bytes(b) })
    }

    fn u32(&mut self) -> Option<u32> {
        let b: [u8; 4] = self.take(4)?.try_into().ok()?;
        Some(if self.be { u32::from_be_bytes(b) } else { u32::from_le_bytes(b) })
    }

    /// Variable name: u16 length, then the name (NUL-terminated).
    fn name(&mut self) -> Option<&'a [u8]> {
        let n = self.u16()? as usize;
        self.take(n).map(strip_nul)
    }
}
