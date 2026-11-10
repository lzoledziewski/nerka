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
//!     nerka --lifecycle 'ecu1: lines ecu == ECU1; reset uptime' lifecycles trace.dlt   (ECU1's boots)
//!     nerka --check 'stall: lines raw ~ /net down/; missing raw ~ /ota/ within 30s' check trace.dlt
//!     nerka export -o mine.nerka-bundle --only packs --only check:storm   (settings bundle)
//!     nerka import --dry-run theirs.nerka-bundle

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use nerka_core::bundle::{Bundle, Choice, Item, Kind, Status, Store};
use nerka_core::pack::{self, Expansion, Library, ParamValue, Params, Pick, Role, Selection};
use nerka_core::{
    Bookmarks, Filter, LineNo, LogSource, Matcher, MatcherOptions, Progress, Query, SearchSet, SignalDef, Signals,
    search, search_within,
};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::time::Instant;

#[derive(Parser)]
#[command(
    name = "nerka",
    version,
    about = "Fast log analysis: filter, search, signals, lifecycles and checks over text, JSON, DLT and captures"
)]
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
    /// A protocol definition (`.ksy`: a Kaitai Struct structure with a
    /// `-nerka:` section; repeatable) for messages in captures; give it
    /// ports with --protocol-port. See doc/protocol-definitions.md.
    #[arg(long, global = true, value_name = "FILE")]
    protocol: Vec<PathBuf>,
    /// UDP destination port of a protocol, by its name (repeatable):
    /// NAME=PORT, e.g. 'MYPROTO=40000'; short for --route NAME:PORT.
    #[arg(long = "protocol-port", global = true, value_name = "NAME=PORT")]
    protocol_port: Vec<String>,
    /// Which datagrams in captures are a protocol's (repeatable): NAME:RULE,
    /// a rule being a port or range, or conditions on what matters, e.g.
    /// 'DLT:port=3490,src=10.0.0.0/8' or 'MYPROTO:port=40000,vlan=5' (keys
    /// port, sport, src, dst, vlan; `*`: every datagram). A protocol given
    /// rules here uses only these; the most specific rule wins.
    #[arg(long, global = true, value_name = "NAME:RULE")]
    route: Vec<String>,
    /// Don't read this protocol in captures (repeatable): DLT, SOMEIP,
    /// syslog or a --protocol's name.
    #[arg(long = "disable-protocol", global = true, value_name = "NAME")]
    disable_protocol: Vec<String>,
    /// Folder compressed MF4 data is unpacked into (default: the per-user
    /// cache folder); unpacked once, reused when the file is opened again.
    #[arg(long = "unpack-dir", global = true, value_name = "DIR")]
    unpack_dir: Option<PathBuf>,
    /// FIBEX file describing DLT non-verbose messages (repeatable): their
    /// payload is shown as text, and their APID / CTID / level come from it.
    #[arg(long, global = true, value_name = "FILE")]
    fibex: Vec<PathBuf>,
    /// Define a signal for `@name` in queries (repeatable):
    /// NAME[(GROUP)][:num|enum|time][:all|filtered][:per(LIFECYCLE)][:default(VALUE)]=QUERY,
    /// e.g. 'speed=/speed=(?<speed>\d+)/'.
    #[arg(long, global = true, value_name = "SPEC")]
    signal: Vec<String>,
    /// Define a lifecycle of one entity (repeatable): NAME: CLAUSES, e.g.
    /// 'ecu1: lines ecu == ECU1; reset uptime; planned {sys.shutdown}'
    /// (`{pack.query}`: a library query, from --pack). It is a signal too
    /// (`@ecu1`, its number); signals reset with it with `:per(ecu1)`.
    #[arg(long, global = true, value_name = "SPEC")]
    lifecycle: Vec<String>,
    /// Define a check (repeatable): NAME: lines QUERY; TEST[; per
    /// LIFECYCLE], e.g. 'storm: lines raw ~ /retry/; rate 20 per 10s'.
    /// Tests: exists, count [OP N], rate N per DUR, gap DUR, then QUERY
    /// within DUR, missing QUERY within DUR, value first|last|min|max.
    #[arg(long, global = true, value_name = "SPEC")]
    check: Vec<String>,
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
    /// Several files: interleave their lines by time (when every file knows
    /// its lines' times: DLT, captures); else one after another, in the
    /// order given.
    #[arg(long, global = true)]
    merge: bool,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Index a file (several: one view) and print statistics (lines, size,
    /// throughput), its fields, protocols and what was skipped.
    #[command(alias = "info")]
    Open {
        #[arg(required = true, value_name = "FILE")]
        files: Vec<PathBuf>,
    },
    /// Print lines matching a regex (like grep -n). With --in, search only
    /// the lines of a filter. With pack picks (-q, -l, -x, -X) instead of a
    /// pattern, print which query matched each line. Several files: one
    /// view, each line as FILE:LINE:.
    Search {
        #[arg(required = true, num_args = 1.., value_names = ["PATTERN", "FILE"])]
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
    /// Several files: one view (`file` is a field), each line as FILE:LINE:.
    Filter {
        /// The NFD query.
        query: String,
        /// The files: text, DLT, captures (pcap, pcapng, MF4).
        #[arg(required = true, value_name = "FILE")]
        files: Vec<PathBuf>,
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
    Lines {
        file: PathBuf,
        /// The first line (1-based).
        from: u64,
        /// The last line (included).
        to: u64,
    },
    /// Run the checks (`--check`) on a file: issues and facts, overall and
    /// per lifecycle (`per`).
    Check {
        /// The files (several: one view).
        #[arg(required = true, value_name = "FILE")]
        files: Vec<PathBuf>,
        /// Only this check (default: all defined).
        #[arg(short = 'n', long)]
        name: Option<String>,
        /// Print JSON.
        #[arg(long)]
        json: bool,
        /// Fail (exit status 1) if an issue shows up.
        #[arg(long)]
        strict: bool,
        /// Also print the evidence: up to this many places per result.
        #[arg(short = 'e', long, value_name = "N", default_value_t = 0)]
        evidence: usize,
    },
    /// Print the lifecycles (`--lifecycle`) of a file: when each starts and
    /// how it ends, and where the definition and the uptime disagree
    /// (findings, on stderr).
    Lifecycles {
        /// The files (several: one view).
        #[arg(required = true, value_name = "FILE")]
        files: Vec<PathBuf>,
        /// Only this lifecycle (default: all defined).
        #[arg(short = 'n', long)]
        name: Option<String>,
        /// Print JSON (the lifecycles and an `issues` array).
        #[arg(long)]
        json: bool,
        /// Fail (exit status 1) if there are findings.
        #[arg(long)]
        strict: bool,
        /// Also count the lines of each lifecycle matching this NFD query.
        #[arg(long, value_name = "QUERY")]
        count: Option<String>,
    },
    /// Export the analysis setup of the per-user config folder (packs,
    /// presets, plots, checks, signals) as a settings bundle.
    Export {
        /// Write the bundle to FILE (default: stdout).
        #[arg(short = 'o', long, value_name = "FILE")]
        output: Option<PathBuf>,
        /// Only these items (repeatable): a KIND (pack, preset, plot, check,
        /// signal; or plural) or KIND:NAME.
        #[arg(long, value_name = "KIND[:NAME]")]
        only: Vec<String>,
    },
    /// Import a settings bundle into the per-user config folder: a line per
    /// item (new, same, different, invalid) and what was done with it.
    /// Exits 1 if an item was invalid (the others are imported).
    Import {
        /// The bundle (`-`: stdin).
        file: PathBuf,
        /// Only print what it would do.
        #[arg(long)]
        dry_run: bool,
        /// Items that differ from yours: keep both (theirs renamed
        /// NAME_imported), take theirs (yours backed up first), or keep mine.
        #[arg(long, value_enum, default_value_t = Conflicts::Both)]
        conflicts: Conflicts,
    },
}

#[derive(Clone, Copy, clap::ValueEnum)]
enum Conflicts {
    Both,
    Theirs,
    Mine,
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

/// The per-user config folder.
fn store() -> Result<Store> {
    Store::user().context("no per-user config folder (APPDATA, XDG_CONFIG_HOME or HOME isn't set)")
}

/// `nerka export`: the bundle to `output` or stdout, what is in it to stderr.
fn export(output: Option<&Path>, only: &[String], out: &mut impl Write) -> Result<()> {
    let mut picks: Vec<(Kind, Option<&str>)> = Vec::new();
    for o in only {
        let (k, name) = o.split_once(':').map_or((o.as_str(), None), |(k, n)| (k, Some(n)));
        let kind =
            Kind::parse(k).with_context(|| format!("--only {o}: '{k}' isn't pack, preset, plot, check or signal"))?;
        picks.push((kind, name));
    }
    let takes =
        |(k, n): &(Kind, Option<&str>), i: &Item| *k == i.kind && n.is_none_or(|n| n.eq_ignore_ascii_case(&i.name));
    let (bundle, errors) = store()?.export(&|i| picks.is_empty() || picks.iter().any(|p| takes(p, i)));
    for e in &errors {
        eprintln!("nerka: not exported: {e}");
    }
    for p in &picks {
        if p.1.is_some() && !bundle.items.iter().any(|i| takes(p, i)) {
            anyhow::bail!("--only {}:{}: no such {} in the config folder", p.0, p.1.unwrap_or_default(), p.0);
        }
    }
    let text = bundle.to_text();
    match output {
        Some(path) => nerka_core::write_atomic(path, &text).with_context(|| format!("writing {}", path.display()))?,
        None => out.write_all(text.as_bytes())?,
    }
    let counts: Vec<String> = Kind::ALL
        .iter()
        .filter_map(|&k| {
            let n = bundle.items.iter().filter(|i| i.kind == k).count();
            (n > 0).then(|| format!("{n} {}", if n == 1 { k.as_str() } else { k.plural() }))
        })
        .collect();
    eprintln!("nerka: exported {}", if counts.is_empty() { "nothing".to_owned() } else { counts.join(", ") });
    Ok(())
}

/// `nerka import`: the plan, a line per item, then what was done.
fn import(file: &Path, dry_run: bool, conflicts: Conflicts, out: &mut impl Write) -> Result<()> {
    let text = if file == Path::new("-") {
        std::io::read_to_string(std::io::stdin())?
    } else {
        std::fs::read_to_string(file).with_context(|| format!("reading {}", file.display()))?
    };
    let bundle = Bundle::parse(&file.display().to_string(), &text)?;
    let store = store()?;
    let mut plan = store.plan(&bundle);
    plan.set_conflicts(match conflicts {
        Conflicts::Both => Choice::Both,
        Conflicts::Theirs => Choice::Take,
        Conflicts::Mine => Choice::Skip,
    });
    for p in &plan.items {
        match &p.status {
            Status::Invalid(why) => writeln!(out, "{:<9}  {} {}: {why}", p.status_text(), p.kind, p.name)?,
            _ => writeln!(out, "{:<9}  {} {}: {}", p.status_text(), p.kind, p.name, p.choice_text(p.choice))?,
        }
    }
    for n in &plan.notes {
        writeln!(out, "note: {n}")?;
    }
    if dry_run {
        writeln!(
            out,
            "dry run: {} item{} would be written",
            plan.changes(),
            if plan.changes() == 1 { "" } else { "s" }
        )?;
    } else {
        let done = store.apply(&plan);
        writeln!(out, "imported: {}", done.summary())?;
        if !done.errors.is_empty() {
            out.flush()?;
            anyhow::bail!("{} item(s) not written", done.errors.len());
        }
    }
    if plan.invalid() > 0 {
        out.flush()?;
        anyhow::bail!("{} invalid item(s) not imported", plan.invalid());
    }
    Ok(())
}

/// Compile an NFD query for a source, with a caret under the error.
fn compile(query: &str, source: &dyn LogSource) -> Result<Query> {
    // `--signal` definitions, and `@time` on files that know their time.
    let signals = SIGNALS.get_or_init(Default::default).for_source(source);
    let q = Query::compile_in(query, source.schema(), &signals)
        .map_err(|e| anyhow::anyhow!("{}\n  {query}\n  {}", e.at(query), e.caret(query)))?;
    // What it can't find, said before it finds nothing (review 2026-11-08 R8,
    // 2026-11-09 R2).
    for w in nerka_core::query_warnings(&q, source) {
        eprintln!("nerka: warning: {w}");
    }
    Ok(q)
}

/// `bookmark` in a query: the bookmarks of the log (its `.nerka-marks`),
/// on the lines of `source` their anchors find.
fn with_marks(filter: Filter, view: &Opened) -> Result<Filter> {
    if !filter.uses_bookmarks() {
        return Ok(filter);
    }
    let marks = view_marks(&view.paths, &*view.source).context("reading the bookmarks")?;
    Ok(filter.with_bookmarks(&std::sync::Arc::new(marks)))
}

/// The bookmarks of a view's files (their `.nerka-marks`), on the rows
/// their lines are, as the GUI loads them.
fn view_marks(paths: &[PathBuf], f: &dyn LogSource) -> std::io::Result<Bookmarks> {
    if let Some(r) = f.reordered() {
        return Ok(view_marks(paths, r.inner)?.remapped(|l| r.row_of[l as usize]));
    }
    let Some(parts) = f.parts() else { return Bookmarks::load_for(&paths[0], f) };
    let files: Vec<(&Path, LineNo, &dyn LogSource)> =
        paths.iter().zip(&parts).map(|(p, part)| (p.as_path(), part.first, part.source.as_ref())).collect();
    Bookmarks::load_for_parts(&files)
}

/// Each row of a view: its file (index into the view's paths) and its
/// line there, merged by time or not.
fn locator(f: &dyn LogSource) -> impl Fn(LineNo) -> (usize, LineNo) + '_ {
    let (inner, line_of) = match f.reordered() {
        Some(r) => (r.inner, Some(r.line_of)),
        None => (f, None),
    };
    let starts: Vec<LineNo> = inner.part_files().iter().map(|&(first, _)| first).collect();
    move |row| {
        let l = line_of.map_or(row, |m| m[row as usize]);
        let k = starts.partition_point(|&s| s <= l).saturating_sub(1);
        (k, l - starts[k])
    }
}

/// A printed line's prefix: `LINE:`, or `FILE:LINE:` for a view of several
/// files (the line its file's own, as grep prints them); `-N` drops the line.
fn write_prefix(
    out: &mut impl Write,
    o: &OutputArgs,
    view: &Opened,
    at: &impl Fn(LineNo) -> (usize, LineNo),
    row: LineNo,
) -> std::io::Result<()> {
    if view.paths.len() > 1 {
        let (k, line) = at(row);
        write!(out, "{}:", view.paths[k].display())?;
        if !o.no_line_numbers {
            write!(out, "{}:", line + 1)?;
        }
    } else if !o.no_line_numbers {
        write!(out, "{}:", row + 1)?;
    }
    Ok(())
}

/// `build` gets the opened source (its fields, its time), so query errors about
/// unknown fields point into the query like syntax errors do.
fn run_filter(
    files: &[PathBuf],
    build: impl FnOnce(&dyn LogSource) -> Result<Filter>,
    o: &OutputArgs,
    out: &mut impl Write,
) -> Result<()> {
    let view = open(files)?;
    let (f, t_index) = (&*view.source, view.took);
    let filter = with_marks(build(f)?, &view)?;
    let at = locator(f);
    let t = Instant::now();
    let res = search(f, &filter, &Progress::new())?;
    let t_search = t.elapsed();
    if o.count {
        writeln!(out, "{}", res.lines.len())?;
    } else {
        for &l in &res.lines {
            write_prefix(out, o, &view, &at, l)?;
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
    files: &[PathBuf],
    within: Option<&str>,
    set: SearchSet,
    names: &[String],
    o: &OutputArgs,
    out: &mut impl Write,
) -> Result<()> {
    let view = open(files)?;
    let (f, t_index) = (&*view.source, view.took);
    let filter = within.map(|q| compile(q, f).and_then(|q| with_marks(Filter::Query(q), &view))).transpose()?;
    let set = SearchSet {
        include: set.include.into_iter().map(|q| with_marks(q, &view)).collect::<Result<_>>()?,
        exclude: set.exclude.into_iter().map(|q| with_marks(q, &view)).collect::<Result<_>>()?,
    };
    let at = locator(f);
    let t = Instant::now();
    let lines = match filter {
        Some(q) => Some(search(f, &q, &Progress::new())?.lines),
        None => None,
    };
    let t_filter = t.elapsed();
    let t = Instant::now();
    let hits = search_within(f, lines.as_deref(), &set, &Progress::new())?;
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
            write_prefix(out, o, &view, &at, l)?;
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

/// Print the lifecycles of `file` (all definitions, or the one named);
/// returns the number of findings.
fn lifecycles(
    files: &[PathBuf],
    name: Option<&str>,
    count: Option<&str>,
    json: bool,
    out: &mut impl Write,
) -> Result<usize> {
    use nerka_core::time::format_time;
    let f = open(files)?.source;
    let signals = SIGNALS.get_or_init(Default::default).for_source(&*f);
    let mut defs: Vec<_> = signals.lifecycles().collect();
    if defs.is_empty() {
        anyhow::bail!("no lifecycle defined: use --lifecycle 'NAME: lines QUERY; reset uptime'");
    }
    if let Some(n) = name {
        defs.retain(|d| d.name().eq_ignore_ascii_case(n));
        if defs.is_empty() {
            let names: Vec<&str> = signals.lifecycles().map(|d| d.name()).collect();
            anyhow::bail!("no lifecycle '{n}' (defined: {})", names.join(", "));
        }
    }
    let hits = match count {
        Some(q) => Some(search(&*f, &Filter::Query(compile(q, &*f)?), &Progress::new())?.lines),
        None => None,
    };
    let time = |t: Option<f64>| t.map(|t| format_time(t, 3)).unwrap_or_else(|| "-".into());
    let secs = |d: Option<f64>| d.map(|d| format!("{d:.1}")).unwrap_or_else(|| "-".into());
    let num = |v: Option<f64>| v.map(|v| format!("{v}")).unwrap_or_else(|| "null".into());
    let (mut rows, mut issues, mut summaries, mut findings) = (Vec::new(), Vec::new(), Vec::new(), 0);
    if !json {
        let c = if hits.is_some() { "	count" } else { "" };
        writeln!(out, "lifecycle	#	line	start	end	duration_s	outcome{c}	findings")?;
    }
    for def in defs {
        let t = Instant::now();
        let l = def.lifecycles(&*f, &Progress::new())?;
        let took = t.elapsed();
        // Lines per lifecycle matching `count`.
        let counts: Option<Vec<u64>> = hits.as_ref().map(|hits| {
            let mut c = vec![0u64; l.lives.len()];
            for &row in hits {
                if let Some((i, _)) = l.at(row) {
                    c[i] += 1;
                }
            }
            c
        });
        let label = |i: usize| format!("{}#{}", def.name(), l.lives[i].number);
        findings += l.findings.len();
        for (i, x) in l.lives.iter().enumerate() {
            let notes: Vec<&str> = l.findings_of(i).map(|f| f.kind.as_str()).collect();
            if json {
                let c = counts.as_ref().map(|c| format!(", \"count\": {}", c[i])).unwrap_or_default();
                rows.push(format!(
                    "  {{\"lifecycle\": {}, \"number\": {}, \"first_line\": {}, \"last_line\": {}, \"lines\": {},                      \"started_by\": {}, \"outcome\": {}, \"marker_line\": {}, \"start\": {}, \"end\": {},                      \"duration\": {}, \"first_uptime\": {}, \"boot\": {}, \"spread\": {}{c}}}",
                    js(def.name()),
                    x.number,
                    x.first + 1,
                    x.last + 1,
                    x.lines,
                    js(x.started_by.as_str()),
                    js(x.outcome.as_str()),
                    x.marker.map_or("null".into(), |m| (m + 1).to_string()),
                    num(x.first_time),
                    num(x.last_time),
                    num(x.duration()),
                    num(x.first_uptime),
                    num(x.boot),
                    num(x.spread),
                ));
            } else {
                let c = counts.as_ref().map(|c| format!("	{}", c[i])).unwrap_or_default();
                writeln!(
                    out,
                    "{}	{}	{}	{}	{}	{}	{}{c}	{}",
                    def.name(),
                    x.number,
                    x.first + 1,
                    time(x.first_time),
                    time(x.last_time),
                    secs(x.duration()),
                    x.outcome.as_str(),
                    notes.join(", ")
                )?;
            }
        }
        for x in &l.findings {
            if json {
                issues.push(format!(
                    "  {{\"kind\": {}, \"line\": {}, \"count\": {}, \"lifecycle\": {}, \"detail\": {}}}",
                    js(x.kind.as_str()),
                    x.row + 1,
                    x.count,
                    js(&label(x.life)),
                    js(&x.detail)
                ));
            } else {
                out.flush()?;
                eprintln!("warning: {} line {}: {}: {}", label(x.life), x.row + 1, x.kind.as_str(), x.detail);
            }
        }
        summaries.push(format!("{}: {}", def.name(), l.summary()));
        if !json {
            out.flush()?;
            eprintln!("{}: {} | {took:.2?}", def.name(), l.summary());
        }
    }
    if json {
        writeln!(
            out,
            "{{\"lifecycles\": [
{}
 ],",
            rows.join(
                ",
"
            )
        )?;
        writeln!(
            out,
            " \"issues\": [
{}
 ],",
            issues.join(
                ",
"
            )
        )?;
        let s: Vec<String> = summaries.iter().map(|s| js(s)).collect();
        writeln!(out, " \"summary\": [{}]}}", s.join(", "))?;
    }
    Ok(findings)
}

/// Run the `--check` checks on `file` (all, or the one named); returns how
/// many results are issues.
fn checks(
    files: &[PathBuf],
    name: Option<&str>,
    json: bool,
    evidence: usize,
    packs: &[PathBuf],
    out: &mut impl Write,
) -> Result<usize> {
    use nerka_core::check::{CheckDef, Checks, Verdict};
    let mut all = Checks::default();
    for spec in CHECKS.get_or_init(Default::default) {
        let d = CheckDef::parse_spec(spec).map_err(|e| {
            anyhow::anyhow!(
                "--check: {}
  {spec}
  {}^",
                e.msg,
                " ".repeat(e.column(spec) - 1)
            )
        })?;
        all.set(d);
    }
    if all.iter().next().is_none() {
        anyhow::bail!("no check defined: use --check 'NAME: lines QUERY; TEST'");
    }
    if let Some(n) = name
        && all.get(n).is_none()
    {
        let names: Vec<&str> = all.iter().map(|d| d.name()).collect();
        anyhow::bail!("no check '{n}' (defined: {})", names.join(", "));
    }
    let lib = if all.iter().any(|d| !d.unresolved().is_empty()) { library(packs)? } else { Default::default() };
    let f = open(files)?.source;
    let signals = SIGNALS.get_or_init(Default::default).for_source(&*f);
    // Every check runs (one may use another's signal); the named one shows.
    let t = Instant::now();
    let run = all.run_all(&*f, &signals, &lib, &Progress::new())?;
    let took = t.elapsed();
    let (mut issues, mut rows) = (0, Vec::new());
    if !json {
        writeln!(out, "check	kind	scope	result")?;
    }
    for (n, r) in run.results {
        if name.is_some_and(|want| !n.eq_ignore_ascii_case(want)) {
            continue;
        }
        let r = r.map_err(|e| anyhow::anyhow!(e))?;
        if let Some(note) = &r.signal_note {
            eprintln!("check '{}': {note}", r.name);
        }
        for s in &r.scopes {
            let shown = match &s.verdict {
                Verdict::Ok => "ok".to_owned(),
                Verdict::Issue(n) => format!("{n}x"),
                Verdict::Fact(Some(v)) => v.clone(),
                Verdict::Fact(None) => "-".into(),
            };
            if matches!(s.verdict, Verdict::Issue(_)) {
                issues += 1;
            }
            if json {
                let verdict = match &s.verdict {
                    Verdict::Ok => "\"ok\"".to_owned(),
                    Verdict::Issue(n) => format!("{{\"issue\": {n}}}"),
                    Verdict::Fact(v) => format!("{{\"fact\": {}}}", v.as_deref().map_or("null".into(), js)),
                };
                let ev: Vec<String> = s
                    .evidence
                    .iter()
                    .take(evidence.max(1))
                    .map(|e| {
                        let lines: Vec<String> = e.rows.iter().map(|r| (r + 1).to_string()).collect();
                        format!("{{\"lines\": [{}], \"note\": {}}}", lines.join(", "), js(&e.note))
                    })
                    .collect();
                rows.push(format!(
                    "  {{\"check\": {}, \"kind\": {}, \"scope\": {}, \"first_line\": {}, \"end_line\": {}, \
                     \"verdict\": {verdict}, \"evidence\": [{}]}}",
                    js(&r.name),
                    js(r.kind.as_str()),
                    js(&s.scope.label),
                    s.scope.from + 1,
                    s.scope.to,
                    ev.join(", ")
                ));
            } else {
                writeln!(out, "{}\t{}\t{}\t{shown}", r.name, r.kind.as_str(), s.scope.label)?;
                for e in s.evidence.iter().take(evidence) {
                    let lines: Vec<String> = e.rows.iter().map(|r| (r + 1).to_string()).collect();
                    let text = f.line_text(e.rows[0]).unwrap_or_default();
                    let note = if e.note.is_empty() { String::new() } else { format!(" ({})", e.note) };
                    writeln!(out, "\t\t\tline {}{note}: {text}", lines.join(" -> "))?;
                }
            }
        }
    }
    if !json {
        out.flush()?;
        eprintln!("checks: {took:.2?}");
    }
    if json {
        writeln!(out, "{{\"results\": [\n{}\n ]}}", rows.join(",\n"))?;
    }
    Ok(issues)
}

/// The `--check` specs, set once in `main`.
static CHECKS: std::sync::OnceLock<Vec<String>> = std::sync::OnceLock::new();

/// A JSON string.
fn js(s: &str) -> String {
    let mut o = String::with_capacity(s.len() + 2);
    o.push('"');
    for c in s.chars() {
        match c {
            '"' => o.push_str("\\\""),
            '\\' => o.push_str("\\\\"),
            '\n' => o.push_str("\\n"),
            '\r' => o.push_str("\\r"),
            '\t' => o.push_str("\\t"),
            c if (c as u32) < 0x20 => o.push_str(&format!("\\u{:04x}", c as u32)),
            c => o.push(c),
        }
    }
    o.push('"');
    o
}

/// The `--signal` definitions, set once in `main`.
static SIGNALS: std::sync::OnceLock<Signals> = std::sync::OnceLock::new();

/// How files are opened (`--dlt-port`, `--someip-port`, …), set once in `main`.
static OPEN_OPTIONS: std::sync::OnceLock<nerka_core::OpenOptions> = std::sync::OnceLock::new();

/// The `--protocol` definitions, and the rules `--route`, `--protocol-port`
/// and `--disable-protocol` give (by protocol name: native or loaded).
fn capture_protocols(cli: &Cli, capture: &mut nerka_core::input::capture::CaptureOptions) -> Result<()> {
    use nerka_core::input::capture::route::{Match, Route};
    use nerka_core::input::proto::{LoadedProtocol, Protocol, ksy::KsyProtocol};
    for f in &cli.protocol {
        let p = KsyProtocol::load(f)?;
        if capture.protocols.iter().any(|o| o.protocol.name().eq_ignore_ascii_case(p.name())) {
            anyhow::bail!("{}: a protocol named {} is loaded already", f.display(), p.name());
        }
        capture.protocols.push(LoadedProtocol::new(std::sync::Arc::new(p)));
    }
    let known = |name: &str| {
        ["DLT", "SOMEIP", "syslog"].iter().any(|n| n.eq_ignore_ascii_case(name))
            || capture.protocols.iter().any(|p| p.protocol.name().eq_ignore_ascii_case(name))
    };
    let mut routes = Vec::new();
    for spec in &cli.protocol_port {
        let (name, port) =
            spec.split_once('=').ok_or_else(|| anyhow::anyhow!("--protocol-port {spec}: write NAME=PORT"))?;
        let port: u16 =
            port.trim().parse().map_err(|_| anyhow::anyhow!("--protocol-port {spec}: `{port}` isn't a port"))?;
        routes.push(Route { protocol: name.trim().to_owned(), rule: Match::port(port) });
    }
    for spec in &cli.route {
        routes.push(spec.parse::<Route>().map_err(|e| anyhow::anyhow!("--route {spec}: {e}"))?);
    }
    for r in &routes {
        if !known(&r.protocol) {
            anyhow::bail!("no protocol is named {} (DLT, SOMEIP, syslog or a --protocol's name)", r.protocol);
        }
    }
    for name in &cli.disable_protocol {
        if !known(name) {
            anyhow::bail!("--disable-protocol {name}: no protocol is named so");
        }
    }
    capture.routes = routes;
    capture.disabled = cli.disable_protocol.clone();
    for p in &capture.protocols {
        let name = p.protocol.name();
        if p.routes.is_empty() && !capture.routes.iter().any(|r| r.protocol.eq_ignore_ascii_case(name)) {
            eprintln!("nerka: protocol {name} matches no datagrams (--route {name}:PORT, or `match:` in its file)");
        }
    }
    Ok(())
}

/// Files opened as one view.
struct Opened {
    source: Box<dyn LogSource>,
    took: std::time::Duration,
    /// The files in the view's order (`--merge` orders them by their start).
    paths: Vec<PathBuf>,
}

/// [`open_silent`], and what the reader skipped or dropped on stderr
/// (review 2026-11-09, R3: counted, never said).
fn open(paths: &[PathBuf]) -> Result<Opened> {
    let view = open_silent(paths)?;
    for n in view.source.info().notes {
        eprintln!("nerka: note: {n}");
    }
    Ok(view)
}

/// One file, or several as one view (review 2026-11-09, R6: the console
/// opened one file): one after another in the order given, or merged by
/// time with `--merge`; files of kinds that don't go together are refused.
fn open_silent(paths: &[PathBuf]) -> Result<Opened> {
    // Said plainly (it was "access denied" on Windows).
    if let Some(dir) = paths.iter().find(|p| p.is_dir()) {
        anyhow::bail!("{} is a folder: give a file", dir.display());
    }
    let t = Instant::now();
    let options = OPEN_OPTIONS.get_or_init(Default::default);
    let progress = Progress::new();
    let (source, paths) = match paths {
        [one] => (
            nerka_core::open_with(one, options, &progress).with_context(|| format!("opening {}", one.display()))?,
            vec![one.clone()],
        ),
        _ if MERGE.get().copied().unwrap_or(false) => nerka_core::open_merged_by_time(paths, options, &progress)?,
        _ => (nerka_core::open_many(paths, options, &progress)?, paths.to_vec()),
    };
    Ok(Opened { source, took: t.elapsed(), paths })
}

/// `--merge`, set once in `main`.
static MERGE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();

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
    // A config folder written by an earlier nerka: to this one's formats.
    if let Some(m) = nerka_core::bundle::Store::user().and_then(|s| s.upgrade().message()) {
        eprintln!("nerka: {m}");
    }
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
    capture_protocols(&cli, &mut options.capture)?;
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
    MERGE.set(cli.merge).expect("set once");
    let mut signals = Signals::default();
    for spec in &cli.signal {
        signals.set(SignalDef::parse_spec(spec).with_context(|| format!("--signal {spec}"))?);
    }
    for spec in &cli.lifecycle {
        let def = nerka_core::lifecycle::LifecycleDef::parse_spec(spec)
            .map_err(|e| anyhow::anyhow!("--lifecycle: {}\n  {spec}\n  {}^", e.msg, " ".repeat(e.column(spec) - 1)))?;
        signals.set(SignalDef::lifecycle(def));
    }
    // Library queries in lifecycles, from the packs.
    if signals.lifecycles().any(|l| !l.unresolved().is_empty())
        && let Some(e) = signals.resolve_lifecycles(&library(&cli.pack)?).into_iter().next()
    {
        anyhow::bail!("{e}");
    }
    SIGNALS.set(signals).expect("set once");
    CHECKS.set(cli.check.clone()).expect("set once");
    let stdout = std::io::stdout().lock();
    let mut out = BufWriter::with_capacity(1 << 16, stdout);

    match cli.cmd {
        Cmd::Open { files } => {
            let view = open_silent(&files)?;
            let (f, took) = (&*view.source, view.took);
            let info = f.info();
            if view.paths.len() > 1 {
                let names: Vec<String> = view.paths.iter().map(|p| p.display().to_string()).collect();
                writeln!(out, "files:       {}", names.join(", "))?;
            } else {
                writeln!(out, "file:        {}", f.name())?;
            }
            writeln!(out, "read as:     {}", f.format())?;
            writeln!(out, "size:        {} bytes", f.size_bytes())?;
            writeln!(out, "lines:       {}", f.line_count())?;
            if !info.protocols.is_empty() {
                writeln!(out, "protocols:   {}", info.protocols.join(", "))?;
            }
            if !f.schema().is_empty() {
                let names: Vec<&str> = f.schema().iter().map(|d| d.name.as_ref()).collect();
                writeln!(out, "fields:      {}", names.join(", "))?;
            }
            writeln!(out, "index mem:   {} KiB", f.index_memory_bytes() / 1024)?;
            writeln!(out, "index time:  {took:.2?} ({:.0} MiB/s)", mib_per_s(f.size_bytes(), took))?;
            for n in &info.notes {
                writeln!(out, "note:        {n}")?;
            }
        }
        Cmd::Search { args, within, picks, ignore_case, fixed, invert, out: o } => {
            let options = MatcherOptions { case_insensitive: ignore_case, fixed_string: fixed, invert };
            // PATTERN FILE…, or FILE… with pack picks.
            let pattern = if picks.is_empty() { args.first().cloned() } else { None };
            let files: Vec<PathBuf> = args.iter().skip(usize::from(pattern.is_some())).map(PathBuf::from).collect();
            if !picks.is_empty() && !files.is_empty() && !files[0].exists() {
                anyhow::bail!("give either a PATTERN or pack picks (-q, -l, -x, -X), not both");
            }
            if files.is_empty() {
                anyhow::bail!("missing FILE (search PATTERN FILE…, or search FILE… with pack picks)");
            }
            match (pattern, picks.is_empty()) {
                (None, true) => anyhow::bail!("missing PATTERN (or pack picks: -q, -l, -x, -X)"),
                (Some(_), false) => unreachable!("a pattern is read only without picks"),
                (Some(pattern), true) => {
                    let m = Matcher::new(&pattern, options)?;
                    match within {
                        None => run_filter(&files, |_| Ok(Filter::Regex(m)), &o, &mut out)?,
                        Some(w) => {
                            let set = SearchSet { include: vec![Filter::Regex(m)], exclude: vec![] };
                            run_search_within(&files, Some(&w), set, &[], &o, &mut out)?;
                        }
                    }
                }
                (None, false) => {
                    if ignore_case || fixed || invert {
                        anyhow::bail!("-i, -F and -v apply to a PATTERN, not to pack picks");
                    }
                    let e = picks.expand(&library(&cli.pack)?)?;
                    let names: Vec<String> = e.parts_with(Role::Include).map(|p| p.query.clone()).collect();
                    run_search_within(&files, within.as_deref(), e.search_set()?, &names, &o, &mut out)?;
                }
            }
        }
        Cmd::Filter { query, files, out: o } => {
            run_filter(&files, |source| Ok(Filter::Query(compile(&query, source)?)), &o, &mut out)?;
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
        Cmd::Export { output, only } => export(output.as_deref(), &only, &mut out)?,
        Cmd::Import { file, dry_run, conflicts } => import(&file, dry_run, conflicts, &mut out)?,
        Cmd::Marks { file } => {
            let f = open(std::slice::from_ref(&file))?.source;
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
        Cmd::Check { files, name, json, strict, evidence } => {
            let issues = checks(&files, name.as_deref(), json, evidence, &cli.pack, &mut out)?;
            if strict && issues > 0 {
                out.flush()?;
                anyhow::bail!("{issues} issue{} found", if issues == 1 { "" } else { "s" });
            }
        }
        Cmd::Lifecycles { files, name, json, strict, count } => {
            let found = lifecycles(&files, name.as_deref(), count.as_deref(), json, &mut out)?;
            if strict && found > 0 {
                out.flush()?;
                anyhow::bail!("{found} finding{}", if found == 1 { "" } else { "s" });
            }
        }
        Cmd::Lines { file, from, to } => {
            if from > to {
                anyhow::bail!("lines {from} to {to}: the first line is after the last");
            }
            let f = open(std::slice::from_ref(&file))?.source;
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
