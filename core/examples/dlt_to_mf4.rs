//! Turn a `.dlt` file into an MF4 bus log of DLT frames, as ASAM bus
//! logging writes them: one sorted `DLT_Frame` channel group (time, ECU id,
//! the frame's bytes as signal data in SD blocks), data in 4 MiB blocks
//! under DL lists. `--deflate` stores every block deflated (DZ blocks);
//! `--one-block` puts all records and all signal data in one block each
//! (as loggers that write one big DZ block do). For testing and
//! benchmarking MF4 import.
//!
//!     cargo run --release -p nerka-core --example dlt_to_mf4 -- big.dlt big.mf4 --deflate

use std::fs::File;
use std::io::{BufWriter, Seek, SeekFrom, Write};

/// Bytes per data block (unless `--one-block`).
const BLOCK: usize = 4 << 20;
/// Bytes per record: time (ns, 8), ECU id (4), offset in the signal data (8).
const RECORD: usize = 20;

/// Blocks appended to the file; links to later blocks are patched at the end.
struct W {
    out: BufWriter<File>,
    at: u64,
}

impl W {
    fn add(&mut self, id: &[u8; 2], links: &[u64], data: &[u8]) -> std::io::Result<u64> {
        let at = self.at;
        let len = 24 + 8 * links.len() + data.len();
        self.out.write_all(b"##")?;
        self.out.write_all(id)?;
        self.out.write_all(&[0; 4])?;
        self.out.write_all(&(len as u64).to_le_bytes())?;
        self.out.write_all(&(links.len() as u64).to_le_bytes())?;
        for l in links {
            self.out.write_all(&l.to_le_bytes())?;
        }
        self.out.write_all(data)?;
        let pad = (8 - len % 8) % 8;
        self.out.write_all(&[0; 8][..pad])?;
        self.at += (len + pad) as u64;
        Ok(at)
    }

    /// A data block of type `id` (DT, SD), deflated in a DZ block if asked.
    fn data(&mut self, id: &[u8; 2], data: &[u8], deflate: bool) -> std::io::Result<u64> {
        if !deflate {
            return self.add(id, &[], data);
        }
        let packed = miniz_oxide::deflate::compress_to_vec_zlib(data, 6);
        let mut d = id.to_vec();
        d.extend_from_slice(&[0; 6]);
        d.extend_from_slice(&(data.len() as u64).to_le_bytes());
        d.extend_from_slice(&(packed.len() as u64).to_le_bytes());
        d.extend_from_slice(&packed);
        self.add(b"DZ", &[], &d)
    }

    /// A DL list of `blocks` (each `lens` long once unpacked).
    fn list(&mut self, blocks: &[u64], lens: &[u64]) -> std::io::Result<u64> {
        let mut links = vec![0];
        links.extend_from_slice(blocks);
        let mut d = vec![0u8; 4];
        d.extend_from_slice(&(blocks.len() as u32).to_le_bytes());
        let mut offset = 0u64;
        for l in lens {
            d.extend_from_slice(&offset.to_le_bytes());
            offset += l;
        }
        self.add(b"DL", &links, &d)
    }

    fn tx(&mut self, s: &str) -> std::io::Result<u64> {
        let mut d = s.as_bytes().to_vec();
        d.push(0);
        self.add(b"TX", &[], &d)
    }

    /// A channel: name, cn_type, sync type, data type, byte offset, bits;
    /// next, composition, conversion and data links.
    #[allow(clippy::too_many_arguments)]
    fn cn(
        &mut self,
        name: &str,
        ty: u8,
        sync: u8,
        dt: u8,
        byte: u32,
        bits: u32,
        links: [u64; 4],
    ) -> std::io::Result<u64> {
        let tx = self.tx(name)?;
        let mut d = vec![ty, sync, dt, 0];
        d.extend_from_slice(&byte.to_le_bytes());
        d.extend_from_slice(&bits.to_le_bytes());
        d.extend_from_slice(&[0; 4 + 4 + 4 + 48]);
        let [next, comp, cc, data] = links;
        self.add(b"CN", &[next, comp, tx, 0, cc, data, 0, 0], &d)
    }
}

/// Records or signal data on their way into blocks.
struct Blocks {
    id: [u8; 2],
    buf: Vec<u8>,
    at: Vec<u64>,
    lens: Vec<u64>,
}

impl Blocks {
    fn flush(&mut self, w: &mut W, deflate: bool) -> std::io::Result<()> {
        if !self.buf.is_empty() {
            self.at.push(w.data(&self.id, &self.buf, deflate)?);
            self.lens.push(self.buf.len() as u64);
            self.buf.clear();
        }
        Ok(())
    }
}

fn main() -> std::io::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let deflate = args.iter().any(|a| a == "--deflate");
    let one = args.iter().any(|a| a == "--one-block");
    let files: Vec<&String> = args.iter().filter(|a| !a.starts_with("--")).collect();
    let [src, dst] = files[..] else {
        eprintln!("usage: dlt_to_mf4 IN.dlt OUT.mf4 [--deflate] [--one-block]");
        std::process::exit(2);
    };
    let data = std::fs::read(src)?;
    let block = if one { usize::MAX } else { BLOCK };
    let mut w = W { out: BufWriter::with_capacity(1 << 20, File::create(dst)?), at: 0 };
    let mut id = b"MDF     4.10    nerka   ".to_vec();
    id.extend_from_slice(&[0; 4]);
    id.extend_from_slice(&410u16.to_le_bytes());
    id.resize(64, 0);
    w.out.write_all(&id)?;
    w.at = 64;
    let hd = w.add(b"HD", &[0; 6], &[0; 32])?;
    let (mut recs, mut sd) = (
        Blocks { id: *b"DT", buf: Vec::new(), at: Vec::new(), lens: Vec::new() },
        Blocks { id: *b"SD", buf: Vec::new(), at: Vec::new(), lens: Vec::new() },
    );
    let (mut pos, mut msgs, mut sd_len, mut start_ns) = (0usize, 0u64, 0u64, None);
    while pos + 20 <= data.len() {
        if &data[pos..pos + 4] != b"DLT\x01" {
            pos += 1;
            continue;
        }
        let secs = u32::from_le_bytes(data[pos + 4..pos + 8].try_into().unwrap()) as u64;
        let usecs = i32::from_le_bytes(data[pos + 8..pos + 12].try_into().unwrap()).max(0) as u64;
        let ecu = &data[pos + 12..pos + 16];
        let len = u16::from_be_bytes([data[pos + 18], data[pos + 19]]) as usize;
        let msg = &data[pos + 16..(pos + 16 + len).min(data.len())];
        pos += 16 + len.max(4);
        let ns = secs * 1_000_000_000 + usecs * 1000;
        let start = *start_ns.get_or_insert(ns);
        recs.buf.extend_from_slice(&(ns - start).to_le_bytes());
        recs.buf.extend_from_slice(ecu);
        recs.buf.extend_from_slice(&sd_len.to_le_bytes());
        sd.buf.extend_from_slice(&(msg.len() as u32).to_le_bytes());
        sd.buf.extend_from_slice(msg);
        sd_len += 4 + msg.len() as u64;
        msgs += 1;
        if recs.buf.len() >= block.min(BLOCK / RECORD * RECORD) && !one {
            recs.flush(&mut w, deflate)?;
        }
        if sd.buf.len() >= block {
            sd.flush(&mut w, deflate)?;
        }
    }
    recs.flush(&mut w, deflate)?;
    sd.flush(&mut w, deflate)?;
    let rec_list = w.list(&recs.at, &recs.lens)?;
    let sd_list = w.list(&sd.at, &sd.lens)?;
    // The channels: t → DLT_Frame (→ ECU_ID → DataBytes).
    let mut cc = vec![1u8, 0, 0, 0, 0, 0, 2, 0];
    cc.extend_from_slice(&[0; 16]);
    cc.extend_from_slice(&0f64.to_le_bytes());
    cc.extend_from_slice(&1e-9f64.to_le_bytes());
    let cc = w.add(b"CC", &[0, 0, 0, 0], &cc)?;
    let bytes = w.cn("DLT_Frame.DataBytes", 1, 0, 10, 12, 64, [0, 0, 0, sd_list])?;
    let ecu = w.cn("DLT_Frame.ECU_ID", 0, 0, 10, 8, 32, [bytes, 0, 0, 0])?;
    let frame = w.cn("DLT_Frame", 0, 0, 10, 8, 96, [0, ecu, 0, 0])?;
    let t = w.cn("t", 2, 1, 2, 0, 64, [frame, 0, cc, 0])?;
    let acq = w.tx("DLT_Frame")?;
    let mut cg = 0u64.to_le_bytes().to_vec();
    cg.extend_from_slice(&msgs.to_le_bytes());
    cg.extend_from_slice(&0x0002u16.to_le_bytes());
    cg.extend_from_slice(&[0; 6]);
    cg.extend_from_slice(&(RECORD as u32).to_le_bytes());
    cg.extend_from_slice(&0u32.to_le_bytes());
    let cg = w.add(b"CG", &[0, t, acq, 0, 0, 0], &cg)?;
    let dg = w.add(b"DG", &[0, cg, rec_list, 0], &[0; 8])?;
    // The header: its first data group and the start time.
    let mut f = w.out.into_inner().map_err(|e| e.into_error())?;
    f.seek(SeekFrom::Start(hd + 24))?;
    f.write_all(&dg.to_le_bytes())?;
    f.seek(SeekFrom::Start(hd + 24 + 48))?;
    f.write_all(&start_ns.unwrap_or(0).to_le_bytes())?;
    let packed = if deflate { ", deflated" } else { "" };
    println!("wrote {msgs} messages ({} MiB) to {dst}{packed}", f.metadata()?.len() >> 20);
    Ok(())
}
