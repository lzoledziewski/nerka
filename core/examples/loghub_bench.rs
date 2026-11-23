//! Score `nerka_core::pattern` against labelled logs: the LogHub 2k samples
//! (<https://github.com/logpai/loghub-2.0>, `2k_dataset/<System>/
//! <System>_2k.log_structured_corrected.csv`; research use, not in this
//! repository). Each CSV's `Content` column is grouped, line by line, and
//! the groups compared with its `EventId`s:
//!
//! - GA (grouping accuracy, Zhu et al. 2019): the share of lines whose
//!   pattern holds exactly the lines of their template;
//! - FGA (Jiang et al. 2024): F1 of patterns that are exactly a template's
//!   lines, over the patterns found and the templates.
//!
//! ```text
//! cargo run --release -p nerka-core --example loghub_bench -- DIR [--config FILE] [--set KEY=VALUE]... [--rule RULE]... [--no-merge] [--similarity S] [--show SYSTEM]
//! ```
//!
//! `--config FILE` (first) is a pattern configuration (`docs/user/cli.md`),
//! `--set` and `--rule` add to it, so a setting's effect is measured.
//!
//! `--show SYSTEM` prints that system's patterns which aren't a template
//! exactly, with the templates their lines have.

use nerka_core::pattern::{self, PatternConfig, PatternOptions};
use nerka_core::{Progress, TextFile};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Instant;

fn main() {
    let mut args = std::env::args().skip(1);
    let mut dir = None;
    let mut options = PatternOptions { lines: true, ..Default::default() };
    let mut show = None;
    while let Some(a) = args.next() {
        match a.as_str() {
            "--no-merge" => options.config.settings.merge = false,
            "--similarity" => options
                .config
                .set(&format!("similarity {}", args.next().expect("--similarity S")))
                .expect("--similarity S"),
            "--config" => {
                let path = args.next().expect("--config FILE");
                let text = std::fs::read_to_string(&path).expect("--config FILE");
                // First: it replaces what came before.
                options.config = PatternConfig::parse(&text).unwrap_or_else(|e| panic!("{path}: {e}"));
            }
            "--set" => options.config.set(&args.next().expect("--set KEY=VALUE")).expect("--set KEY=VALUE"),
            "--rule" => options.config.add_rule(&args.next().expect("--rule RULE")).expect("--rule RULE"),
            "--show" => show = args.next(),
            _ => dir = Some(PathBuf::from(a)),
        }
    }
    let dir = dir.expect(
        "usage: loghub_bench DIR [--config FILE] [--set KEY=VALUE]... [--rule RULE]... [--no-merge] [--similarity S] [--show SYSTEM]",
    );
    let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)
        .expect("DIR")
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|e| e == "csv"))
        .collect();
    files.sort();
    println!(
        "{:<12} {:>6} {:>9} {:>9} {:>7} {:>7} {:>7} {:>8}",
        "system", "lines", "templates", "patterns", "GA", "FGA", "WGA", "ms"
    );
    let (mut sum_ga, mut sum_fga, mut sum_wga, mut n) = (0.0, 0.0, 0.0, 0);
    for f in &files {
        let name = f.file_name().unwrap().to_string_lossy();
        let system = name.split('_').next().unwrap_or(&name).to_string();
        let (contents, truth) = read(f);
        let source = TextFile::from_bytes(&system, contents.join("\n").into_bytes());
        let t = Instant::now();
        let ps = pattern::detect(&source, None, &options, &Progress::new()).expect("detect");
        let ms = t.elapsed().as_secs_f64() * 1000.0;
        // Each line's pattern.
        let mut found = vec![usize::MAX; truth.len()];
        for (k, p) in ps.patterns.iter().enumerate() {
            for &l in &p.lines[0] {
                found[l as usize] = k;
            }
        }
        assert!(found.iter().all(|&k| k != usize::MAX), "{system}: a line in no pattern");
        let (ga, fga, wga) = score(&found, &truth);
        println!(
            "{system:<12} {:>6} {:>9} {:>9} {ga:>7.3} {fga:>7.3} {wga:>7.3} {ms:>8.1}",
            truth.len(),
            truth.iter().collect::<std::collections::HashSet<_>>().len(),
            ps.patterns.len()
        );
        sum_ga += ga;
        sum_fga += fga;
        sum_wga += wga;
        n += 1;
        if show.as_deref().is_some_and(|s| s.eq_ignore_ascii_case(&system)) {
            explain(&ps, &truth);
        }
    }
    if n > 0 {
        println!(
            "{:<12} {:>6} {:>9} {:>9} {:>7.3} {:>7.3} {:>7.3}",
            "average",
            "",
            "",
            "",
            sum_ga / n as f64,
            sum_fga / n as f64,
            sum_wga / n as f64
        );
    }
}

/// GA, FGA and WGA of the grouping `found` (a pattern per line) against
/// `truth` (a template per line). WGA is GA that also counts a pattern of
/// whole templates (all the lines of each template it holds, as a state
/// `VM {Started|Stopped}` holds two) as right: what GA loses to merging
/// templates whole, whether states or wrong merges, against splitting or
/// mixing them.
fn score(found: &[usize], truth: &[String]) -> (f64, f64, f64) {
    let groups = |of: &mut dyn Iterator<Item = String>| {
        let mut g: HashMap<String, Vec<usize>> = HashMap::new();
        for (i, k) in of.enumerate() {
            g.entry(k).or_default().push(i);
        }
        g
    };
    let pred = groups(&mut found.iter().map(|k| k.to_string()));
    let real = groups(&mut truth.iter().cloned());
    let mut right_lines = 0;
    let mut right_groups = 0;
    let mut whole_lines = 0;
    for lines in pred.values() {
        if real[&truth[lines[0]]] == *lines {
            right_lines += lines.len();
            right_groups += 1;
        }
        let mut of: HashMap<&str, usize> = HashMap::new();
        for &l in lines {
            *of.entry(&truth[l]).or_default() += 1;
        }
        if of.iter().all(|(t, &c)| real[*t].len() == c) {
            whole_lines += lines.len();
        }
    }
    let ga = right_lines as f64 / truth.len() as f64;
    let (p, r) = (right_groups as f64 / pred.len() as f64, right_groups as f64 / real.len() as f64);
    let fga = if p + r == 0.0 { 0.0 } else { 2.0 * p * r / (p + r) };
    (ga, fga, whole_lines as f64 / truth.len() as f64)
}

/// The patterns that aren't exactly one template's lines.
fn explain(ps: &pattern::Patterns, truth: &[String]) {
    let mut sizes: HashMap<&str, usize> = HashMap::new();
    for t in truth {
        *sizes.entry(t).or_default() += 1;
    }
    for (k, p) in ps.patterns.iter().enumerate() {
        let mut of: HashMap<&str, usize> = HashMap::new();
        for &l in &p.lines[0] {
            *of.entry(&truth[l as usize]).or_default() += 1;
        }
        let exact = of.len() == 1 && of.iter().all(|(t, &c)| sizes[t] == c);
        if exact {
            continue;
        }
        let mut of: Vec<(&str, usize)> = of.into_iter().collect();
        of.sort_by_key(|a| std::cmp::Reverse(a.1));
        let parts: Vec<String> = of.iter().map(|(t, c)| format!("{t} {c}/{}", sizes[t])).collect();
        println!("  #{k} {:>4}  {}\n          templates: {}", p.count(), p.template(), parts.join(", "));
    }
}

/// The `Content` and `EventId` columns of a LogHub structured CSV.
fn read(path: &Path) -> (Vec<String>, Vec<String>) {
    let text = std::fs::read_to_string(path).expect("read CSV");
    let mut rows = csv(&text).into_iter();
    let header = rows.next().expect("header");
    let col = |name: &str| header.iter().position(|h| h == name).unwrap_or_else(|| panic!("no column {name}"));
    let (c, e) = (col("Content"), col("EventId"));
    rows.filter(|r| r.len() > c.max(e)).map(|r| (r[c].replace(['\n', '\r'], " "), r[e].clone())).unzip()
}

/// RFC 4180 rows: `,` between fields, `"` around fields holding them (`""`:
/// a `"`).
fn csv(text: &str) -> Vec<Vec<String>> {
    let mut rows = Vec::new();
    let (mut row, mut field) = (Vec::new(), String::new());
    let mut chars = text.chars().peekable();
    let mut quoted = false;
    while let Some(ch) = chars.next() {
        match (quoted, ch) {
            (true, '"') if chars.peek() == Some(&'"') => {
                chars.next();
                field.push('"');
            }
            (true, '"') => quoted = false,
            (true, c) => field.push(c),
            (false, '"') => quoted = true,
            (false, ',') => row.push(std::mem::take(&mut field)),
            (false, '\r') => {}
            (false, '\n') => {
                row.push(std::mem::take(&mut field));
                rows.push(std::mem::take(&mut row));
            }
            (false, c) => field.push(c),
        }
    }
    if !field.is_empty() || !row.is_empty() {
        row.push(field);
        rows.push(row);
    }
    rows
}
