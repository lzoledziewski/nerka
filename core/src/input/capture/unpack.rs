//! Compressed MF4 data (DZ blocks), unpacked once into a cache file.
//!
//! The reader works on bytes it can borrow: frames point into the mapped
//! file, which a deflate stream can't offer. A file with DZ blocks therefore
//! gets its data blocks — all of them, unpacked or copied, in the order the
//! block tree reaches them — written as a stream into one file in the
//! unpack cache, which is mapped like any log. The block tree is still read
//! from the original; `Layout` says where each data block's bytes are in
//! the unpacked file. Memory stays as low as for an uncompressed file, and
//! the search is as fast; the cost is disk.
//!
//! The cache file is named after the original's path, size and
//! modification time, so opening the file again maps it without unpacking.
//! The cache keeps under a size limit by removing the least recently used
//! files after writing a new one (a file in use stays: on Windows it can't
//! be removed while mapped). Without a cache folder
//! ([`UnpackOptions::dir`]), the data is unpacked into memory.

use super::super::Bytes;
use crate::{Error, Progress, Result};
use miniz_oxide::inflate::stream::{InflateState, inflate};
use miniz_oxide::{DataFormat, MZError, MZFlush, MZStatus};
use rayon::prelude::*;
use std::collections::HashMap;
use std::fs::File;
use std::hash::{Hash, Hasher};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// The unpack cache's default size limit: 8 GiB.
pub const DEFAULT_UNPACK_LIMIT: u64 = 8 << 30;

/// Extension of the cache's files.
const EXT: &str = "mf4data";

/// Where compressed data is unpacked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnpackOptions {
    /// The cache folder ([`crate::cache_dir`] in the CLI and GUI); `None`
    /// unpacks into memory.
    pub dir: Option<PathBuf>,
    /// Bytes the cache keeps at most (least recently used files go first).
    pub limit: u64,
}

impl Default for UnpackOptions {
    fn default() -> Self {
        Self { dir: None, limit: DEFAULT_UNPACK_LIMIT }
    }
}

/// One data block (DT, SD, RD, DV or DZ) as the reader will see it.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Leaf {
    /// The block's address in the file.
    pub at: u64,
    /// Where its data section is in the file, and how long it is there.
    pub data_at: usize,
    pub data_len: usize,
    /// For a DZ block: its zip type, zip parameter and unpacked length.
    pub dz: Option<(u8, u32, u64)>,
}

impl Leaf {
    fn len(&self) -> u64 {
        self.dz.map_or(self.data_len as u64, |(_, _, n)| n)
    }
}

/// Where the data blocks' bytes are: block address -> (offset, length) in
/// the unpacked bytes.
pub(crate) type Layout = HashMap<u64, (usize, usize)>;

/// A DZ block's data section: the block type it replaces, zip type, zip
/// parameter, unpacked and packed lengths, then the packed bytes.
pub(crate) fn dz_header(data: &[u8]) -> Result<(u8, u32, u64, &[u8])> {
    let bad = || Error::Format("MF4: damaged DZ block".into());
    if data.len() < 24 {
        return Err(bad());
    }
    let zip_type = data[2];
    let param = u32::from_le_bytes(data[4..8].try_into().unwrap());
    let org = u64::from_le_bytes(data[8..16].try_into().unwrap());
    let packed = usize::try_from(u64::from_le_bytes(data[16..24].try_into().unwrap())).map_err(|_| bad())?;
    let bytes = data.get(24..24usize.checked_add(packed).ok_or_else(bad)?).ok_or_else(bad)?;
    if zip_type > 1 {
        return Err(Error::Format(format!("MF4: DZ zip type {zip_type} isn't supported (only deflate)")));
    }
    Ok((zip_type, param, org, bytes))
}

/// Unpack (or find in the cache) the data blocks `leaves` of the MF4 in
/// `buf`. `None` when none is compressed: the reader borrows from the file
/// as it is. Sets `progress`'s total for the unpacking and the reading of
/// the unpacked bytes after it.
pub(crate) fn unpack(
    buf: &[u8],
    path: Option<&Path>,
    leaves: &[Leaf],
    options: &UnpackOptions,
    progress: &Progress,
) -> Result<Option<(Bytes, Layout)>> {
    if !leaves.iter().any(|l| l.dz.is_some()) {
        return Ok(None);
    }
    let mut layout = Layout::new();
    let mut total = 0u64;
    for l in leaves {
        let len = usize::try_from(l.len()).map_err(|_| Error::Format("MF4: data too large".into()))?;
        layout.insert(l.at, (total as usize, len));
        total = total.checked_add(l.len()).ok_or_else(|| Error::Format("MF4: data too large".into()))?;
    }
    let packed: u64 = leaves.iter().map(|l| l.data_len as u64).sum();
    let (Some(dir), Some(path)) = (&options.dir, path) else {
        // Into memory (no cache folder, or bytes without a file). Reserve
        // only what a damaged length can't blow up.
        progress.set_total(packed + 2 * total);
        let mut v = Vec::with_capacity(total.min(256 << 20) as usize);
        write_all(buf, leaves, &mut v, progress)?;
        return Ok(Some((Bytes::Owned(v), layout)));
    };
    let target = dir.join(format!("{:016x}.{EXT}", key(path)?));
    if std::fs::metadata(&target).is_ok_and(|m| m.len() == total) {
        // Used again: it is the most recently used now.
        let _ = File::options().write(true).open(&target).and_then(|f| f.set_modified(SystemTime::now()));
        progress.set_total(total);
        return Ok(Some((Bytes::map(&target)?, layout)));
    }
    progress.set_total(packed + 2 * total);
    std::fs::create_dir_all(dir).map_err(|e| cache_error(dir, e))?;
    let tmp = dir.join(format!(".{:016x}.{}.tmp", key(path)?, std::process::id()));
    let written = (|| -> Result<()> {
        let mut w = BufWriter::with_capacity(1 << 20, File::create(&tmp)?);
        write_all(buf, leaves, &mut w, progress)?;
        w.into_inner().map_err(|e| e.into_error())?;
        Ok(())
    })();
    if let Err(e) = written {
        let _ = std::fs::remove_file(&tmp);
        return Err(match e {
            Error::Io(e) => cache_error(dir, e),
            e => e,
        });
    }
    if std::fs::rename(&tmp, &target).is_err() {
        // Another tab or instance put it there first (and may have it
        // mapped): use that one.
        let _ = std::fs::remove_file(&tmp);
        if !std::fs::metadata(&target).is_ok_and(|m| m.len() == total) {
            return Err(Error::Format(format!("could not write to the unpack cache {}", dir.display())));
        }
    }
    trim(dir, options.limit, &target);
    Ok(Some((Bytes::map(&target)?, layout)))
}

fn cache_error(dir: &Path, e: std::io::Error) -> Error {
    Error::Format(format!("could not unpack into the cache {}: {e}", dir.display()))
}

/// The cache file's name: the original's path, size and modification time.
fn key(path: &Path) -> Result<u64> {
    let meta = std::fs::metadata(path)?;
    let mtime = meta.modified().ok().and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok());
    let mut h = std::hash::DefaultHasher::new();
    // The layout's version: a change to what the file holds changes it.
    1u32.hash(&mut h);
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf()).hash(&mut h);
    meta.len().hash(&mut h);
    mtime.hash(&mut h);
    Ok(h.finish())
}

/// Write every leaf's bytes, unpacking DZ blocks, checking for
/// cancellation per chunk.
fn write_all(buf: &[u8], leaves: &[Leaf], w: &mut impl Write, progress: &Progress) -> Result<()> {
    // Runs of DZ blocks up to this size are unpacked in parallel, a batch
    // at a time, and written in order (review 2026-11-14, P4b: unpacking
    // was one thread); bigger ones stream through one buffer.
    const IN_PARALLEL: u64 = 64 << 20;
    let batch = 2 * rayon::current_num_threads().max(1);
    let small = |l: &Leaf| l.dz.is_some_and(|(_, _, org)| org <= IN_PARALLEL);
    let mut i = 0;
    while i < leaves.len() {
        let mut j = i;
        while j < leaves.len() && j - i < batch && small(&leaves[j]) {
            j += 1;
        }
        if j > i {
            if progress.is_cancelled() {
                return Err(Error::Cancelled);
            }
            let blocks: Vec<Result<Vec<u8>>> =
                leaves[i..j].par_iter().map(|l| unpack_block(buf, l, progress)).collect();
            for b in blocks {
                w.write_all(&b?)?;
            }
            i = j;
            continue;
        }
        let l = &leaves[i];
        i += 1;
        let data = &buf[l.data_at..l.data_at + l.data_len];
        if l.dz.is_none() {
            for chunk in data.chunks(1 << 20) {
                if progress.is_cancelled() {
                    return Err(Error::Cancelled);
                }
                w.write_all(chunk)?;
                progress.add(2 * chunk.len() as u64);
            }
            continue;
        }
        let (zip_type, _, org, packed) = dz_header(data)?;
        if zip_type == 1 {
            w.write_all(&unpack_block(buf, l, progress)?)?;
        } else {
            progress.add((data.len() - packed.len()) as u64);
            inflate_into(packed, org, w, progress)?;
        }
    }
    Ok(())
}

/// A DZ block's data, unpacked whole (and put back in order if
/// transposed).
fn unpack_block(buf: &[u8], l: &Leaf, progress: &Progress) -> Result<Vec<u8>> {
    let data = &buf[l.data_at..l.data_at + l.data_len];
    let (zip_type, param, org, packed) = dz_header(data)?;
    let v = miniz_oxide::inflate::decompress_to_vec_zlib_with_limit(packed, org as usize)
        .map_err(|_| Error::Format("MF4: damaged DZ data".into()))?;
    if v.len() as u64 != org {
        return Err(Error::Format("MF4: DZ data shorter than its header says".into()));
    }
    progress.add(data.len() as u64 + org);
    Ok(if zip_type == 1 { untranspose(&v, param as usize) } else { v })
}

/// Inflate a zlib stream of `org` bytes into `w`, a chunk at a time.
fn inflate_into(packed: &[u8], org: u64, w: &mut impl Write, progress: &Progress) -> Result<()> {
    let bad = |what: &str| Error::Format(format!("MF4: DZ data {what}"));
    let mut state = InflateState::new_boxed(DataFormat::Zlib);
    let mut out = vec![0u8; 1 << 20];
    let (mut at, mut written) = (0usize, 0u64);
    loop {
        if progress.is_cancelled() {
            return Err(Error::Cancelled);
        }
        let r = inflate(&mut state, &packed[at..], &mut out, MZFlush::None);
        at += r.bytes_consumed;
        written += r.bytes_written as u64;
        if written > org {
            return Err(bad("longer than its header says"));
        }
        w.write_all(&out[..r.bytes_written])?;
        progress.add((r.bytes_consumed + r.bytes_written) as u64);
        match r.status {
            Ok(MZStatus::StreamEnd) => break,
            Ok(_) => {}
            Err(MZError::Buf) if r.bytes_consumed == 0 && r.bytes_written == 0 => return Err(bad("cut short")),
            Err(MZError::Buf) => {}
            Err(_) => return Err(bad("damaged")),
        }
    }
    if written != org {
        return Err(bad("shorter than its header says"));
    }
    Ok(())
}

/// Undo a DZ transposition: `cols` bytes per record, stored column by
/// column; the bytes after the last whole record are as they were.
fn untranspose(v: &[u8], cols: usize) -> Vec<u8> {
    if cols < 2 || v.len() < cols {
        return v.to_vec();
    }
    let rows = v.len() / cols;
    let mut out = vec![0u8; v.len()];
    for c in 0..cols {
        let col = &v[c * rows..(c + 1) * rows];
        for (r, &b) in col.iter().enumerate() {
            out[r * cols + c] = b;
        }
    }
    out[rows * cols..].copy_from_slice(&v[rows * cols..]);
    out
}

/// Keep the cache under `limit`: remove the least recently used files
/// (never `keep`), and leftovers of interrupted unpacking.
fn trim(dir: &Path, limit: u64, keep: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    let mut files = Vec::new();
    for e in entries.flatten() {
        let (p, Ok(meta)) = (e.path(), e.metadata()) else { continue };
        let modified = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
        let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if name.ends_with(".tmp") && name.starts_with('.') {
            // A day old: not being written any more.
            if modified.elapsed().is_ok_and(|a| a.as_secs() > 24 * 3600) {
                let _ = std::fs::remove_file(&p);
            }
        } else if p.extension().is_some_and(|x| x == EXT) {
            files.push((modified, meta.len(), p));
        }
    }
    let mut used: u64 = files.iter().map(|f| f.1).sum();
    files.sort();
    for (_, len, p) in files {
        if used <= limit {
            break;
        }
        if p != keep && std::fs::remove_file(&p).is_ok() {
            used -= len;
        }
    }
}

/// What the unpack cache in `dir` holds: files and bytes.
pub fn cache_usage(dir: &Path) -> (u64, u64) {
    let Ok(entries) = std::fs::read_dir(dir) else { return (0, 0) };
    entries
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == EXT))
        .filter_map(|e| e.metadata().ok())
        .fold((0, 0), |(n, b), m| (n + 1, b + m.len()))
}

/// Empty the unpack cache in `dir`. Returns the files removed and those
/// that are in use (open in a tab: they stay).
pub fn clear_cache(dir: &Path) -> (u64, u64) {
    let Ok(entries) = std::fs::read_dir(dir) else { return (0, 0) };
    let mut out = (0, 0);
    for e in entries.flatten().filter(|e| e.path().extension().is_some_and(|x| x == EXT)) {
        match std::fs::remove_file(e.path()) {
            Ok(()) => out.0 += 1,
            Err(_) => out.1 += 1,
        }
    }
    out
}
