//! Search inside a filter result: which of the filter's lines match the
//! search queries, and which query matched each of them.
//!
//! Search never looks outside the filter's lines (`doc/search-manager.md`).
//! A hit is a line that matches at least one *include* query and no
//! *exclude* query (without a filter: any line of the file); every include query that matches it is reported, so
//! the UI can colour hits per query. With no include queries every line
//! that no exclude query matches is a hit.
//!
//! Two paths, chosen per search:
//!
//! * **scan** — contiguous sources, every include query has a prefilter
//!   regex and the filter keeps a sizeable share of the file: each include
//!   query runs the buffer scan of [`search`](super::search::search) over
//!   the whole file and the hits are intersected with the filter's lines.
//! * **per line** — everything else: each of the filter's lines is
//!   evaluated, in parallel chunks.

use super::search::{LINES_PER_TASK, has_fast_path, scan, work};
use crate::event::Event;
use crate::{Filter, LineNo, LogSource, Progress, Result};
use rayon::prelude::*;

/// Search queries: hits match any include and no exclude.
#[derive(Debug, Clone, Default)]
pub struct SearchSet {
    pub include: Vec<Filter>,
    pub exclude: Vec<Filter>,
}

/// Lines of a search inside a filter result, with the include queries
/// each one matched.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SearchHits {
    /// Ascending, a subset of the filter's lines.
    pub lines: Vec<LineNo>,
    /// `words` bit words per line: bit `i` = include query `i` matched.
    masks: Vec<u64>,
    words: usize,
}

impl SearchHits {
    fn new(queries: usize) -> Self {
        Self { lines: Vec::new(), masks: Vec::new(), words: queries.div_ceil(64).max(1) }
    }

    fn push(&mut self, line: LineNo, mask: &[u64]) {
        self.lines.push(line);
        self.masks.extend_from_slice(mask);
    }

    pub fn len(&self) -> usize {
        self.lines.len()
    }

    pub fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }

    /// Indices of the include queries that matched hit `i`, ascending.
    pub fn matched(&self, i: usize) -> impl Iterator<Item = usize> + '_ {
        let mask = &self.masks[i * self.words..(i + 1) * self.words];
        mask.iter()
            .enumerate()
            .flat_map(|(w, &bits)| (0..64).filter(move |b| bits >> b & 1 == 1).map(move |b| w * 64 + b))
    }

    /// Hits per include query (`queries` = number of include queries).
    pub fn counts(&self, queries: usize) -> Vec<usize> {
        let mut v = vec![0; queries];
        for i in 0..self.len() {
            self.matched(i).for_each(|q| v[q] += 1);
        }
        v
    }

    /// Index into `lines` of the first hit at or after `line`.
    pub fn position_at_or_after(&self, line: LineNo) -> usize {
        self.lines.partition_point(|&l| l < line)
    }
}

/// Which path to take; tests force each one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) enum Path {
    Auto,
    Scan,
    PerLine,
}

/// The filter keeps at least 1/`SCAN_SHARE` of the lines: scanning the
/// whole buffer beats evaluating each of them.
const SCAN_SHARE: u64 = 16;

/// Search `set` inside `within` (the filter's lines, ascending), or in
/// every line when there is no filter (`None`).
pub fn search_within(
    source: &dyn LogSource,
    within: Option<&[LineNo]>,
    set: &SearchSet,
    progress: &Progress,
) -> Result<SearchHits> {
    search_within_path(source, within, set, progress, Path::Auto)
}

/// The lines a search runs over.
#[derive(Clone, Copy)]
enum Within<'a> {
    All(LineNo),
    Lines(&'a [LineNo]),
}

impl Within<'_> {
    fn len(&self) -> u64 {
        match self {
            Within::All(n) => *n,
            Within::Lines(v) => v.len() as u64,
        }
    }

    /// The `i`-th line.
    fn line(&self, i: u64) -> LineNo {
        match self {
            Within::All(_) => i,
            Within::Lines(v) => v[i as usize],
        }
    }
}

pub(crate) fn search_within_path(
    source: &dyn LogSource,
    within: Option<&[LineNo]>,
    set: &SearchSet,
    progress: &Progress,
    path: Path,
) -> Result<SearchHits> {
    let within = within.map_or(Within::All(source.line_count()), Within::Lines);
    let bind = |fs: &[Filter]| fs.iter().map(|f| f.bind(source.schema())).collect::<std::result::Result<Vec<_>, _>>();
    let set = SearchSet { include: bind(&set.include)?, exclude: bind(&set.exclude)? };
    let can_scan = !set.include.is_empty() && set.include.iter().all(|f| has_fast_path(source, f));
    let scan_it = match path {
        Path::Auto => can_scan && within.len() * SCAN_SHARE >= source.line_count(),
        Path::Scan => can_scan,
        Path::PerLine => false,
    };
    if scan_it { by_scan(source, within, &set, progress) } else { per_line(source, within, &set, progress) }
}

fn by_scan(source: &dyn LogSource, within: Within<'_>, set: &SearchSet, progress: &Progress) -> Result<SearchHits> {
    progress.set_total(set.include.iter().map(|f| work(source, f)).sum::<u64>() + within.len());
    // (line, include index) of every include hit inside the filter.
    let mut pairs: Vec<(LineNo, usize)> = Vec::new();
    for (q, f) in set.include.iter().enumerate() {
        let lines = scan(source, f, progress)?;
        match within {
            Within::All(_) => pairs.extend(lines.into_iter().map(|l| (l, q))),
            Within::Lines(w) => pairs.extend(intersect(&lines, w).map(|l| (l, q))),
        }
    }
    pairs.sort_unstable();
    let mut hits = SearchHits::new(set.include.len());
    let mut mask = vec![0u64; hits.words];
    for (k, &(line, q)) in pairs.iter().enumerate() {
        mask[q / 64] |= 1 << (q % 64);
        if pairs.get(k + 1).is_none_or(|&(next, _)| next != line) {
            hits.push(line, &mask);
            mask.fill(0);
        }
    }
    progress.add(within.len() - hits.len() as u64);
    if set.exclude.is_empty() {
        progress.add(hits.len() as u64);
        return Ok(hits);
    }
    // Drop excluded hits, evaluating only the hit lines.
    let keep: Vec<bool> = hits
        .lines
        .par_chunks(LINES_PER_TASK as usize)
        .map(|chunk| {
            if progress.is_cancelled() {
                return Err(crate::Error::Cancelled);
            }
            let v: Vec<bool> = chunk
                .iter()
                .map(|&l| {
                    let mut ev = Event::from_source(source, l);
                    !set.exclude.iter().any(|f| f.matches(&mut ev))
                })
                .collect();
            progress.add(chunk.len() as u64);
            Ok(v)
        })
        .collect::<Result<Vec<_>>>()?
        .concat();
    let mut out = SearchHits::new(set.include.len());
    for (i, _) in keep.iter().enumerate().filter(|(_, k)| **k) {
        out.push(hits.lines[i], &hits.masks[i * hits.words..(i + 1) * hits.words]);
    }
    Ok(out)
}

/// Lines of `a` that are also in `b` (both ascending).
fn intersect<'a>(a: &'a [LineNo], b: &'a [LineNo]) -> impl Iterator<Item = LineNo> + 'a {
    let mut j = 0;
    a.iter().copied().filter(move |&l| {
        while b.get(j).is_some_and(|&x| x < l) {
            j += 1;
        }
        b.get(j) == Some(&l)
    })
}

fn per_line(source: &dyn LogSource, within: Within<'_>, set: &SearchSet, progress: &Progress) -> Result<SearchHits> {
    let n = within.len();
    progress.set_total(n);
    let words = SearchHits::new(set.include.len()).words;
    let tasks: Vec<std::ops::Range<u64>> =
        (0..n.div_ceil(LINES_PER_TASK)).map(|t| t * LINES_PER_TASK..((t + 1) * LINES_PER_TASK).min(n)).collect();
    let parts: Vec<SearchHits> = tasks
        .into_par_iter()
        .map(|range| {
            if progress.is_cancelled() {
                return Err(crate::Error::Cancelled);
            }
            let mut hits = SearchHits::new(set.include.len());
            let mut mask = vec![0u64; words];
            let len = range.end - range.start;
            for i in range {
                let l = within.line(i);
                let mut ev = Event::from_source(source, l);
                if let Some(m) = eval(&mut ev, set, &mut mask) {
                    hits.push(l, m);
                }
            }
            progress.add(len);
            Ok(hits)
        })
        .collect::<Result<_>>()?;
    let mut out = SearchHits::new(set.include.len());
    for p in parts {
        out.lines.extend(p.lines);
        out.masks.extend(p.masks);
    }
    Ok(out)
}

/// The include mask of a hit, or `None` if the line isn't one.
fn eval<'m>(ev: &mut Event<'_>, set: &SearchSet, mask: &'m mut [u64]) -> Option<&'m [u64]> {
    mask.fill(0);
    let mut any = set.include.is_empty();
    for (q, f) in set.include.iter().enumerate() {
        if f.matches(ev) {
            mask[q / 64] |= 1 << (q % 64);
            any = true;
        }
    }
    (any && !set.exclude.iter().any(|f| f.matches(ev))).then_some(mask)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::search::tests::run_on;
    use crate::input::mem::{MemSource, Row};
    use crate::{Level, Matcher, MatcherOptions, Query, TextFile};

    /// Naive reference: render each filter line and evaluate it alone,
    /// sequentially.
    fn naive(source: &dyn LogSource, within: &[LineNo], set: &SearchSet) -> SearchHits {
        let bind = |fs: &[Filter]| fs.iter().map(|f| f.bind(source.schema()).unwrap()).collect();
        let set = SearchSet { include: bind(&set.include), exclude: bind(&set.exclude) };
        let mut hits = SearchHits::new(set.include.len());
        let mut mask = vec![0u64; hits.words];
        for &l in within {
            let raw = source.line_bytes(l).unwrap();
            if let Some(m) = eval(&mut Event::with_raw(source, l, &raw), &set, &mut mask) {
                hits.push(l, m);
            }
        }
        hits
    }

    /// Filter, then search inside its result on every path; all must agree
    /// with the naive evaluation. Returns `(line, matched include queries)`.
    fn run(source: &dyn LogSource, filter: &str, include: &[&str], exclude: &[&str]) -> Vec<(LineNo, Vec<usize>)> {
        let q = |s: &&str| Filter::Query(Query::compile(s).unwrap_or_else(|e| panic!("{s}: {e}")));
        let within = run_on(source, &q(&filter));
        let set = SearchSet { include: include.iter().map(q).collect(), exclude: exclude.iter().map(q).collect() };
        let want = naive(source, &within, &set);
        for path in [Path::Auto, Path::Scan, Path::PerLine] {
            let got = search_within_path(source, Some(&within), &set, &Progress::new(), path).unwrap();
            assert_eq!(got, want, "{path:?} disagrees with naive");
            // Without a filter: every line.
            let all: Vec<LineNo> = (0..source.line_count()).collect();
            let got = search_within_path(source, None, &set, &Progress::new(), path).unwrap();
            assert_eq!(got, naive(source, &all, &set), "{path:?} without a filter disagrees with naive");
        }
        (0..want.len()).map(|i| (want.lines[i], want.matched(i).collect())).collect()
    }

    const LOG: &str = "\
[INFO] cam1 frame drop\n\
[ERROR] cam1 frame drop\n\
[ERROR] cam2 frame drop\n\
[WARN] cam2 timeout\n\
[ERROR] disk full\n\
[INFO] cam1 timeout\n\
[ERROR] cam1 timeout cam2\n";

    fn text() -> TextFile {
        TextFile::from_bytes("t", LOG.as_bytes().to_vec())
    }

    #[test]
    fn only_inside_the_filter_and_per_query() {
        let hits = run(&text(), "level >= WARN", &["/cam1/", "/timeout/"], &[]);
        assert_eq!(hits, [(1, vec![0]), (3, vec![1]), (6, vec![0, 1])]);
        // Lines 0 and 5 match but are outside the filter.
        assert_eq!(run(&text(), "/cam/", &["/drop/"], &["/cam2/"]), [(0, vec![0]), (1, vec![0])]);
        // No include: everything the excludes leave.
        assert_eq!(run(&text(), "level >= ERROR", &[], &["/cam/"]), [(4, vec![])]);
        // Per-line include queries (no prefilter) and an empty filter.
        assert_eq!(run(&text(), "/cam2/", &["level == ERROR OR /drop/"], &[]), [(2, vec![0]), (6, vec![0])]);
        assert!(run(&text(), "/nothing/", &["/cam/"], &[]).is_empty());
    }

    #[test]
    fn many_queries_and_lines() {
        // More than 64 queries need a second mask word.
        let mut s = String::new();
        for i in 0..150_000u32 {
            s.push_str(&format!("{i:06} {} k{} payload\n", if i % 3 == 0 { "ERROR" } else { "INFO" }, i % 70));
        }
        let src = TextFile::from_bytes("big", s.into_bytes());
        let pats: Vec<String> = (0..70).map(|k| format!(r"/ k{k} /")).collect();
        let include: Vec<&str> = pats.iter().map(String::as_str).collect();
        let hits = run(&src, "/ERROR/", &include, &["/^0000/"]);
        assert_eq!(hits.len(), 50_000 - 34);
        assert!(hits.iter().all(|(l, m)| *m == [(*l % 70) as usize]));
    }

    #[test]
    fn source_fields_and_regex_filters() {
        let row = |text, apid, mcnt, level| Row { text, apid, mcnt, level };
        let src = MemSource::new(vec![
            row("nav slow", Some("NAV"), Some(1), Some(Level::Warn)),
            row("nav ok", Some("NAV"), Some(2), Some(Level::Info)),
            row("stor slow", Some("STOR"), Some(3), Some(Level::Error)),
            row("nav slow again", Some("NAV"), Some(4), Some(Level::Error)),
        ]);
        assert_eq!(run(&src, "apid == NAV", &["/slow/", "level >= ERROR"], &["mcnt == 1"]), [(3, vec![0, 1])]);
        // Plain regex filters work as search queries too.
        let m = |p: &str| Filter::Regex(Matcher::new(p, MatcherOptions::default()).unwrap());
        let set = SearchSet { include: vec![m("slow"), m("ok")], exclude: vec![] };
        for path in [Path::Scan, Path::PerLine] {
            let hits = search_within_path(&text(), Some(&[0, 3, 5]), &set, &Progress::new(), path).unwrap();
            assert!(hits.is_empty());
            let hits = search_within_path(&src, Some(&[1, 2]), &set, &Progress::new(), path).unwrap();
            assert_eq!(hits.lines, [1, 2]);
            assert_eq!(hits.counts(2), [1, 1]);
        }
    }

    #[test]
    fn errors_and_cancel() {
        let q = |s: &str| Filter::Query(Query::compile(s).unwrap());
        let set = SearchSet { include: vec![q("apid == NAV")], exclude: vec![] };
        assert!(matches!(search_within(&text(), Some(&[0]), &set, &Progress::new()), Err(crate::Error::Query(_))));
        let p = Progress::new();
        p.cancel();
        let set = SearchSet { include: vec![q("/cam/")], exclude: vec![] };
        for path in [Path::Scan, Path::PerLine] {
            let r = search_within_path(&text(), Some(&[0, 1]), &set, &p, path);
            assert!(matches!(r, Err(crate::Error::Cancelled)), "{path:?}");
        }
    }
}
