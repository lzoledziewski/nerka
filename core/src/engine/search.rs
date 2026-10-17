//! Filtering: produce the set of matching lines (klogg's "filtered view").
//!
//! Fast path — sources with a contiguous buffer and a filter that has a
//! *prefilter regex* (every plain regex search; DSL queries containing a
//! positive `raw ~ /…/` in a top-level AND): the buffer is split into
//! line-aligned chunks searched in parallel. Inside a chunk the regex runs
//! over raw bytes; on a hit the candidate line is verified (full query, or a
//! cross-line check for plain regexes) and the scan skips to the next line.
//! Lines without hits cost only the SIMD-accelerated regex scan.
//!
//! Slow path — everything else (inverted regex, `level >= ERROR` alone,
//! decoded formats): every line is tested individually, still in parallel.

use crate::dsl::Query;
use crate::{LineNo, LogSource, Progress, Result};
use rayon::prelude::*;
use regex::bytes::{Regex, RegexBuilder};
use std::ops::Range;

const CHUNK_BYTES: u64 = 4 * 1024 * 1024;
const LINES_PER_TASK: u64 = 64 * 1024;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MatcherOptions {
    pub case_insensitive: bool,
    /// Treat the pattern as a literal string, not a regex.
    pub fixed_string: bool,
    /// Select lines that do NOT match.
    pub invert: bool,
}

/// Plain regex filter (the klogg-style search box).
#[derive(Debug, Clone)]
pub struct Matcher {
    regex: Regex,
    options: MatcherOptions,
}

impl Matcher {
    pub fn new(pattern: &str, options: MatcherOptions) -> Result<Self> {
        let pat = if options.fixed_string { regex::escape(pattern) } else { pattern.to_owned() };
        let regex = RegexBuilder::new(&pat)
            .case_insensitive(options.case_insensitive)
            // `^`/`$` anchor at line boundaries (LF or CRLF) when we scan a
            // whole buffer, exactly as when we test a single line.
            .multi_line(true)
            .crlf(true)
            .build()?;
        Ok(Self { regex, options })
    }

    pub fn options(&self) -> &MatcherOptions {
        &self.options
    }

    pub fn is_line_match(&self, line: &[u8]) -> bool {
        self.regex.is_match(line) != self.options.invert
    }
}

/// Anything the engine can filter by.
#[derive(Debug, Clone)]
pub enum Filter {
    Regex(Matcher),
    Query(Query),
}

impl Filter {
    pub fn matches(&self, line: &[u8]) -> bool {
        match self {
            Filter::Regex(m) => m.is_line_match(line),
            Filter::Query(q) => q.matches(line),
        }
    }

    fn prefilter(&self) -> Option<&Regex> {
        match self {
            Filter::Regex(m) if !m.options.invert => Some(&m.regex),
            Filter::Regex(_) => None,
            Filter::Query(q) => q.prefilter(),
        }
    }

    /// Merged, sorted, non-overlapping byte ranges to highlight in a line.
    pub fn highlighter(&self) -> Highlighter {
        match self {
            Filter::Regex(m) if !m.options.invert => Highlighter { regexes: vec![m.regex.clone()] },
            Filter::Regex(_) => Highlighter::default(),
            Filter::Query(q) => Highlighter { regexes: q.highlight_regexes() },
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct Highlighter {
    regexes: Vec<Regex>,
}

impl Highlighter {
    pub fn spans(&self, line: &[u8]) -> Vec<Range<usize>> {
        let mut v: Vec<Range<usize>> = self
            .regexes
            .iter()
            .flat_map(|re| re.find_iter(line).filter(|m| !m.is_empty()).map(|m| m.range()))
            .collect();
        v.sort_by_key(|r| r.start);
        let mut out: Vec<Range<usize>> = Vec::with_capacity(v.len());
        for r in v {
            match out.last_mut() {
                Some(last) if r.start <= last.end => last.end = last.end.max(r.end),
                _ => out.push(r),
            }
        }
        out
    }
}

#[derive(Debug, Clone, Default)]
pub struct SearchResult {
    /// Matching line numbers, ascending.
    pub lines: Vec<LineNo>,
}

impl SearchResult {
    /// Index into `lines` of the first match at or after `line`.
    pub fn position_at_or_after(&self, line: LineNo) -> usize {
        self.lines.partition_point(|&l| l < line)
    }

    pub fn contains(&self, line: LineNo) -> bool {
        self.lines.binary_search(&line).is_ok()
    }
}

pub fn search(source: &dyn LogSource, filter: &Filter, progress: &Progress) -> Result<SearchResult> {
    let lines = match (source.contiguous(), filter.prefilter()) {
        (Some(buf), Some(pre)) => {
            // A plain regex only needs re-checking for hits that reach past
            // the line content; a query is always fully evaluated.
            let always_verify = matches!(filter, Filter::Query(_));
            scan_buffer(source, buf, pre, always_verify, &|l| filter.matches(l), progress)?
        }
        _ => scan_lines(source, &|l| filter.matches(l), progress)?,
    };
    Ok(SearchResult { lines })
}

fn scan_buffer(
    source: &dyn LogSource,
    buf: &[u8],
    pre: &Regex,
    always_verify: bool,
    verify: &(dyn Fn(&[u8]) -> bool + Sync),
    progress: &Progress,
) -> Result<Vec<LineNo>> {
    let total_lines = source.line_count();
    progress.set_total(buf.len() as u64);

    let mut chunks: Vec<Range<LineNo>> = Vec::new();
    let mut first = 0;
    while first < total_lines {
        let target = source.line_start(first) + CHUNK_BYTES;
        let last =
            if target >= buf.len() as u64 { total_lines } else { (source.line_of_offset(target) + 1).min(total_lines) };
        chunks.push(first..last);
        first = last;
    }

    let parts: Vec<Vec<LineNo>> = chunks
        .into_par_iter()
        .map(|lines| {
            if progress.is_cancelled() {
                return Err(crate::Error::Cancelled);
            }
            let start = source.line_start(lines.start) as usize;
            let end = source.line_start(lines.end) as usize;
            // Haystack ends at the chunk end but starts at 0 so `^` and
            // word boundaries see the real left context.
            let hay = &buf[..end];
            let mut out = Vec::new();
            let mut pos = start;
            while pos < end {
                let Some(m) = pre.find_at(hay, pos) else { break };
                let line = source.line_of_offset(m.start() as u64);
                let content = source.line_bytes(line).unwrap_or_default();
                let content_end = source.line_start(line) as usize + content.len();
                // `a\s+b` or `\n` can reach into the terminator or next line;
                // such a hit only counts if the line matches on its own.
                let ok = if always_verify || m.end() > content_end { verify(content) } else { true };
                if ok {
                    out.push(line);
                }
                pos = source.line_start(line + 1) as usize;
            }
            progress.add((end - start) as u64);
            Ok(out)
        })
        .collect::<Result<_>>()?;

    Ok(parts.concat())
}

fn scan_lines(
    source: &dyn LogSource,
    pred: &(dyn Fn(&[u8]) -> bool + Sync),
    progress: &Progress,
) -> Result<Vec<LineNo>> {
    let n = source.line_count();
    progress.set_total(n);
    let tasks: Vec<Range<LineNo>> =
        (0..n.div_ceil(LINES_PER_TASK)).map(|t| t * LINES_PER_TASK..((t + 1) * LINES_PER_TASK).min(n)).collect();

    let parts: Vec<Vec<LineNo>> = tasks
        .into_par_iter()
        .map(|range| {
            if progress.is_cancelled() {
                return Err(crate::Error::Cancelled);
            }
            let len = range.end - range.start;
            let v = range.filter(|&l| source.line_bytes(l).is_some_and(pred)).collect();
            progress.add(len);
            Ok(v)
        })
        .collect::<Result<_>>()?;
    Ok(parts.concat())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::TextFile;

    fn regex_filter(pat: &str, opts: MatcherOptions) -> Filter {
        Filter::Regex(Matcher::new(pat, opts).unwrap())
    }

    /// Run through the engine and cross-check against naive per-line eval.
    fn run_filter(text: &str, f: &Filter) -> Vec<LineNo> {
        let src = TextFile::from_bytes("t", text.as_bytes().to_vec());
        let fast = search(&src, f, &Progress::new()).unwrap().lines;
        let slow = scan_lines(&src, &|l| f.matches(l), &Progress::new()).unwrap();
        assert_eq!(fast, slow, "fast and slow paths disagree");
        fast
    }

    fn run(text: &str, pat: &str, opts: MatcherOptions) -> Vec<LineNo> {
        run_filter(text, &regex_filter(pat, opts))
    }

    const LOG: &str = "INFO start\nWARN disk 91%\nERROR io failed\ninfo again\r\nERROR net down\n";

    #[test]
    fn simple_regex() {
        assert_eq!(run(LOG, "ERROR", Default::default()), [2, 4]);
        assert_eq!(run(LOG, r"\d+%", Default::default()), [1]);
    }

    #[test]
    fn anchors_respect_lines_and_crlf() {
        assert_eq!(run(LOG, "^info again$", Default::default()), [3]);
        assert_eq!(run(LOG, "down$", Default::default()), [4]);
    }

    #[test]
    fn case_insensitive_and_fixed() {
        let ci = MatcherOptions { case_insensitive: true, ..Default::default() };
        assert_eq!(run(LOG, "info", ci), [0, 3]);
        let fixed = MatcherOptions { fixed_string: true, ..Default::default() };
        assert_eq!(run("a.b\naxb\n", "a.b", fixed), [0]);
    }

    #[test]
    fn invert() {
        let inv = MatcherOptions { invert: true, ..Default::default() };
        assert_eq!(run(LOG, "ERROR", inv), [0, 1, 3]);
    }

    #[test]
    fn no_cross_line_false_positives() {
        assert_eq!(run("foo\nbar\nfoo bar\n", r"foo\s+bar", Default::default()), [2]);
        assert!(run("a\r\nb\n", r"\n", Default::default()).is_empty());
        assert!(run("a\r\nb\n", r"\r", Default::default()).is_empty());
    }

    #[test]
    fn empty_match_pattern_hits_every_line() {
        assert_eq!(run("a\nb\n\n", "x*", Default::default()), [0, 1, 2]);
    }

    #[test]
    fn dsl_queries_use_both_paths() {
        let log = "[INFO] SSL ok\n[ERROR] SSL bad\n[ERROR] disk\n[ERROR] SSL healthcheck\n[FATAL] CERT\n";
        let q = |s| Filter::Query(Query::compile(s).unwrap());
        // prefilter path
        assert_eq!(run_filter(log, &q("level >= ERROR AND raw ~ /SSL|CERT/ AND NOT raw ~ /healthcheck/")), [1, 4]);
        // per-line path
        assert_eq!(run_filter(log, &q("level >= ERROR")), [1, 2, 3, 4]);
        assert_eq!(run_filter(log, &q("level == INFO OR /disk/")), [0, 2]);
    }

    #[test]
    fn many_chunks() {
        let mut s = String::new();
        for i in 0..200_000 {
            s.push_str(&format!("{i:08} {} payload payload payload\n", if i % 997 == 0 { "ERROR" } else { "INFO" }));
        }
        let hits = run(&s, "ERROR", Default::default());
        assert_eq!(hits.len(), 200_000_usize.div_ceil(997));
        assert!(hits.iter().all(|l| l % 997 == 0));
    }

    #[test]
    fn highlight_spans_merge() {
        let f = Filter::Query(Query::compile("/ab/ AND /bc/").unwrap());
        assert_eq!(f.highlighter().spans(b"xabcx ab"), [1..4, 6..8]);
        assert!(
            regex_filter("x", MatcherOptions { invert: true, ..Default::default() })
                .highlighter()
                .spans(b"x")
                .is_empty()
        );
    }
}
