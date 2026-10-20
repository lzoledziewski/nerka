//! Turn a `.dlt` file into a pcap capture of DLT over UDP, as a logger on a
//! car's network would see it: VLAN-tagged Ethernet, IPv4, UDP port 3490,
//! several messages per datagram, and every 400th datagram large enough to
//! be split into IP fragments. For testing and benchmarking capture import.
//!
//!     cargo run --release -p nerka-core --example dlt_to_pcap -- big.dlt big.pcap

use std::io::{BufWriter, Write};

/// UDP payload per datagram, and per fragmented datagram.
const DATAGRAM: usize = 1400;
const BIG_DATAGRAM: usize = 4000;
/// IP payload per fragment (a multiple of 8).
const FRAGMENT: usize = 1480;

fn main() -> std::io::Result<()> {
    let mut args = std::env::args().skip(1);
    let (src, dst) = match (args.next(), args.next()) {
        (Some(s), Some(d)) => (s, d),
        _ => {
            eprintln!("usage: dlt_to_pcap IN.dlt OUT.pcap");
            std::process::exit(2);
        }
    };
    let data = std::fs::read(&src)?;
    let mut out = BufWriter::with_capacity(1 << 20, std::fs::File::create(&dst)?);
    out.write_all(&0xA1B2_C3D4u32.to_le_bytes())?;
    for x in [2u16, 4] {
        out.write_all(&x.to_le_bytes())?;
    }
    for x in [0u32, 0, 262_144, 1] {
        out.write_all(&x.to_le_bytes())?;
    }
    let (mut pos, mut datagrams, mut frames, mut msgs) = (0usize, 0u64, 0u64, 0u64);
    let mut payload = Vec::with_capacity(BIG_DATAGRAM + 2048);
    let mut time = (0u32, 0u32);
    let mut id = 0u16;
    while pos + 20 <= data.len() {
        if &data[pos..pos + 4] != b"DLT\x01" {
            pos += 1;
            continue;
        }
        let len = u16::from_be_bytes([data[pos + 18], data[pos + 19]]) as usize;
        if pos + 16 + len > data.len() || len < 4 {
            break;
        }
        if payload.is_empty() {
            time = (
                u32::from_le_bytes(data[pos + 4..pos + 8].try_into().unwrap()),
                u32::from_le_bytes(data[pos + 8..pos + 12].try_into().unwrap()),
            );
        }
        payload.extend_from_slice(&data[pos + 16..pos + 16 + len]);
        msgs += 1;
        pos += 16 + len;
        let limit = if datagrams % 400 == 399 { BIG_DATAGRAM } else { DATAGRAM };
        if payload.len() >= limit || pos + 20 > data.len() {
            id = id.wrapping_add(1);
            frames += datagram(&mut out, &payload, id, time)?;
            datagrams += 1;
            payload.clear();
        }
    }
    out.flush()?;
    eprintln!("{msgs} messages in {datagrams} datagrams, {frames} frames");
    Ok(())
}

/// Write one UDP datagram (fragmented if needed); returns the frames written.
fn datagram(out: &mut impl Write, payload: &[u8], id: u16, (secs, usecs): (u32, u32)) -> std::io::Result<u64> {
    let mut udp = Vec::with_capacity(8 + payload.len());
    udp.extend_from_slice(&50000u16.to_be_bytes());
    udp.extend_from_slice(&3490u16.to_be_bytes());
    udp.extend_from_slice(&((8 + payload.len()) as u16).to_be_bytes());
    udp.extend_from_slice(&[0, 0]);
    udp.extend_from_slice(payload);
    let chunks: Vec<&[u8]> = udp.chunks(FRAGMENT).collect();
    let n = chunks.len();
    for (i, c) in chunks.iter().enumerate() {
        let flags = if i + 1 < n { 0x2000 } else { 0 } | (i * FRAGMENT / 8) as u16;
        let mut f = Vec::with_capacity(18 + 20 + c.len());
        f.extend_from_slice(&[2; 12]);
        f.extend_from_slice(&[0x81, 0x00, 0, 5, 0x08, 0x00]);
        f.extend_from_slice(&[0x45, 0]);
        f.extend_from_slice(&((20 + c.len()) as u16).to_be_bytes());
        f.extend_from_slice(&id.to_be_bytes());
        f.extend_from_slice(&flags.to_be_bytes());
        f.extend_from_slice(&[64, 17, 0, 0, 10, 0, 0, 1, 10, 0, 0, 2]);
        f.extend_from_slice(c);
        for x in [secs, usecs, f.len() as u32, f.len() as u32] {
            out.write_all(&x.to_le_bytes())?;
        }
        out.write_all(&f)?;
    }
    Ok(n as u64)
}
