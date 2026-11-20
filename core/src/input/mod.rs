//! Format-independent access to log lines.
//!
//! Every input format (plain text, pcap, DLT, …) implements [`LogSource`].
//! Binary formats decode records into displayable text lines; the search
//! engine and UIs only ever see this trait.

pub mod capture;
pub mod concat;
pub mod dlt;
pub mod json;
pub mod merged;
pub mod proto;
pub mod records;
pub(crate) mod schema;

use crate::event::{FieldDef, FieldId, FieldValue, Level};
use crate::{LineNo, Progress, Result};
use memmap2::Mmap;
use std::borrow::Cow;
use std::fs::File;
use std::io::Read;
use std::path::Path;

/// How files are opened.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OpenOptions {
    /// How DLT is found in captures (pcap, pcapng, MF4).
    pub capture: capture::CaptureOptions,
    /// Descriptions of DLT non-verbose messages (FIBEX).
    pub fibex: Option<std::sync::Arc<dlt::fibex::Fibex>>,
    /// How a text file is split into rows: lines, multi-line records or
    /// JSON; `None`: JSON if the file looks like JSON, else lines.
    pub records: Option<records::RecordSplit>,
    /// Protocols turned off for single captures (by path), instead of
    /// [`capture::CaptureOptions::disabled`]: a choice made when opening
    /// several files.
    pub disabled_for: Vec<(std::path::PathBuf, Vec<String>)>,
}

impl OpenOptions {
    /// The capture options for `path`: its own protocol choice, if it has
    /// one.
    pub fn capture_for(&self, path: &Path) -> std::borrow::Cow<'_, capture::CaptureOptions> {
        match self.disabled_for.iter().find(|(p, _)| p == path) {
            Some((_, off)) => {
                std::borrow::Cow::Owned(capture::CaptureOptions { disabled: off.clone(), ..self.capture.clone() })
            }
            None => std::borrow::Cow::Borrowed(&self.capture),
        }
    }
}

/// Open `path` with the reader its content calls for (default
/// [`OpenOptions`]): see [`open_with`].
pub fn open(path: impl AsRef<Path>, progress: &Progress) -> Result<Box<dyn LogSource>> {
    open_with(path, &OpenOptions::default(), progress)
}

/// Open `path` with the reader its content calls for: DLT if it starts
/// with a DLT storage header, the DLT inside a pcap / pcapng / MF4 capture,
/// plain text otherwise.
pub fn open_with(path: impl AsRef<Path>, options: &OpenOptions, progress: &Progress) -> Result<Box<dyn LogSource>> {
    let path = path.as_ref();
    let mut head = [0u8; 16];
    let n = File::open(path)?.read(&mut head)?;
    let head = &head[..n];
    Ok(if dlt::is_dlt(head) {
        Box::new(dlt::DltFile::open(path, progress)?.with_fibex(options.fibex.clone()))
    } else if capture::is_capture(head) {
        Box::new(capture::open(path, &options.capture_for(path), progress)?.0.with_fibex(options.fibex.clone()))
    } else {
        // JSON is told from the start of the file (a first value whole).
        let mut start = vec![0u8; 64 * 1024];
        let n = File::open(path)?.read(&mut start)?;
        match records::RecordSplit::choose(options.records.as_ref(), &start[..n]) {
            records::RecordSplit::Lines => Box::new(text::TextFile::open(path, progress)?),
            split => Box::new(records::RecordFile::open(path, split, progress)?),
        }
    })
}

/// A [`LogSource::data_id`] no other source has had in this process.
pub(crate) fn new_data_id() -> usize {
    static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(1);
    NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

/// File contents: a read-only mapping, or owned bytes (tests, empty files).
pub(crate) enum Bytes {
    Mapped(Mmap),
    Owned(Vec<u8>),
}

impl Bytes {
    /// Map `path` read-only. If another process truncates the file while
    /// it is mapped, reads may fault: the same trade-off klogg and ripgrep
    /// make for speed. Follow/tail mode will remap on growth (see roadmap).
    pub(crate) fn map(path: &Path) -> Result<Bytes> {
        let file = File::open(path)?;
        Ok(if file.metadata()?.len() == 0 {
            Bytes::Owned(Vec::new())
        } else {
            // SAFETY: read-only mapping; see doc comment above.
            Bytes::Mapped(unsafe { Mmap::map(&file)? })
        })
    }

    pub(crate) fn bytes(&self) -> &[u8] {
        match self {
            Bytes::Mapped(m) => m,
            Bytes::Owned(v) => v,
        }
    }
}

/// Bring a mapped file's pages in on every core before a reader walks it
/// on one thread (review 2026-11-14, P8: an open was mostly the kernel's
/// page faults, one per 4 KiB, all on the reader's thread). Reads a byte
/// per page; stops early when cancelled (the reader then says so).
pub(crate) fn prefault(data: &[u8], progress: &Progress) {
    use rayon::prelude::*;
    const PAGE: usize = 4096;
    if data.len() < 16 << 20 {
        return;
    }
    let sum: u64 = data
        .par_chunks(4 << 20)
        .map(|c| if progress.is_cancelled() { 0 } else { c.iter().step_by(PAGE).map(|&b| u64::from(b)).sum() })
        .sum();
    std::hint::black_box(sum);
}

/// A column of a source whose lines are tab-separated cells (DLT).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ColumnDef {
    /// Owned for columns a protocol definition names (`input::proto::ksy`).
    pub name: Cow<'static, str>,
    /// Typical content width in characters; 0 for the last column, which
    /// takes the rest of the line.
    pub width: u16,
}

impl ColumnDef {
    /// A column with a fixed name.
    pub const fn new(name: &'static str, width: u16) -> ColumnDef {
        ColumnDef { name: Cow::Borrowed(name), width }
    }
}

/// What a source can say about itself besides its lines (review
/// 2026-11-09, R2 and R3: counts that reached no one; 2026-11-08, R8).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SourceInfo {
    /// The protocols of its messages, by name, each once (empty: none).
    pub protocols: Vec<String>,
    /// What it dropped or couldn't read, for people: `14 bytes skipped:
    /// not DLT messages`. Empty: nothing.
    pub notes: Vec<String>,
    /// Messages without `net.*` fields: their sender wasn't kept (a capture
    /// of more senders than it keeps).
    pub no_sender: u64,
    /// The values of `file` in a view of several files, one per file in
    /// order (empty: one file).
    pub files: Vec<String>,
    /// What the notes count, one by one (the first [`Skips::MAX`] of each
    /// file), for finding them: in file order.
    pub skips: std::sync::Arc<Vec<Skip>>,
    /// Skips past those listed.
    pub skips_more: u64,
    /// What the files say about themselves that helps tell why something
    /// is missing: a capture's own drop counts.
    pub about: Vec<String>,
}

/// A piece of a file a reader couldn't read, and where it was: what the
/// counts in [`SourceInfo::notes`] are made of.
#[derive(Debug, Clone, PartialEq)]
pub struct Skip {
    pub kind: SkipKind,
    /// The first line read after it (0-based, in the source's lines; none:
    /// none after it).
    pub line: Option<LineNo>,
    /// When: the capture time of its frame, or of the message before it in
    /// a `.dlt` file (seconds since 1970, UTC).
    pub time: Option<f64>,
    /// Its frame's number in a pcap / pcapng file, from 1: Wireshark's
    /// `frame.number`.
    pub frame: Option<u64>,
    /// Where it starts, in bytes from the start of the file (pcap, pcapng,
    /// `.dlt`).
    pub offset: Option<u64>,
    /// How many bytes (a lost fragment: the bytes that did arrive).
    pub bytes: u64,
    /// The protocol and the datagram's addresses, ports and VLAN, as known:
    /// `DLT 10.0.0.1:3490 → 10.0.0.2:3490 VLAN 5`.
    pub detail: String,
    /// Its file, in a view of several (empty: one file).
    pub file: String,
    /// Its first bytes (at most [`Skip::HEAD`]): what the data looked
    /// like, for a bug report without the file. A datagram that lost a
    /// fragment: the datagram's start, if that fragment came.
    pub head: Vec<u8>,
    /// Its frame was cut by the capture (its snap length): bytes kept, and
    /// the frame's length on the wire.
    pub cut: Option<(u64, u64)>,
}

impl Skip {
    /// Bytes kept of each skip.
    pub const HEAD: usize = 16;

    /// The first [`Skip::HEAD`] bytes of `b`.
    pub fn head_of(b: &[u8]) -> Vec<u8> {
        b[..b.len().min(Self::HEAD)].to_vec()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkipKind {
    /// Bytes of a datagram, or of a `.dlt` file, that weren't whole
    /// messages.
    NotMessages,
    /// A datagram whose fragments didn't all arrive.
    LostFragment,
    /// Bytes of the file that couldn't be read as records.
    Unreadable,
}

impl SkipKind {
    pub fn text(self) -> &'static str {
        match self {
            SkipKind::NotMessages => "not whole messages",
            SkipKind::LostFragment => "lost a fragment",
            SkipKind::Unreadable => "unreadable",
        }
    }
}

/// Where an export's skips go, shared by a reader's parts.
type SkipSink = std::rc::Rc<std::cell::RefCell<Box<dyn FnMut(&Skip)>>>;

/// Skips as a reader finds them: the first [`Skips::MAX`] listed and the
/// rest counted, or, for an export ([`Skips::streaming`]), each handed on
/// as it is found and none kept (a badly damaged file can have millions).
#[derive(Default)]
pub struct Skips {
    pub list: Vec<Skip>,
    pub more: u64,
    /// Export: where each skip goes (shared by a reader's parts); the
    /// last one is held until the next shows whether it runs on.
    stream: Option<SkipSink>,
    held: Option<Skip>,
}

impl Skips {
    /// Skips listed per file, at most.
    pub const MAX: usize = 10_000;

    /// Skips handed to `out` one by one (in the order found), none kept.
    pub fn streaming(out: Box<dyn FnMut(&Skip)>) -> Skips {
        Skips { stream: Some(std::rc::Rc::new(std::cell::RefCell::new(out))), ..Skips::default() }
    }

    /// Another collector for a part of the same reader: an export's goes to
    /// the same place, a list is its own (joined with [`Skips::append`]).
    pub fn share(&self) -> Skips {
        Skips { stream: self.stream.clone(), ..Skips::default() }
    }

    /// Another part's list joined to this one (the cap holds).
    pub fn append(&mut self, mut other: Skips) {
        other.finish();
        let (list, more) = other.take();
        self.more += more;
        list.into_iter().for_each(|s| self.push(s));
    }

    pub fn push(&mut self, s: Skip) {
        if let Some(out) = &self.stream {
            if let Some(h) = self.held.replace(s) {
                (out.borrow_mut())(&h);
            }
        } else if self.list.len() < Self::MAX {
            self.list.push(s);
        } else {
            self.more += 1;
        }
    }

    /// Add to the last skip if `s` runs on from it (same kind, its bytes
    /// right after), else push `s`: bytes skipped piece by piece are one
    /// skip.
    pub fn push_or_extend(&mut self, s: Skip) {
        let last = if self.stream.is_some() { self.held.as_mut() } else { self.list.last_mut() };
        if let Some(last) = last
            && last.kind == s.kind
            && last.offset.zip(s.offset).is_some_and(|(a, b)| a + last.bytes == b)
        {
            last.bytes += s.bytes;
            return;
        }
        self.push(s);
    }

    /// The reader is done: an export gets the skip it held; a list is put
    /// in file order (by line, then time).
    pub fn finish(&mut self) {
        if let (Some(out), Some(h)) = (&self.stream, self.held.take()) {
            (out.borrow_mut())(&h);
        }
        self.list.sort_by(|a, b| {
            (a.line.unwrap_or(LineNo::MAX), a.time.unwrap_or(f64::MAX))
                .partial_cmp(&(b.line.unwrap_or(LineNo::MAX), b.time.unwrap_or(f64::MAX)))
                .unwrap_or(std::cmp::Ordering::Equal)
        });
    }

    /// The last skip pushed (an export's: the one still held).
    pub fn last_mut(&mut self) -> Option<&mut Skip> {
        if self.stream.is_some() { self.held.as_mut() } else { self.list.last_mut() }
    }

    /// Take the list (and the count past it).
    pub fn take(&mut self) -> (Vec<Skip>, u64) {
        (std::mem::take(&mut self.list), std::mem::take(&mut self.more))
    }
}

/// The header of [`skip_tsv`] rows.
pub const SKIPS_TSV_HEADER: &str =
    "kind\tline\ttime (UTC)\tframe\toffset\tbytes\tframe cut (kept/wire)\tdetail\tfile\thead (hex)";

/// A skip as a tab-separated row (no line break): kind, line (from 1),
/// time (UTC), frame, offset, bytes, whether the capture cut its frame
/// (bytes kept / on the wire), detail, file (in a view of several), its
/// first bytes in hex.
pub fn skip_tsv(s: &Skip) -> String {
    let opt = |v: Option<u64>| v.map_or_else(String::new, |v| v.to_string());
    let time = s.time.map_or_else(String::new, |t| crate::time::format_utc(t, 6));
    let cut = s.cut.map_or_else(String::new, |(kept, wire)| format!("{kept}/{wire}"));
    let head: Vec<String> = s.head.iter().map(|b| format!("{b:02x}")).collect();
    format!(
        "{}\t{}\t{time}\t{}\t{}\t{}\t{cut}\t{}\t{}\t{}",
        s.kind.text(),
        opt(s.line.map(|l| l + 1)),
        opt(s.frame),
        opt(s.offset),
        s.bytes,
        s.detail.replace(['\t', '\n'], " "),
        s.file.replace(['\t', '\n'], " "),
        head.join(" ")
    )
}

/// What an export of skips says about where it came from, for a bug
/// report: `# key: value` lines before the table.
pub fn skips_report_header(files: &[(&Path, String)], options: &OpenOptions) -> String {
    let mut out = format!(
        "# nerka {} skips export: what couldn't be read, one row each\n# system: {} {}\n",
        env!("CARGO_PKG_VERSION"),
        std::env::consts::OS,
        std::env::consts::ARCH
    );
    for (path, format) in files {
        // The file's name only: a path can say more than the user means to.
        let name = path.file_name().map_or_else(|| path.display().to_string(), |n| n.to_string_lossy().into());
        let size = std::fs::metadata(path).map_or_else(|_| "?".into(), |m| m.len().to_string());
        out.push_str(&format!("# file: {name} ({size} bytes, {format})\n"));
    }
    out.push_str(&format!("# capture options: {}\n", options.capture.describe()));
    out.push_str(&format!(
        "# head: each skip's first {} bytes (a datagram's: its start); the rest of the data is not in this file\n",
        Skip::HEAD
    ));
    out
}

/// Counts of an export, for its summary.
#[derive(Debug, Default)]
pub struct SkipSummary {
    /// Per kind: skips and bytes.
    pub kinds: Vec<(SkipKind, u64, u64)>,
    /// Per detail (protocol and addresses): skips.
    pub details: std::collections::HashMap<String, u64>,
    /// Skips in frames the capture cut (its snap length).
    pub cut: u64,
    /// What each capture's own statistics say it dropped.
    pub capture: Vec<String>,
}

impl SkipSummary {
    pub fn add(&mut self, s: &Skip) {
        self.cut += u64::from(s.cut.is_some());
        match self.kinds.iter_mut().find(|k| k.0 == s.kind) {
            Some(k) => {
                k.1 += 1;
                k.2 += s.bytes;
            }
            None => self.kinds.push((s.kind, 1, s.bytes)),
        }
        // Bounded: a capture keeps at most 65,536 senders.
        if self.details.len() < 100_000 || self.details.contains_key(&s.detail) {
            *self.details.entry(s.detail.clone()).or_default() += 1;
        }
    }

    /// `# summary …` lines: per kind, and the 20 details with most skips.
    pub fn text(&self) -> String {
        let mut out = String::from("# summary\n");
        if self.kinds.is_empty() {
            out.push_str("# nothing skipped\n");
        }
        for (k, n, bytes) in &self.kinds {
            out.push_str(&format!("# {}: {n} skips, {bytes} bytes\n", k.text()));
        }
        if self.cut > 0 {
            out.push_str(&format!(
                "# {} of them in frames the capture cut (its snap length): see 'frame cut'\n",
                self.cut
            ));
        }
        for c in &self.capture {
            out.push_str(&format!("# {c}\n"));
        }
        let mut d: Vec<(&String, &u64)> = self.details.iter().collect();
        d.sort_by(|a, b| b.1.cmp(a.1).then(a.0.cmp(b.0)));
        for (detail, n) in d.iter().take(20) {
            out.push_str(&format!("# from {}: {n}\n", if detail.is_empty() { "-" } else { detail }));
        }
        if d.len() > 20 {
            out.push_str(&format!("# … and {} more\n", d.len() - 20));
        }
        out
    }
}

/// What kind of file `head` (its first bytes) starts, as an export of
/// skips names it.
fn kind_of(head: &[u8]) -> &'static str {
    if dlt::is_dlt(head) {
        "DLT file"
    } else if capture::pcap::is_pcapng(head) {
        "pcapng"
    } else if capture::pcap::is_pcap(head) {
        "pcap"
    } else if capture::mf4::is_mf4(head) {
        "MF4"
    } else {
        "text: nothing is skipped"
    }
}

fn file_head(path: &Path) -> Result<Vec<u8>> {
    let mut head = vec![0u8; 64];
    let n = File::open(path)?.read(&mut head)?;
    head.truncate(n);
    Ok(head)
}

/// Every skip of `files` (path and the name its rows give it; a `.dlt`
/// file or a capture: others have none) written to `out` as a report for
/// a bug: [`skips_report_header`], the table ([`skip_tsv`] rows, in the
/// order found) and [`SkipSummary`]. The files are read again and no skip
/// is kept, however many there are. Returns how many were written; the
/// first error writing to `out` stops the export.
pub fn export_skips(
    files: &[(std::path::PathBuf, String)],
    options: &OpenOptions,
    out: Box<dyn std::io::Write>,
    progress: &Progress,
) -> Result<u64> {
    use std::cell::RefCell;
    use std::rc::Rc;
    let heads: Vec<Vec<u8>> = files.iter().map(|(p, _)| file_head(p)).collect::<Result<_>>()?;
    let kinds: Vec<(&Path, String)> =
        files.iter().zip(&heads).map(|((p, _), h)| (p.as_path(), kind_of(h).to_owned())).collect();
    let out = Rc::new(RefCell::new(out));
    // How many were written, the first write error, the summary.
    let state = Rc::new(RefCell::new((0u64, None::<std::io::Error>, SkipSummary::default())));
    let write = |text: &str| -> Result<()> { Ok(out.borrow_mut().write_all(text.as_bytes())?) };
    write(&skips_report_header(&kinds, options))?;
    write(SKIPS_TSV_HEADER)?;
    write("\n")?;
    for ((path, name), head) in files.iter().zip(&heads) {
        let sink: Box<dyn FnMut(&Skip)> = {
            let (state, out, name) =
                (state.clone(), out.clone(), if files.len() > 1 { name.clone() } else { String::new() });
            Box::new(move |s: &Skip| {
                let mut st = state.borrow_mut();
                if st.1.is_some() {
                    return;
                }
                st.2.add(s);
                let row = skip_tsv(&Skip { file: name.clone(), ..s.clone() }) + "\n";
                match out.borrow_mut().write_all(row.as_bytes()) {
                    Ok(()) => st.0 += 1,
                    Err(e) => st.1 = Some(e),
                }
            })
        };
        let skips = Skips::streaming(sink);
        if dlt::is_dlt(head) {
            let data = Bytes::map(path)?;
            dlt::index_skips(data.bytes(), progress, skips)?;
        } else if capture::is_capture(head) {
            let stats = capture::read_skips(path, &options.capture_for(path), progress, skips)?;
            let report = stats.drops_report();
            let line = if files.len() > 1 { format!("{name}: {report}") } else { report };
            state.borrow_mut().2.capture.push(line);
        }
        if let Some(e) = state.borrow_mut().1.take() {
            return Err(e.into());
        }
    }
    let summary = state.borrow().2.text();
    write(&summary)?;
    out.borrow_mut().flush()?;
    let written = state.borrow().0;
    Ok(written)
}

/// What a query on `source` can't find, said before it finds nothing: a
/// `proto` no message has, `net.*` of messages whose sender wasn't kept, a
/// `file` no file is (with what to write instead).
pub fn query_warnings(query: &crate::Query, source: &dyn LogSource) -> Vec<String> {
    let info = source.info();
    let mut out = Vec::new();
    if !info.protocols.is_empty() {
        for v in query.values_compared("proto") {
            if !info.protocols.iter().any(|p| p.eq_ignore_ascii_case(&v)) {
                out.push(format!("no message here is of a protocol '{v}' (there are: {})", info.protocols.join(", ")));
            }
        }
    }
    if info.no_sender > 0 && query.expr().field_names().iter().any(|f| f.starts_with("net.")) {
        out.push(format!(
            "net.* has no value for {} messages: the capture has more senders than are kept",
            info.no_sender
        ));
    }
    // A full path, or a name two files share: the value to write (review
    // 2026-11-09 R15).
    if !info.files.is_empty() {
        let ends = |a: &str, b: &str| a == b || a.ends_with(&format!("/{b}"));
        for v in query.values_compared(concat::FILE_FIELD) {
            if info.files.contains(&v) {
                continue;
            }
            let path = v.replace('\\', "/");
            let near: Vec<String> =
                info.files.iter().filter(|f| ends(&path, f) || ends(f, &path)).map(|f| format!("'{f}'")).collect();
            out.push(if near.is_empty() {
                format!("no file here is '{v}' (there are: {})", info.files.join(", "))
            } else {
                format!("no file here is '{v}': write {}", near.join(" or "))
            });
        }
    }
    out
}

pub trait LogSource: Send + Sync {
    /// Human readable name (usually the file path).
    fn name(&self) -> &str;

    fn line_count(&self) -> LineNo;

    /// Bytes of a line without its terminator. Text sources borrow them
    /// from the file; decoded formats render them.
    fn line_bytes(&self, line: LineNo) -> Option<Cow<'_, [u8]>>;

    /// Line as text; invalid UTF-8 is replaced with U+FFFD.
    fn line_text(&self, line: LineNo) -> Option<Cow<'_, str>> {
        self.line_bytes(line).map(|b| match b {
            Cow::Borrowed(b) => String::from_utf8_lossy(b),
            Cow::Owned(v) => Cow::Owned(String::from_utf8_lossy(&v).into_owned()),
        })
    }

    /// The line's cells for a detailed view of one line: one per
    /// [`Self::columns`] entry (plain text: the whole line as one cell).
    /// Unlike [`Self::line_text`], which flattens a decoded message's own
    /// line breaks and tabs into spaces so it stays one row of cells, the
    /// values keep them (DLT: a payload logged as several lines).
    fn line_detail(&self, line: LineNo) -> Option<Vec<String>> {
        let text = self.line_text(line)?;
        Some(match self.columns().len() {
            0 => vec![text.into_owned()],
            n => text.splitn(n, '\t').map(str::to_owned).collect(),
        })
    }

    /// Size of the underlying file or buffer.
    fn size_bytes(&self) -> u64 {
        0
    }

    /// Memory used by the source's line or message index.
    fn index_memory_bytes(&self) -> usize {
        0
    }

    /// Its protocols, and what it dropped or couldn't read.
    fn info(&self) -> SourceInfo {
        SourceInfo::default()
    }

    /// What the source is read as, for people: `text`, `JSON`,
    /// `pcap: DLT + SOMEIP`.
    fn format(&self) -> String {
        "text".into()
    }

    /// Columns of the rendered lines, if they are tab-separated cells. UIs
    /// show such sources as a table; plain text has none.
    fn columns(&self) -> &[ColumnDef] {
        &[]
    }

    /// Fields this source supplies for every line (DLT: `ecu`, `apid`, …).
    /// Queries refer to them by name; plain text has none.
    fn schema(&self) -> &[FieldDef] {
        &[]
    }

    /// Value of field `id` (an index into [`Self::schema`]) for `line`.
    /// `None` when the line has no value for it. Must not render the line:
    /// header-only filters rely on this being cheap.
    fn field(&self, _line: LineNo, _id: FieldId) -> Option<FieldValue<'_>> {
        None
    }

    /// The level the format itself records for `line`. The outer `None`
    /// means the source doesn't supply levels, so they're detected from
    /// the text instead.
    fn supplied_level(&self, _line: LineNo) -> Option<Option<Level>> {
        None
    }

    /// Contiguous byte buffer holding all lines separated by `\n`, if the
    /// source has one. Text files return their mapping, which lets the
    /// search engine run the regex over the whole buffer instead of
    /// line-by-line. Decoded formats may return `None`.
    fn contiguous(&self) -> Option<&[u8]> {
        None
    }

    /// Map a byte offset inside [`Self::contiguous`] to a line number.
    fn line_of_offset(&self, _offset: u64) -> LineNo {
        0
    }

    /// Byte offset of the start of `line` inside [`Self::contiguous`].
    fn line_start(&self, _line: LineNo) -> u64 {
        0
    }

    /// For a source made of several ([`concat::Concat`]): its parts, each
    /// with its first line in the whole. The engine searches them one by
    /// one, so each keeps its own fast path.
    fn parts(&self) -> Option<Vec<Part<'_>>> {
        None
    }

    /// For a source whose rows are another source's lines in another order
    /// ([`merged::Merged`]: files' lines sorted by time): that source and the
    /// map both ways. The engine scans that source (its fast path) and maps
    /// the lines, unless a query depends on row numbers.
    fn reordered(&self) -> Option<Reorder<'_>> {
        None
    }

    /// `(first line, line count)` of each file the source shows: one for a
    /// plain file, one per part for [`concat::Concat`] (bookmarks are kept
    /// per file).
    fn part_files(&self) -> Vec<(LineNo, LineNo)> {
        vec![(0, self.line_count())]
    }

    /// The time of `line` as the source knows it, in seconds since the Unix
    /// epoch (UTC): DLT's storage (receive) time. Plain text has none: a
    /// tab captures it with a `time` signal instead.
    fn time(&self, _line: LineNo) -> Option<f64> {
        None
    }

    /// Argument `index` of `line`'s message, if the source knows the
    /// message's arguments (DLT non-verbose messages described by FIBEX):
    /// a value without a regex, for signals.
    fn argument(&self, _line: LineNo, _index: usize) -> Option<crate::signal::SignalValue> {
        None
    }

    /// How long the line's writer had been running when it wrote the line,
    /// in seconds, if the format records it (DLT's `tmsp`): the built-in
    /// `@uptime`, for lifecycles ([`crate::lifecycle`]).
    fn uptime(&self, _line: LineNo) -> Option<f64> {
        None
    }

    /// Where `line` is, to find it again when row numbers change
    /// (bookmarks): see [`Anchor`]. `None` if the source can't say (a view
    /// of several files: ask its [`Self::parts`]).
    fn anchor(&self, _line: LineNo) -> Option<Anchor> {
        None
    }

    /// The line that holds an anchor made in another view of the file (a
    /// record holding a line anchored when the file was read as lines),
    /// if the content there still matches.
    fn line_holding(&self, _anchor: &Anchor) -> Option<LineNo> {
        None
    }

    /// The lines that start at these byte offsets of the file (an
    /// [`Anchor::offset`]), in one pass; `None` where no line starts there.
    fn lines_at(&self, offsets: &[u64]) -> Vec<Option<LineNo>> {
        vec![None; offsets.len()]
    }

    /// Identifies the data the source reads, for caches (signal events): a
    /// part of a [`concat::Concat`] answers its file's, so the part made
    /// for each scan is the same one. Files take a `new_data_id` when
    /// opened (an address could be reused by the next file); the default,
    /// the address, is for sources that live as long as their caches (tests).
    fn data_id(&self) -> usize {
        self as *const Self as *const () as usize
    }
}

/// Rows that are another source's lines in another order
/// ([`LogSource::reordered`]).
pub struct Reorder<'a> {
    pub inner: &'a dyn LogSource,
    /// Row -> line of `inner`.
    pub line_of: &'a [LineNo],
    /// Line of `inner` -> row.
    pub row_of: &'a [LineNo],
}

/// Where a line is in its file, to find it again when row numbers change
/// (a capture indexed with other ports, a file that changed): the byte
/// offset it starts at (`None` if its bytes aren't contiguous in the file,
/// e.g. a reassembled datagram) and a hash of its first bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Anchor {
    pub offset: Option<u64>,
    pub hash: u32,
}

/// Bytes of a line that its [`Anchor::hash`] covers.
pub const ANCHOR_BYTES: usize = 64;

impl Anchor {
    /// The anchor of a line starting at `offset` whose bytes are `bytes`.
    pub fn of(offset: Option<u64>, bytes: &[u8]) -> Anchor {
        // FNV-1a, 32 bits.
        let hash = bytes[..bytes.len().min(ANCHOR_BYTES)]
            .iter()
            .fold(0x811c_9dc5u32, |h, &b| (h ^ u32::from(b)).wrapping_mul(0x0100_0193));
        Anchor { offset, hash }
    }

    /// The anchor of text at `offset` whose bytes from there are `bytes`:
    /// the hash covers its first line only (without its line break), so a
    /// line and a record starting there have the same anchor.
    pub fn of_text(offset: Option<u64>, bytes: &[u8]) -> Anchor {
        let head = &bytes[..bytes.len().min(ANCHOR_BYTES)];
        let line = head.split(|&b| b == b'\n').next().unwrap_or_default();
        Anchor::of(offset, line.strip_suffix(b"\r").unwrap_or(line))
    }
}

/// One part of a source made of several.
pub struct Part<'a> {
    /// Its first line in the whole.
    pub first: LineNo,
    pub source: Box<dyn LogSource + 'a>,
}
pub mod text;

#[cfg(test)]
pub(crate) mod mem;
