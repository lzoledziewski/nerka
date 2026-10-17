//! `nerka` — command line front end to the Nerka engine.
//!
//!     nerka open   app.log
//!     nerka search -i 'error|timeout' app.log
//!     nerka filter 'level >= ERROR AND raw ~ /SSL|CERT/' app.log
//!     nerka lines  app.log 1000 1050
//!     nerka marks  app.log

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use nerka_core::{Bookmarks, Filter, LogSource, Matcher, MatcherOptions, Progress, Query, TextFile, search};
use std::io::{BufWriter, Write};
use std::path::PathBuf;
use std::time::Instant;

#[derive(Parser)]
#[command(name = "nerka", version, about = "Fast regex log analysis")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Index a file and print statistics (lines, size, throughput).
    #[command(alias = "info")]
    Open { file: PathBuf },
    /// Print lines matching a regex (like grep -n).
    Search {
        pattern: String,
        file: PathBuf,
        /// Case-insensitive.
        #[arg(short = 'i', long)]
        ignore_case: bool,
        /// Pattern is a literal string.
        #[arg(short = 'F', long)]
        fixed: bool,
        /// Select non-matching lines.
        #[arg(short = 'v', long)]
        invert: bool,
        #[command(flatten)]
        out: OutputArgs,
    },
    /// Print lines matching an NFD query, e.g. 'level >= ERROR AND raw ~ /SSL/'.
    Filter {
        query: String,
        file: PathBuf,
        #[command(flatten)]
        out: OutputArgs,
    },
    /// List bookmarks saved for a file (by the UI).
    Marks { file: PathBuf },
    /// Print a 1-based inclusive range of lines.
    Lines { file: PathBuf, from: u64, to: u64 },
}

#[derive(clap::Args)]
struct OutputArgs {
    /// Only print the number of matching lines.
    #[arg(short = 'c', long)]
    count: bool,
    /// Do not prefix lines with their number.
    #[arg(short = 'N', long)]
    no_line_numbers: bool,
    /// Print timing to stderr.
    #[arg(long)]
    stats: bool,
}

fn run_filter(file: &PathBuf, filter: &Filter, o: &OutputArgs, out: &mut impl Write) -> Result<()> {
    let (f, t_index) = open(file)?;
    let t = Instant::now();
    let res = search(&f, filter, &Progress::new())?;
    let t_search = t.elapsed();
    if o.count {
        writeln!(out, "{}", res.lines.len())?;
    } else {
        for &l in &res.lines {
            if !o.no_line_numbers {
                write!(out, "{}:", l + 1)?;
            }
            out.write_all(f.line_bytes(l).unwrap_or_default())?;
            out.write_all(b"\n")?;
        }
    }
    if o.stats {
        out.flush()?;
        eprintln!(
            "{} matches in {} lines | index {t_index:.2?} | search {t_search:.2?} ({:.0} MiB/s)",
            res.lines.len(),
            f.line_count(),
            mib_per_s(f.size_bytes(), t_search)
        );
    }
    Ok(())
}

fn open(path: &PathBuf) -> Result<(TextFile, std::time::Duration)> {
    let t = Instant::now();
    let f = TextFile::open(path, &Progress::new()).with_context(|| format!("opening {}", path.display()))?;
    Ok((f, t.elapsed()))
}

fn mib_per_s(bytes: u64, d: std::time::Duration) -> f64 {
    bytes as f64 / 1024.0 / 1024.0 / d.as_secs_f64().max(1e-9)
}

fn main() -> std::process::ExitCode {
    match run() {
        Ok(()) => std::process::ExitCode::SUCCESS,
        // `nerka search … | head` closes the pipe early: not an error.
        Err(e) if e.downcast_ref::<std::io::Error>().is_some_and(|e| e.kind() == std::io::ErrorKind::BrokenPipe) => {
            std::process::ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("nerka: {e:#}");
            std::process::ExitCode::FAILURE
        }
    }
}

fn run() -> Result<()> {
    let cli = Cli::parse();
    let stdout = std::io::stdout().lock();
    let mut out = BufWriter::with_capacity(1 << 16, stdout);

    match cli.cmd {
        Cmd::Open { file } => {
            let (f, took) = open(&file)?;
            writeln!(out, "file:        {}", f.name())?;
            writeln!(out, "size:        {} bytes", f.size_bytes())?;
            writeln!(out, "lines:       {}", f.line_count())?;
            writeln!(out, "index mem:   {} KiB", f.index().memory_bytes() / 1024)?;
            writeln!(out, "index time:  {took:.2?} ({:.0} MiB/s)", mib_per_s(f.size_bytes(), took))?;
        }
        Cmd::Search { pattern, file, ignore_case, fixed, invert, out: o } => {
            let m =
                Matcher::new(&pattern, MatcherOptions { case_insensitive: ignore_case, fixed_string: fixed, invert })?;
            run_filter(&file, &Filter::Regex(m), &o, &mut out)?;
        }
        Cmd::Filter { query, file, out: o } => {
            let q = Query::compile(&query).map_err(|e| {
                anyhow::anyhow!("{e}\n  {query}\n  {}^", " ".repeat(query[..e.pos.min(query.len())].chars().count()))
            })?;
            run_filter(&file, &Filter::Query(q), &o, &mut out)?;
        }
        Cmd::Marks { file } => {
            let marks = Bookmarks::load_for(&file)?;
            let f = TextFile::open(&file, &Progress::new())?;
            for (l, note) in marks.iter() {
                let text = f.line_text(l).unwrap_or_default();
                if note.is_empty() {
                    writeln!(out, "{}: {text}", l + 1)?;
                } else {
                    writeln!(out, "{}: {text}\n    # {note}", l + 1)?;
                }
            }
        }
        Cmd::Lines { file, from, to } => {
            let (f, _) = open(&file)?;
            let last = to.min(f.line_count());
            for l in from.max(1)..=last {
                out.write_all(f.line_bytes(l - 1).unwrap_or_default())?;
                out.write_all(b"\n")?;
            }
        }
    }
    out.flush()?;
    Ok(())
}
