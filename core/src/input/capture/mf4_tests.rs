use super::super::tests::{dlt, fragments, ipv4, lines, pairs, udp};
use super::super::{CaptureOptions, from_bytes};
use crate::LogSource;

/// A tiny MDF 4 writer: blocks appended, links patched afterwards.
struct W(Vec<u8>);

impl W {
    fn new() -> W {
        let mut b = b"MDF     4.10    nerka   ".to_vec();
        b.extend_from_slice(&[0; 4]);
        b.extend_from_slice(&410u16.to_le_bytes());
        b.resize(64, 0);
        W(b)
    }

    fn add(&mut self, id: &[u8; 2], links: &[u64], data: &[u8]) -> u64 {
        let at = self.0.len() as u64;
        let len = 24 + 8 * links.len() + data.len();
        self.0.extend_from_slice(b"##");
        self.0.extend_from_slice(id);
        self.0.extend_from_slice(&[0; 4]);
        self.0.extend_from_slice(&(len as u64).to_le_bytes());
        self.0.extend_from_slice(&(links.len() as u64).to_le_bytes());
        for l in links {
            self.0.extend_from_slice(&l.to_le_bytes());
        }
        self.0.extend_from_slice(data);
        while !self.0.len().is_multiple_of(8) {
            self.0.push(0);
        }
        at
    }

    fn link(&mut self, block: u64, i: usize, to: u64) {
        let at = block as usize + 24 + 8 * i;
        self.0[at..at + 8].copy_from_slice(&to.to_le_bytes());
    }

    fn tx(&mut self, s: &str) -> u64 {
        if s.is_empty() {
            return 0;
        }
        let mut d = s.as_bytes().to_vec();
        d.push(0);
        self.add(b"TX", &[], &d)
    }

    /// A channel: (name, cn_type, sync, data_type, byte, bits), next and
    /// composition links, conversion and data links.
    #[allow(clippy::too_many_arguments)]
    fn cn(
        &mut self,
        name: &str,
        ty: u8,
        sync: u8,
        dt: u8,
        byte: u32,
        bits: u32,
        next: u64,
        comp: u64,
        cc: u64,
        data: u64,
    ) -> u64 {
        let tx = self.tx(name);
        let mut d = vec![ty, sync, dt, 0];
        d.extend_from_slice(&byte.to_le_bytes());
        d.extend_from_slice(&bits.to_le_bytes());
        d.extend_from_slice(&[0; 4 + 4 + 4 + 48]);
        self.add(b"CN", &[next, comp, tx, 0, cc, data, 0, 0], &d)
    }

    fn linear(&mut self, a: f64, b: f64) -> u64 {
        let mut d = vec![1, 0, 0, 0, 0, 0, 2, 0];
        d.extend_from_slice(&[0; 16]);
        d.extend_from_slice(&a.to_le_bytes());
        d.extend_from_slice(&b.to_le_bytes());
        self.add(b"CC", &[0, 0, 0, 0], &d)
    }

    #[allow(clippy::too_many_arguments)]
    fn cg(&mut self, next: u64, cn: u64, acq: &str, si: u64, rid: u64, flags: u16, bytes: u32) -> u64 {
        let acq = self.tx(acq);
        let mut d = rid.to_le_bytes().to_vec();
        d.extend_from_slice(&0u64.to_le_bytes());
        d.extend_from_slice(&flags.to_le_bytes());
        d.extend_from_slice(&[0; 6]);
        d.extend_from_slice(&bytes.to_le_bytes());
        d.extend_from_slice(&0u32.to_le_bytes());
        self.add(b"CG", &[next, cn, acq, si, 0, 0], &d)
    }

    /// A DZ block holding `data` (that of an `org` block: DT, SD, …):
    /// deflated, or transposed in `cols` columns first.
    fn dz(&mut self, org: &[u8; 2], cols: Option<u32>, data: &[u8]) -> u64 {
        let packed = match cols {
            Some(c) => miniz_oxide::deflate::compress_to_vec_zlib(&transpose(data, c as usize), 6),
            None => miniz_oxide::deflate::compress_to_vec_zlib(data, 6),
        };
        let mut d = org.to_vec();
        d.extend_from_slice(&[u8::from(cols.is_some()), 0]);
        d.extend_from_slice(&cols.unwrap_or(0).to_le_bytes());
        d.extend_from_slice(&(data.len() as u64).to_le_bytes());
        d.extend_from_slice(&(packed.len() as u64).to_le_bytes());
        d.extend_from_slice(&packed);
        self.add(b"DZ", &[], &d)
    }

    fn header(&mut self, start_ns: u64) -> u64 {
        let mut d = start_ns.to_le_bytes().to_vec();
        d.extend_from_slice(&[0; 24]);
        self.add(b"HD", &[0; 6], &d)
    }
}

/// What a DZ transposition does: `cols` bytes per record, stored column by
/// column; the bytes after the last whole record stay as they are.
fn transpose(v: &[u8], cols: usize) -> Vec<u8> {
    let rows = v.len() / cols;
    let mut out = Vec::with_capacity(v.len());
    for c in 0..cols {
        out.extend((0..rows).map(|r| v[r * cols + c]));
    }
    out.extend_from_slice(&v[rows * cols..]);
    out
}

const START_NS: u64 = 1_700_000_000_000_000_000;

/// How [`x2e_packed`] stores its two data blocks.
#[derive(Clone, Copy, Debug)]
enum Pack {
    Plain,
    /// Both deflated (transposed in that many columns, if any).
    Both(Option<u32>),
    /// Only the second, deflated.
    Second,
}

/// An X2E-style file: unsorted data (2-byte record ids), one Ethernet
/// frame group (43-byte records) with its frame bytes in a VLSD group.
/// `named`: ASAM channel names; else anonymous (found by shape). `frames`:
/// (time in ns from the start, bus channel, EtherType, bytes after the
/// EtherType); `vlsd_first` puts each frame's data before its record. The
/// records go into two DT blocks split at `split` bytes.
fn x2e(named: bool, frames: &[(u64, u8, u16, Vec<u8>)], vlsd_first: bool, split: usize) -> Vec<u8> {
    x2e_packed(named, frames, vlsd_first, split, Pack::Plain)
}

fn x2e_packed(named: bool, frames: &[(u64, u8, u16, Vec<u8>)], vlsd_first: bool, split: usize, pack: Pack) -> Vec<u8> {
    let mut w = W::new();
    let hd = w.header(START_NS);
    let n = |s: &'static str| if named { s } else { "" };
    let si = {
        let d = [2u8, 7, 0, 0, 0, 0, 0, 0];
        w.add(b"SI", &[0, 0, 0], &d)
    };
    let vcg = w.cg(0, 0, "", 0, 2, 0x0001, 0);
    let data = w.cn(n("ETH_Frame.DataBytes"), 1, 0, 10, 35, 64, 0, 0, 0, vcg);
    let len = w.cn(n("ETH_Frame.DataLength"), 0, 0, 0, 31, 32, data, 0, 0, 0);
    let ty = w.cn(n("ETH_Frame.EtherType"), 0, 0, 0, 21, 16, len, 0, 0, 0);
    let dst = w.cn(n("ETH_Frame.Destination"), 0, 0, 0, 15, 48, ty, 0, 0, 0);
    let src = w.cn(n("ETH_Frame.Source"), 0, 0, 0, 9, 48, dst, 0, 0, 0);
    let frame = w.cn(n("ETH_Frame"), 0, 0, 10, 9, 272, 0, src, 0, 0);
    let chan = w.cn(n("ETH_Frame.BusChannel"), 0, 0, 0, 8, 8, frame, 0, 0, 0);
    let cc = w.linear(0.0, 1e-9);
    let t = w.cn(n("t"), 2, 1, 2, 0, 64, chan, 0, cc, 0);
    let cg = w.cg(vcg, t, if named { "ETH_Frame" } else { "" }, si, 1, 0x0006, 43);
    let mut records = Vec::new();
    for (ns, ch, ety, bytes) in frames {
        let mut fixed = 1u16.to_le_bytes().to_vec();
        let mut r = vec![0u8; 43];
        r[..8].copy_from_slice(&ns.to_le_bytes());
        r[8] = *ch;
        r[21..23].copy_from_slice(&ety.to_le_bytes());
        r[31..35].copy_from_slice(&(bytes.len() as u32).to_le_bytes());
        fixed.extend_from_slice(&r);
        let mut v = 2u16.to_le_bytes().to_vec();
        v.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
        v.extend_from_slice(bytes);
        if vlsd_first {
            records.extend(v);
            records.extend(fixed);
        } else {
            records.extend(fixed);
            records.extend(v);
        }
    }
    let split = split.min(records.len());
    let (a, b) = records.split_at(split);
    let (dt1, dt2) = match pack {
        Pack::Plain => (w.add(b"DT", &[], a), w.add(b"DT", &[], b)),
        Pack::Both(cols) => (w.dz(b"DT", cols, a), w.dz(b"DT", cols, b)),
        Pack::Second => (w.add(b"DT", &[], a), w.dz(b"DT", None, b)),
    };
    let mut dl_data = vec![0u8; 4];
    dl_data.extend_from_slice(&2u32.to_le_bytes());
    let dl = w.add(b"DL", &[0, dt1, dt2], &dl_data);
    let dg = w.add(b"DG", &[0, cg, dl, 0], &[2, 0, 0, 0, 0, 0, 0, 0]);
    w.link(hd, 0, dg);
    w.0
}

fn ip_dlt(id: u16, apid: &str, text: &str) -> Vec<u8> {
    ipv4(17, id, 0, &udp(3490, &dlt(apid, text)))
}

#[test]
fn ethernet_frames_in_vlsd_groups() {
    let big = dlt("BIG", &"b".repeat(600));
    let frags = fragments(9, &udp(3490, &big), 400);
    let mut tagged = vec![0, 5, 0x08, 0x00];
    tagged.extend_from_slice(&ip_dlt(3, "VLAN", "tagged"));
    let frames = vec![
        (1_000_000, 1, 0x0800, ip_dlt(1, "NAV", "first")),
        (2_500_000, 2, 0x8100, tagged),
        (3_000_000, 1, 0x0800, frags[1][..].to_vec()),
        (4_000_000, 1, 0x0800, frags[0][..].to_vec()),
        (5_000_000, 1, 0x0806, vec![0; 28]),
        (6_000_000, 1, 0x0800, ipv4(17, 4, 0, &udp(30490, &dlt("XXX", "other port")))),
    ];
    for named in [true, false] {
        for vlsd_first in [true, false] {
            for split in [0, 61, 1000, usize::MAX] {
                let file = x2e(named, &frames, vlsd_first, split);
                let what = format!("named {named}, vlsd first {vlsd_first}, split {split}");
                let (f, s) =
                    from_bytes("t.mf4", file, &CaptureOptions::default()).unwrap_or_else(|e| panic!("{what}: {e}"));
                let got = lines(&f);
                assert_eq!(got.iter().map(|(a, _)| a.as_str()).collect::<Vec<_>>(), ["NAV", "VLAN", "BIG"], "{what}");
                assert_eq!(got[2].1, "b".repeat(600), "{what}");
                assert_eq!((s.net.reassembled, s.messages), (1, 3), "{what}");
                // Start time plus the master channel through its conversion.
                assert!(
                    f.line_text(1).unwrap().starts_with("2023-11-14 22:13:20.002500\t"),
                    "{what}: {:?}",
                    f.line_text(1)
                );
            }
        }
    }
    // Messages in the file stay there: no side buffer without a split
    // record or fragments.
    let file = x2e(true, &frames[..2], false, usize::MAX);
    let (f, _) = from_bytes("t.mf4", file, &CaptureOptions::default()).unwrap();
    assert_eq!((f.line_count(), f.side_bytes()), (2, 0));
}

/// A sorted data group of DLT frames whose bytes are in an SD block (the
/// record holds the offset), with the ECU id in the record.
#[test]
fn dlt_frames_with_signal_data() {
    for packed in [false, true] {
        dlt_frames_with_signal_data_in(packed);
    }
}

/// `packed`: the SD block deflated in a DZ block.
fn dlt_frames_with_signal_data_in(packed: bool) {
    let mut w = W::new();
    let hd = w.header(START_NS);
    let msgs: Vec<(&str, Vec<u8>)> =
        vec![("ECUA", dlt("ONE", "first")), ("ECUB", [dlt("TWO", "second"), dlt("THR", "third")].concat())];
    // DLT messages without an ECU id in their header take the frame's.
    let mut no_weid = dlt("NOE", "no ecu id");
    no_weid[0] &= !0x04;
    let rest = no_weid.split_off(8);
    no_weid.truncate(4);
    no_weid.extend_from_slice(&rest);
    let len = (no_weid.len() as u16).to_be_bytes();
    no_weid[2..4].copy_from_slice(&len);
    let msgs = [msgs, vec![("ECUC", no_weid)]].concat();
    let mut sd = Vec::new();
    let mut records = Vec::new();
    for (i, (ecu, bytes)) in msgs.iter().enumerate() {
        let mut r = vec![0u8; 29];
        r[..8].copy_from_slice(&(i as u64 + 1).to_le_bytes());
        r[13..17].copy_from_slice(ecu.as_bytes());
        r[17..21].copy_from_slice(&(bytes.len() as u32).to_le_bytes());
        r[21..29].copy_from_slice(&(sd.len() as u64).to_le_bytes());
        sd.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
        sd.extend_from_slice(bytes);
        records.extend(r);
    }
    let sd = if packed { w.dz(b"SD", None, &sd) } else { w.add(b"SD", &[], &sd) };
    let data = w.cn("DLT_Frame.DataBytes", 1, 0, 10, 21, 64, 0, 0, 0, sd);
    let ecu = w.cn("DLT_Frame.ECU_ID", 0, 0, 10, 13, 32, data, 0, 0, 0);
    let frame = w.cn("DLT_Frame", 0, 0, 10, 9, 160, 0, ecu, 0, 0);
    let t = w.cn("t", 2, 1, 2, 0, 64, frame, 0, 0, 0);
    let cg = w.cg(0, t, "DLT_Frame", 0, 0, 0x0002, 29);
    let dt = w.add(b"DT", &[], &records);
    let dg = w.add(b"DG", &[0, cg, dt, 0], &[0; 8]);
    w.link(hd, 0, dg);
    let (f, s) = from_bytes("t.mf4", w.0, &CaptureOptions::default()).unwrap();
    assert_eq!(lines(&f), pairs(&[("ONE", "first"), ("TWO", "second"), ("THR", "third"), ("NOE", "no ecu id")]));
    assert_eq!(s.messages, 4);
    // Own ECU id (WEID) first; without one, the frame's.
    let ecus: Vec<String> = (0..4).map(|l| f.line_text(l).unwrap().split('\t').nth(2).unwrap().to_owned()).collect();
    assert_eq!(ecus, ["ECU1", "ECU1", "ECU1", "ECUC"]);
    // A time channel without a conversion: the raw value in seconds.
    assert!(f.line_text(1).unwrap().starts_with("2023-11-14 22:13:22.000000\t"));
}

#[test]
fn compressed_and_other_files() {
    let mut w = W::new();
    let hd = w.header(START_NS);
    let data = w.cn("ETH_Frame.DataBytes", 0, 0, 10, 9, 64, 0, 0, 0, 0);
    let ty = w.cn("ETH_Frame.EtherType", 0, 0, 0, 21, 16, data, 0, 0, 0);
    let cg = w.cg(0, ty, "ETH_Frame", 0, 1, 0x0002, 30);
    let dz = w.add(b"DZ", &[], &[b'D', b'T', 0, 0, 0, 0, 0, 0]);
    let dg = w.add(b"DG", &[0, cg, dz, 0], &[1, 0, 0, 0, 0, 0, 0, 0]);
    w.link(hd, 0, dg);
    let e = from_bytes("t.mf4", w.0, &CaptureOptions::default()).err().expect("a cut DZ block is refused");
    assert!(e.to_string().contains("DZ"), "{e}");
    // An MF4 without DLT has no lines; `open` picks the MF4 reader by content.
    let empty = {
        let mut w = W::new();
        w.header(START_NS);
        w.0
    };
    let dir = std::env::temp_dir().join(format!("nerka-mf4-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let (a, b) = (dir.join("a.mf4"), dir.join("b.dat"));
    std::fs::write(&a, &empty).unwrap();
    std::fs::write(&b, x2e(true, &[(0, 0, 0x0800, ip_dlt(1, "NAV", "x"))], false, usize::MAX)).unwrap();
    let p = crate::Progress::new();
    assert_eq!(crate::open(&a, &p).unwrap().line_count(), 0);
    assert_eq!(crate::open(&b, &p).unwrap().line_count(), 1);
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn compressed_data_reads_like_uncompressed() {
    let big = dlt("BIG", &"b".repeat(600));
    let frags = fragments(9, &udp(3490, &big), 400);
    let frames = vec![
        (1_000_000, 1, 0x0800, ip_dlt(1, "NAV", "first")),
        (3_000_000, 1, 0x0800, frags[1][..].to_vec()),
        (4_000_000, 1, 0x0800, frags[0][..].to_vec()),
        (6_000_000, 1, 0x0800, ip_dlt(2, "LAST", "last")),
    ];
    let o = CaptureOptions::default();
    for split in [0, 61, 1000, usize::MAX] {
        let (plain, ps) = from_bytes("t.mf4", x2e(true, &frames, false, split), &o).unwrap();
        for pack in [Pack::Both(None), Pack::Both(Some(7)), Pack::Both(Some(45)), Pack::Second] {
            let what = format!("split {split}, {pack:?}");
            let file = x2e_packed(true, &frames, false, split, pack);
            let (f, s) = from_bytes("t.mf4", file, &o).unwrap_or_else(|e| panic!("{what}: {e}"));
            assert_eq!(lines(&f), lines(&plain), "{what}");
            assert_eq!(s, ps, "{what}");
            let all = |f: &crate::DltFile| {
                (0..f.line_count()).map(|l| f.line_text(l).unwrap().into_owned()).collect::<Vec<_>>()
            };
            assert_eq!(all(&f), all(&plain), "{what}: times and columns too");
        }
    }
}

#[test]
fn damaged_compressed_data_is_an_error() {
    let frames = vec![(1_000_000, 1, 0x0800, ip_dlt(1, "NAV", "first"))];
    let file = x2e_packed(true, &frames, false, 0, Pack::Both(None));
    // The second half of the second block's packed bytes, zeroed.
    let at = file.windows(4).rposition(|w| w == b"##DZ").unwrap();
    let packed = u64::from_le_bytes(file[at + 40..at + 48].try_into().unwrap()) as usize;
    let mut cut = file.clone();
    cut[at + 48 + packed / 2..at + 48 + packed].fill(0);
    let e = from_bytes("t.mf4", cut, &CaptureOptions::default()).err().expect("damaged");
    assert!(e.to_string().contains("DZ"), "{e}");
    // A zip type nerka doesn't know.
    let mut other = file;
    other[at + 24 + 2] = 2;
    let e = from_bytes("t.mf4", other, &CaptureOptions::default()).err().expect("unknown zip type");
    assert!(e.to_string().contains("zip type 2"), "{e}");
}

/// Opened from a file with a cache folder: unpacked into the cache once,
/// read from there again; a changed file is unpacked anew, and the cache
/// keeps under its limit.
#[test]
fn compressed_files_are_unpacked_into_the_cache_once() {
    use crate::input::capture::{cache_usage, clear_cache};
    let dir = std::env::temp_dir().join(format!("nerka-unpack-{}", std::process::id()));
    let cache = dir.join("cache");
    std::fs::create_dir_all(&dir).unwrap();
    let log = dir.join("a.mf4");
    let frames =
        vec![(1_000_000, 1, 0x0800, ip_dlt(1, "NAV", "first")), (2_000_000, 1, 0x0800, ip_dlt(2, "NAV", "second"))];
    std::fs::write(&log, x2e_packed(true, &frames, false, 61, Pack::Both(None))).unwrap();
    let mut o = crate::OpenOptions::default();
    o.capture.unpack.dir = Some(cache.clone());
    let p = crate::Progress::new();
    let text = |o: &crate::OpenOptions| {
        let f = crate::open_with(&log, o, &p).unwrap();
        (0..f.line_count()).map(|l| f.line_text(l).unwrap().rsplit('\t').next().unwrap().to_owned()).collect::<Vec<_>>()
    };
    assert_eq!(text(&o), ["first", "second"]);
    assert_eq!(cache_usage(&cache).0, 1);
    // The second time it comes from the cache: change the cached text to see.
    let cached = std::fs::read_dir(&cache)
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .find(|p| p.extension().is_some_and(|x| x == "mf4data"))
        .unwrap();
    let mut bytes = std::fs::read(&cached).unwrap();
    let at = bytes.windows(6).position(|w| w == b"second").unwrap();
    bytes[at..at + 6].copy_from_slice(b"SECOND");
    std::fs::write(&cached, bytes).unwrap();
    assert_eq!(text(&o), ["first", "SECOND"]);
    // A changed file is unpacked again; over the limit, the older file goes.
    let f = std::fs::File::options().write(true).open(&log).unwrap();
    f.set_modified(std::time::SystemTime::now() + std::time::Duration::from_secs(60)).unwrap();
    drop(f);
    o.capture.unpack.limit = 1;
    assert_eq!(text(&o), ["first", "second"]);
    let files: Vec<_> = std::fs::read_dir(&cache).unwrap().flatten().map(|e| e.path()).collect();
    assert_eq!(files.len(), 1, "{files:?}");
    assert_ne!(files[0], cached);
    assert_eq!(clear_cache(&cache), (1, 0));
    assert_eq!(cache_usage(&cache), (0, 0));
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn start_time_with_the_local_time_flag() {
    let start = 1_700_000_000_123_456_789u64;
    let hd = |flags: u8| {
        let mut d = start.to_le_bytes().to_vec();
        d.extend_from_slice(&[0, 0, 0, 0, flags, 0, 0, 0]);
        d
    };
    // UTC (no flag, or only "offsets valid"), or the flag ignored.
    assert_eq!(super::start_time(&hd(0), true), start);
    assert_eq!(super::start_time(&hd(2), true), start);
    assert_eq!(super::start_time(&hd(1), false), start);
    // The recorder's wall clock, read in this computer's zone.
    let secs = (start / 1_000_000_000) as f64;
    let shift = (crate::time::local_to_utc(secs) - secs) as i64 * 1_000_000_000;
    assert_eq!(super::start_time(&hd(1), true) as i64, start as i64 + shift);
    // A short header block is UTC.
    assert_eq!(super::start_time(&start.to_le_bytes(), true), start);
}
