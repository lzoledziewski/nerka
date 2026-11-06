//! `nerka` — command line front end to the Nerka engine.
//!
//!     nerka open   app.log          (plain text or DLT, picked by content)
//!     nerka search -i 'error|timeout' app.log
//!     nerka filter 'level >= ERROR AND raw ~ /SSL|CERT/' app.log
//!     nerka lines  app.log 1000 1050
//!     nerka marks  app.log
//!     nerka packs                                       (list loaded query packs)
//!     nerka expand -q vision.frame_drops -p cam=CAM1    (pack picks -> plain NFD)
//!     nerka search --in 'level >= WARN' timeout app.log (search inside a filter)
//!     nerka search --in 'ecu == SOCB' -l ota app.log     (per pack query)
//!     nerka --signal 'v=/v=(?<v>\d+)/' filter '@v > 100' app.log   (signals)
//!     nerka --fibex ecu1.xml filter 'apid == NAV' trace.dlt         (non-verbose DLT)

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use nerka_core::pack::{self, Expansion, Library, ParamValue, Params, Pick, Role, Selection};
use nerka_core::{
    Bookmarks, Filter, LogSource, Matcher, MatcherOptions, Progress, Query, SearchSet, SignalDef, Signals, search,
    search_within,
};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::time::Instant;

#[derive(Parser)]
#[command(name = "nerka", version, about = "Fast regex log analysis")]
struct Cli {
    /// Load query packs from a .nerka-pack file or a directory of them
    /// (repeatable), besides the per-user pack directory.
    #[arg(long, global = true, value_name = "DIR|FILE")]
    pack: Vec<PathBuf>,
    /// UDP destination port that carries DLT in captures (pcap, pcapng,
    /// MF4; repeatable). Default: 3490.
    #[arg(long = "dlt-port", global = true, value_name = "PORT")]
    dlt_port: Vec<u16>,
    /// UDP destination port that carries SOME/IP in captures (repeatable).
    /// Default: 30490 (service discovery); services use ports of their own.
    #[arg(long = "someip-port", global = true, value_name = "PORT")]
    someip_port: Vec<u16>,
    /// UDP destination port that carries syslog in captures (repeatable).
    /// Default: 514.
    #[arg(long = "syslog-port", global = true, value_name = "PORT")]
    syslog_port: Vec<u16>,
    /// Folder compressed MF4 data is unpacked into (default: the per-user
    /// cache folder); unpacked once, reused when the file is opened again.
    #[arg(long = "unpack-dir", global = true, value_name = "DIR")]
    unpack_dir: Option<PathBuf>,
    /// FIBEX file describing DLT non-verbose messages (repeatable): their
    /// payload is shown as text, and their APID / CTID / level come from it.
    #[arg(long, global = true, value_name = "FILE")]
    fibex: Vec<PathBuf>,
    /// Define a signal for `@name` in queries (repeatable):
    /// NAME[(GROUP)][:num|enum]=QUERY, e.g. 'speed=/speed=(?<speed>\d+)/'.
    #[arg(long, global = true, value_name = "SPEC")]
    signal: Vec<String>,
    /// Show times (DLT's time column, signals) in this computer's local
    /// time and read times without a zone as local; default: UTC.
    #[arg(long = "local-time", global = true)]
    local_time: bool,
    /// Read an MF4 start time marked as local time as UTC instead of in
    /// this computer's zone.
    #[arg(long = "mf4-utc", global = true)]
    mf4_utc: bool,
    /// Read text files as records: `start=/REGEX/` (a line matching it
    /// starts a record), `blank` (blank lines separate them), `sep=/REGEX/`
    /// (lines matching it separate them), `json` (keys are fields), `lines`,
    /// or `auto` (the default: JSON if the file looks like JSON, else lines).
    #[arg(long, global = true, value_name = "SPEC")]
    records: Option<String>,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Index a file and print statistics (lines, size, throughput).
    #[command(alias = "info")]
    Open { file: PathBuf },
    /// Print lines matching a regex (like grep -n). With --in, search only
    /// the lines of a filter. With pack picks (-q, -l, -x, -X) instead of a
    /// pattern, print which query matched each line.
    Search {
        #[arg(required = true, num_args = 1..=2, value_names = ["PATTERN", "FILE"])]
        args: Vec<String>,
        /// Only search the lines matching this NFD filter.
        #[arg(long = "in", value_name = "FILTER")]
        within: Option<String>,
        #[command(flatten)]
        picks: PickArgs,
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
    /// Expand pack picks into plain NFD and print it.
    Expand {
        #[command(flatten)]
        picks: PickArgs,
        /// Also print each picked query's part of the expansion.
        #[arg(long)]
        parts: bool,
    },
    /// List the loaded packs and their queries.
    Packs,
    /// List bookmarks saved for a file (by the UI).
    Marks { file: PathBuf },
    /// Print a 1-based inclusive range of lines.
    Lines { file: PathBuf, from: u64, to: u64 },
}

#[derive(clap::Args)]
struct OutputArgs {
    /// Only print the number of matching lines (and, for pack picks, the
    /// hits per query).
    #[arg(short = 'c', long)]
    count: bool,
    /// Do not prefix lines with their number.
    #[arg(short = 'N', long)]
    no_line_numbers: bool,
    /// Print timing to stderr.
    #[arg(long)]
    stats: bool,
}

/// Queries picked from the packs.
#[derive(clap::Args)]
struct PickArgs {
    /// Include a query (pack.query); repeatable.
    #[arg(short = 'q', long = "query", value_name = "ID")]
    queries: Vec<String>,
    /// Include every query with this label (or one below it).
    #[arg(short = 'l', long = "label")]
    labels: Vec<String>,
    /// Require a query (AND): kept lines must match it too.
    #[arg(short = 'r', long = "require", value_name = "ID")]
    require: Vec<String>,
    /// Require every query with this label (each one ANDed).
    #[arg(short = 'R', long = "require-label", value_name = "LABEL")]
    require_labels: Vec<String>,
    /// Exclude a query (AND NOT).
    #[arg(short = 'x', long = "exclude", value_name = "ID")]
    exclude: Vec<String>,
    /// Exclude every query with this label.
    #[arg(short = 'X', long = "exclude-label", value_name = "LABEL")]
    exclude_labels: Vec<String>,
    /// Parameter value, escaped where it is used: NAME=VALUE.
    #[arg(short = 'p', long = "param", value_name = "NAME=VALUE")]
    params: Vec<String>,
    /// Parameter value inserted as regex syntax inside regexes: NAME=VALUE.
    #[arg(long = "raw-param", value_name = "NAME=VALUE")]
    raw_params: Vec<String>,
}

impl PickArgs {
    fn is_empty(&self) -> bool {
        self.queries.is_empty()
            && self.labels.is_empty()
            && self.require.is_empty()
            && self.require_labels.is_empty()
            && self.exclude.is_empty()
            && self.exclude_labels.is_empty()
    }

    /// Queries come before labels, each in command-line order.
    fn expand(&self, lib: &Library) -> Result<Expansion> {
        let picks = |ids: &[String], labels: &[String]| -> Vec<Pick> {
            ids.iter().map(|q| Pick::Query(q.clone())).chain(labels.iter().map(|l| Pick::Label(l.clone()))).collect()
        };
        let sel = Selection {
            include: picks(&self.queries, &self.labels),
            require: picks(&self.require, &self.require_labels),
            exclude: picks(&self.exclude, &self.exclude_labels),
            signal: Vec::new(),
        };
        let mut params = Params::new();
        for (list, raw) in [(&self.params, false), (&self.raw_params, true)] {
            for kv in list {
                let (k, v) = kv.split_once('=').with_context(|| format!("expected NAME=VALUE, got '{kv}'"))?;
                params.insert(k.to_owned(), ParamValue { value: v.to_owned(), raw });
            }
        }
        Ok(pack::expand(&sel, lib, &params)?)
    }
}

/// The per-user pack directory (if it exists) plus every `--pack`.
fn library(extra: &[PathBuf]) -> Result<Library> {
    let mut lib = Library::new();
    for p in pack::default_dir().filter(|d| d.is_dir()).iter().chain(extra) {
        lib.load(p)?;
    }
    Ok(lib)
}

/// Compile an NFD query for a source, with a caret under the error.
fn compile(query: &str, source: &dyn LogSource) -> Result<Query> {
    // `--signal` definitions, and `@time` on files that know their time.
    let signals = SIGNALS.get_or_init(Default::default).for_source(source);
    Query::compile_in(query, source.schema(), &signals).map_err(|e| {
        anyhow::anyhow!("{e}\n  {query}\n  {}^", " ".repeat(query[..e.pos.min(query.len())].chars().count()))
    })
}

/// `bookmark` in a query: the bookmarks of the log (its `.nerka-marks`),
/// on the lines of `source` their anchors find.
fn with_marks(filter: Filter, file: &Path, source: &dyn LogSource) -> Result<Filter> {
    if !filter.uses_bookmarks() {
        return Ok(filter);
    }
    let marks =
        Bookmarks::load_for(file, source).with_context(|| format!("reading the bookmarks of {}", file.display()))?;
    Ok(filter.with_bookmarks(&std::sync::Arc::new(marks)))
}

/// `build` gets the opened source (its fields, its time), so query errors about
/// unknown fields point into the query like syntax errors do.
fn run_filter(
    file: &PathBuf,
    build: impl FnOnce(&dyn LogSource) -> Result<Filter>,
    o: &OutputArgs,
    out: &mut impl Write,
) -> Result<()> {
    let (f, t_index) = open(file)?;
    let filter = with_marks(build(&*f)?, file, &*f)?;
    let t = Instant::now();
    let res = search(&*f, &filter, &Progress::new())?;
    let t_search = t.elapsed();
    if o.count {
        writeln!(out, "{}", res.lines.len())?;
    } else {
        for &l in &res.lines {
            if !o.no_line_numbers {
                write!(out, "{}:", l + 1)?;
            }
            out.write_all(&f.line_bytes(l).unwrap_or_default())?;
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

/// Search inside the lines of `within` (every line without it). `names`
/// are the include queries' names, printed per hit; empty for a pattern.
fn run_search_within(
    file: &PathBuf,
    within: Option<&str>,
    set: SearchSet,
    names: &[String],
    o: &OutputArgs,
    out: &mut impl Write,
) -> Result<()> {
    let (f, t_index) = open(file)?;
    let filter = within.map(|q| compile(q, &*f).and_then(|q| with_marks(Filter::Query(q), file, &*f))).transpose()?;
    let set = SearchSet {
        include: set.include.into_iter().map(|q| with_marks(q, file, &*f)).collect::<Result<_>>()?,
        exclude: set.exclude.into_iter().map(|q| with_marks(q, file, &*f)).collect::<Result<_>>()?,
    };
    let t = Instant::now();
    let lines = match filter {
        Some(q) => Some(search(&*f, &q, &Progress::new())?.lines),
        None => None,
    };
    let t_filter = t.elapsed();
    let t = Instant::now();
    let hits = search_within(&*f, lines.as_deref(), &set, &Progress::new())?;
    let t_search = t.elapsed();
    if o.count {
        writeln!(out, "{}", hits.len())?;
        if !names.is_empty() {
            for (name, n) in names.iter().zip(hits.counts(names.len())) {
                writeln!(out, "{name}\t{n}")?;
            }
        }
    } else {
        for (i, &l) in hits.lines.iter().enumerate() {
            if !o.no_line_numbers {
                write!(out, "{}:", l + 1)?;
            }
            if !names.is_empty() {
                let ids: Vec<&str> = hits.matched(i).map(|q| names[q].as_str()).collect();
                write!(out, "[{}] ", ids.join(","))?;
            }
            out.write_all(&f.line_bytes(l).unwrap_or_default())?;
            out.write_all(b"\n")?;
        }
    }
    if o.stats {
        out.flush()?;
        eprintln!(
            "{} hits in {} filtered of {} lines | index {t_index:.2?} | filter {t_filter:.2?} | search {t_search:.2?}",
            hits.len(),
            lines.as_ref().map_or(f.line_count() as usize, Vec::len),
            f.line_count()
        );
    }
    Ok(())
}

/// The `--signal` definitions, set once in `main`.
static SIGNALS: std::sync::OnceLock<Signals> = std::sync::OnceLock::new();

/// How files are opened (`--dlt-port`, `--someip-port`, …), set once in `main`.
static OPEN_OPTIONS: std::sync::OnceLock<nerka_core::OpenOptions> = std::sync::OnceLock::new();

fn open(path: &PathBuf) -> Result<(Box<dyn LogSource>, std::time::Duration)> {
    let t = Instant::now();
    let options = OPEN_OPTIONS.get_or_init(Default::default);
    let f = nerka_core::open_with(path, options, &Progress::new())
        .with_context(|| format!("opening {}", path.display()))?;
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
    let mut options = nerka_core::OpenOptions::default();
    if !cli.dlt_port.is_empty() {
        options.capture.dlt_udp_ports = cli.dlt_port.clone();
    }
    if !cli.someip_port.is_empty() {
        options.capture.someip_udp_ports = cli.someip_port.clone();
    }
    if !cli.syslog_port.is_empty() {
        options.capture.syslog_udp_ports = cli.syslog_port.clone();
    }
    options.capture.unpack.dir = cli.unpack_dir.clone().or_else(nerka_core::cache_dir);
    options.capture.mf4_local_time = !cli.mf4_utc;
    if let Some(spec) = &cli.records {
        options.records = nerka_core::input::records::RecordSplit::parse_choice(spec)?;
    }
    nerka_core::time::set_local(cli.local_time);
    if !cli.fibex.is_empty() {
        options.fibex = Some(std::sync::Arc::new(nerka_core::Fibex::load(&cli.fibex)?));
    }
    OPEN_OPTIONS.set(options).expect("set once");
    let mut signals = Signals::default();
    for spec in &cli.signal {
        signals.set(SignalDef::parse_spec(spec).with_context(|| format!("--signal {spec}"))?);
    }
    SIGNALS.set(signals).expect("set once");
    let stdout = std::io::stdout().lock();
    let mut out = BufWriter::with_capacity(1 << 16, stdout);

    match cli.cmd {
        Cmd::Open { file } => {
            let (f, took) = open(&file)?;
            writeln!(out, "file:        {}", f.name())?;
            writeln!(out, "size:        {} bytes", f.size_bytes())?;
            writeln!(out, "lines:       {}", f.line_count())?;
            if !f.schema().is_empty() {
                let names: Vec<&str> = f.schema().iter().map(|d| d.name.as_ref()).collect();
                writeln!(out, "fields:      {}", names.join(", "))?;
            }
            writeln!(out, "index mem:   {} KiB", f.index_memory_bytes() / 1024)?;
            writeln!(out, "index time:  {took:.2?} ({:.0} MiB/s)", mib_per_s(f.size_bytes(), took))?;
        }
        Cmd::Search { mut args, within, picks, ignore_case, fixed, invert, out: o } => {
            let file = PathBuf::from(args.pop().expect("clap requires FILE"));
            let options = MatcherOptions { case_insensitive: ignore_case, fixed_string: fixed, invert };
            match (args.pop(), picks.is_empty()) {
                (Some(_), false) => anyhow::bail!("give either a PATTERN or pack picks (-q, -l, -x, -X), not both"),
                (None, true) => anyhow::bail!("missing PATTERN (or pack picks: -q, -l, -x, -X)"),
                (Some(pattern), true) => {
                    let m = Matcher::new(&pattern, options)?;
                    match within {
                        None => run_filter(&file, |_| Ok(Filter::Regex(m)), &o, &mut out)?,
                        Some(w) => {
                            let set = SearchSet { include: vec![Filter::Regex(m)], exclude: vec![] };
                            run_search_within(&file, Some(&w), set, &[], &o, &mut out)?;
                        }
                    }
                }
                (None, false) => {
                    if ignore_case || fixed || invert {
                        anyhow::bail!("-i, -F and -v apply to a PATTERN, not to pack picks");
                    }
                    let e = picks.expand(&library(&cli.pack)?)?;
                    let names: Vec<String> = e.parts_with(Role::Include).map(|p| p.query.clone()).collect();
                    run_search_within(&file, within.as_deref(), e.search_set()?, &names, &o, &mut out)?;
                }
            }
        }
        Cmd::Filter { query, file, out: o } => {
            run_filter(&file, |source| Ok(Filter::Query(compile(&query, source)?)), &o, &mut out)?;
        }
        Cmd::Expand { picks, parts } => {
            if picks.is_empty() {
                anyhow::bail!("nothing picked: use -q ID, -l LABEL, -r ID, -R LABEL, -x ID or -X LABEL");
            }
            let e = picks.expand(&library(&cli.pack)?)?;
            writeln!(out, "{}", e.nfd)?;
            if parts {
                for p in &e.parts {
                    let role = match p.role {
                        Role::Include => "include",
                        Role::Require => "require",
                        Role::Exclude => "exclude",
                        Role::Signal => "signal",
                    };
                    let note = p.covered.as_ref().map(|by| format!(" (in {by}, not repeated)")).unwrap_or_default();
                    writeln!(out, "{role} {}{note}: {}", p.query, e.part_nfd(p))?;
                }
            }
        }
        Cmd::Packs => {
            for p in library(&cli.pack)?.packs() {
                writeln!(out, "{}  ({})", p.name, p.file)?;
                for q in &p.queries {
                    let mut line = format!("  {}.{}", p.name, q.id);
                    if !q.params.is_empty() {
                        line += &format!("({})", q.params.join(", "));
                    }
                    if let Some(parent) = p.parent_of(q) {
                        line += &format!("  parent: {parent}");
                    }
                    if !q.labels.is_empty() {
                        line += &format!("  labels: {}", q.labels.join(", "));
                    }
                    writeln!(out, "{line}")?;
                }
            }
        }
        Cmd::Marks { file } => {
            let (f, _) = open(&file)?;
            let marks = Bookmarks::load_for(&file, &*f)?;
            for (l, name, note) in marks.iter_named() {
                let text = f.line_text(l).unwrap_or_default();
                // Its anchor didn't find its line: it may be elsewhere.
                let name = if marks.is_unsure(l) { format!("{name}?") } else { name.to_owned() };
                if note.is_empty() {
                    writeln!(out, "{name} {}: {text}", l + 1)?;
                } else {
                    writeln!(out, "{name} {}: {text}\n    # {note}", l + 1)?;
                }
            }
        }
        Cmd::Lines { file, from, to } => {
            let (f, _) = open(&file)?;
            let last = to.min(f.line_count());
            for l in from.max(1)..=last {
                out.write_all(&f.line_bytes(l - 1).unwrap_or_default())?;
                out.write_all(b"\n")?;
            }
        }
    }
    out.flush()?;
    Ok(())
}
