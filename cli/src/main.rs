//! `nerka` — command line front end to the Nerka engine.
//!
//! ```text
//! nerka open   app.log          (plain text or DLT, picked by content)
//! nerka search -i 'error|timeout' app.log
//! nerka filter 'level >= ERROR AND raw ~ /SSL|CERT/' app.log
//! nerka lines  app.log 1000 1050
//! nerka filter --json -m 20 'level >= ERROR' app.log   (JSON Lines, at most 20)
//! nerka open --json trace.dlt                       (what a file is: fields, time, levels)
//! nerka help nfd                                    (the query language)
//! nerka marks  app.log
//! nerka packs                                       (list loaded query packs)
//! nerka expand -q vision.frame_drops -p cam=CAM1    (pack picks -> plain NFD)
//! nerka search --in 'level >= WARN' timeout app.log (search inside a filter)
//! nerka search --in 'ecu == SOCB' -l ota app.log     (per pack query)
//! nerka --signal 'v=/v=(?<v>\d+)/' filter '@v > 100' app.log   (signals)
//! nerka --fibex ecu1.xml filter 'apid == NAV' trace.dlt         (non-verbose DLT)
//! nerka --lifecycle 'ecu1: lines ecu == ECU1; reset uptime' lifecycles trace.dlt   (ECU1's boots)
//! nerka --check 'stall: lines raw ~ /net down/; missing raw ~ /ota/ within 30s' check trace.dlt
//! nerka export -o mine.nerka-bundle --only packs --only check:storm   (settings bundle)
//! nerka import --dry-run theirs.nerka-bundle
//! ```

use anyhow::{Context, Result};
use clap::{CommandFactory, Parser, Subcommand};
use nerka_core::agent;
use nerka_core::bundle::{Bundle, Choice, Item, Kind, Status, Store};
use nerka_core::json::{Value, string as js};
use nerka_core::pack::{self, Expansion, Library, ParamValue, Pick, Role, Selection};
use nerka_core::pattern::{self, PatternConfig, PatternOptions};
use nerka_core::{
    Bookmarks, Filter, LineNo, LogSource, Matcher, MatcherOptions, Progress, Query, SearchSet, SignalDef, Signals,
    search, search_within,
};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::time::Instant;

mod mcp;

#[derive(Parser)]
#[command(
    name = "nerka",
    version,
    about = "Fast log analysis: filter, search, signals, lifecycles and checks over text, JSON, DLT and captures",
    after_help = "Exit status: 0 lines found (or done), 1 no line found (filter, search), an issue                   (--strict) or an invalid item (import), 2 an error.",
    disable_help_subcommand = true
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
    /// ports with --protocol-port. See docs/user/protocol-definitions.md.
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
    /// `NAME[(GROUP)][:num|enum|time][:all|filtered][:per(LIFECYCLE)][:default(VALUE)]=QUERY`,
    /// e.g. `'speed=/speed=(?<speed>\d+)/'`.
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
    /// within DUR, missing QUERY within DUR, value first|last|min|max. Rate,
    /// gap, then and missing expect lines in time order (several files: --merge).
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
    /// Several files: their lines sorted by time (when every file knows
    /// its lines' times: DLT, captures; ties in the order given); else one
    /// after another, in the order given.
    #[arg(long, global = true)]
    merge: bool,
    /// How text files are read (repeatable): `NAME` forces that profile
    /// on every text file (a saved one, or `default`); `NAME: CLAUSES`
    /// defines one for this run, chosen by its rules before the saved ones,
    /// e.g. `'app: name *.log; time /^(?<time>\S+ \S+)/; zone local'`.
    /// Clauses: name GLOBS, first /REGEX/, records SPEC, time auto|none|/REGEX/,
    /// zone utc|local|+HH:MM, numbers s|ms|us|ns|auto, signal SPEC.
    #[arg(long, global = true, value_name = "NAME|SPEC")]
    profile: Vec<String>,
    /// Move a file's times (repeatable): FILE=OFFSET, the file as given or
    /// its name, e.g. 'ecu1.dlt=+3h' or 'app.log=-0.250s'.
    #[arg(long, global = true, value_name = "FILE=OFFSET")]
    offset: Vec<String>,
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
        /// Print one JSON object: files, format, size, lines, index,
        /// columns, fields (name, kind, costly), protocols, first and last
        /// time, lines per level, how each file was read, notes.
        #[arg(long)]
        json: bool,
    },
    /// An MCP server (stdio) for an AI agent of your choice: its tools work
    /// on the files open in a nerka window, and change what you see there
    /// (docs/user/agents.md).
    Mcp {
        /// The window's process id (default: NERKA_WINDOW, else the window
        /// focused last).
        #[arg(long, value_name = "PID")]
        window: Option<u32>,
        /// Print the MCP configuration (JSON) to put in a client's settings.
        #[arg(long, conflicts_with = "install")]
        config: bool,
        /// Register `nerka mcp` with an AI client by running its own `mcp
        /// add`: claude, codex or gemini.
        #[arg(long, value_name = "CLIENT")]
        install: Option<String>,
    },
    /// Help: the commands, `help COMMAND`, `help nfd` (the query language),
    /// `help fields FILE…` (the fields queries can use on those files),
    /// `help agent` (how to analyse with nerka, for AI agents) or `help
    /// skill` (the same as a SKILL.md).
    Help {
        #[arg(value_name = "TOPIC")]
        topic: Vec<String>,
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
        /// Also every line up to this long before each match, by time
        /// (30s, 500ms, 2m): what led to it. Matches are marked `>`, `--`
        /// stands between windows apart.
        #[arg(long, value_name = "DUR")]
        before: Option<String>,
        /// Also every line up to this long after each match, by time.
        #[arg(long, value_name = "DUR")]
        after: Option<String>,
        #[command(flatten)]
        out: OutputArgs,
    },
    /// Group lines into patterns (experimental): message templates with
    /// `<*>` where the values differ, most lines first, each with its count,
    /// first line and (with --queries) an NFD query finding its lines.
    Patterns {
        /// The files (several: one view).
        #[arg(required = true, value_name = "FILE")]
        files: Vec<PathBuf>,
        /// Only the lines matching this NFD filter.
        #[arg(long = "in", value_name = "FILTER")]
        within: Option<String>,
        #[command(flatten)]
        engine: PatternArgs,
        /// Print at most this many patterns (0: all).
        #[arg(short = 'n', long, default_value_t = 30)]
        top: usize,
        /// The rarest patterns first.
        #[arg(long)]
        rare: bool,
        /// Also print each slot's values: lines, first and last line.
        #[arg(long)]
        values: bool,
        /// Also print each pattern's NFD query.
        #[arg(long)]
        queries: bool,
        /// Also print each pattern's first N lines (0: all), with their
        /// line numbers.
        #[arg(short = 'l', long, value_name = "N")]
        lines: Option<usize>,
        /// Print one JSON object: `total`, `patterns` (each with what the
        /// options above add) and `more`.
        #[arg(long)]
        json: bool,
        /// Print timing to stderr.
        #[arg(long)]
        stats: bool,
    },
    /// Write the files' lines with every word made up, so a trace can be
    /// shared (for tuning patterns, a bug report): the messages keep their
    /// structure, and so their patterns. DLT stays DLT (times, levels and
    /// IDs kept); other files become text.
    Anonymize {
        /// The files (several: one view).
        #[arg(required = true, value_name = "FILE")]
        files: Vec<PathBuf>,
        /// The file to write.
        #[arg(short = 'o', long, value_name = "OUT")]
        output: PathBuf,
        /// Only the lines matching this NFD filter.
        #[arg(long = "in", value_name = "FILTER")]
        within: Option<String>,
        /// A word to keep as it is (repeatable; any case).
        #[arg(long, value_name = "WORD")]
        keep: Vec<String>,
        /// The same seed makes the same words (default: a random one, so
        /// nobody can map them back).
        #[arg(long)]
        seed: Option<String>,
        /// Make up DLT ECU, application and context IDs too.
        #[arg(long)]
        ids: bool,
    },
    /// Compare two sets of lines (experimental): two files, or one file's
    /// lines of two filters (--a, --b). Lines are grouped into patterns across
    /// both, then what differs is listed: patterns and slot values new in B,
    /// gone from B, then those more or less often, surest first.
    Compare {
        /// A_FILE B_FILE, or one FILE with --a and --b.
        #[arg(required = true, num_args = 1..=2, value_name = "FILE")]
        files: Vec<PathBuf>,
        /// Set A: only the lines matching this NFD filter.
        #[arg(long = "a", value_name = "FILTER")]
        a: Option<String>,
        /// Set B: only the lines matching this NFD filter.
        #[arg(long = "b", value_name = "FILTER")]
        b: Option<String>,
        /// Both sets: only the lines matching this NFD filter.
        #[arg(long = "in", value_name = "FILTER")]
        within: Option<String>,
        #[command(flatten)]
        engine: PatternArgs,
        /// Only changes at least this sure (new and gone ones are always
        /// shown): G², 3.84 is p = 0.05, 10.8 is p = 0.001.
        #[arg(long, default_value_t = 10.8)]
        min_score: f64,
        /// Print at most this many differences (0: all).
        #[arg(short = 'n', long, default_value_t = 30)]
        top: usize,
        /// Also print each pattern's NFD query.
        #[arg(long)]
        queries: bool,
        /// Print one JSON object: `a`, `b`, `differences` and `more`.
        #[arg(long)]
        json: bool,
        /// Print timing to stderr.
        #[arg(long)]
        stats: bool,
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
    /// List the bookmarks saved for a file (its `.nerka-marks`), or add,
    /// change or remove one: an agent's findings, there when the file is
    /// opened in the window (an open window takes them in when it gets
    /// the focus back).
    Marks {
        file: PathBuf,
        /// Print JSON Lines: line, name, note, author, unsure (its anchor
        /// didn't find its line), raw.
        #[arg(long)]
        json: bool,
        /// Bookmark this line (from 1), or change the bookmark there; prints it.
        #[arg(long, value_name = "LINE")]
        add: Option<u64>,
        /// With --add: its note.
        #[arg(long, requires = "add")]
        note: Option<String>,
        /// With --add: its name (default: the first free of B1, B2…).
        #[arg(long, requires = "add")]
        name: Option<String>,
        /// With --add: who made it, `ai` by default for a new one; `--by ''`:
        /// the user's.
        #[arg(long, requires = "add", value_name = "WHO")]
        by: Option<String>,
        /// Remove the bookmark on this line (from 1).
        #[arg(long, value_name = "LINE", conflicts_with = "add")]
        remove: Option<u64>,
    },
    /// Print a 1-based inclusive range of lines.
    Lines {
        file: PathBuf,
        /// The first line (1-based).
        from: u64,
        /// The last line (included).
        to: u64,
        #[command(flatten)]
        json: JsonArgs,
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
    /// presets, plots, checks, signals, profiles) as a settings bundle.
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

/// What a pattern run's stages did (`--stats`).
fn engine_stats(s: &pattern::PatternStats) -> String {
    let mut out = format!(
        "stage 1: {} templates {:.2?} | stage 2: {:.2?}, kept apart: {} new slots, {} side, {} rules",
        s.templates, s.stage1, s.stage2, s.apart, s.side, s.rules
    );
    if s.ignored != [0, 0] {
        out += &format!(" | ignored {} lines", s.ignored[0] + s.ignored[1]);
    }
    out
}

/// How `patterns` and `compare` group lines.
#[derive(clap::Args)]
struct PatternArgs {
    /// Group this field's text (default: `payload` if the file has it,
    /// else the line).
    #[arg(long)]
    field: Option<String>,
    /// How alike a message must be to join a template (0–1; the setting
    /// `similarity`).
    #[arg(long)]
    similarity: Option<f32>,
    /// Don't merge templates by alignment (Drain's templates only; the
    /// setting `merge off`).
    #[arg(long)]
    no_merge: bool,
    /// Settings and rules (default: the saved ones, `<config>/nerka/patterns`;
    /// `-`: none, the defaults).
    #[arg(long, value_name = "FILE")]
    pattern_config: Option<PathBuf>,
    /// A setting for this run, after the configuration's (repeatable):
    /// `similarity=0.7`, `header=sample 16 min 4`.
    #[arg(long = "set", value_name = "KEY=VALUE")]
    set: Vec<String>,
    /// A rule for this run, after the configuration's (repeatable):
    /// `'apid == NAV: keep Elapsed'`.
    #[arg(long = "rule", value_name = "RULE")]
    rules: Vec<String>,
}

impl PatternArgs {
    /// The options: the configuration, then `--set`, `--rule`,
    /// `--similarity`, `--no-merge`.
    fn options(self, lines: bool) -> Result<PatternOptions> {
        let mut config = match &self.pattern_config {
            Some(p) if p == Path::new("-") => PatternConfig::default(),
            Some(p) => {
                let text = std::fs::read_to_string(p).with_context(|| format!("reading {}", p.display()))?;
                PatternConfig::parse(&text).map_err(|e| anyhow::anyhow!("{}: {e}", p.display()))?
            }
            None => PatternConfig::load_saved().map_err(anyhow::Error::msg)?,
        };
        for s in &self.set {
            config.set(s).map_err(|e| anyhow::anyhow!("--set {s}: {e}"))?;
        }
        for r in &self.rules {
            config.add_rule(r).map_err(|e| anyhow::anyhow!("--rule {r}: {e}"))?;
        }
        if let Some(s) = self.similarity {
            config.set(&format!("similarity {s}")).map_err(|e| anyhow::anyhow!("--similarity: {e}"))?;
        }
        if self.no_merge {
            config.settings.merge = false;
        }
        Ok(PatternOptions { field: self.field, lines, config })
    }
}

/// `--json` for commands that print lines.
#[derive(clap::Args)]
struct JsonArgs {
    /// Print JSON Lines: an object per line, `{"file", "line", "time",
    /// "level", "raw"}` (time in UTC, RFC 3339; null where none).
    #[arg(long)]
    json: bool,
    /// With --json: also these fields, typed, as `"fields": {…}`
    /// (comma-separated; `nerka help fields FILE` lists them).
    #[arg(long, value_delimiter = ',', value_name = "FIELD,…", requires = "json")]
    fields: Vec<String>,
}

#[derive(clap::Args)]
struct OutputArgs {
    /// Only print the number of matching lines (and, for pack picks, the
    /// hits per query); -m and --skip don't change it.
    #[arg(short = 'c', long)]
    count: bool,
    /// Print at most N lines; how many more there are goes to stderr
    /// (with --json: a last `{"more": N, "skip": K}` line).
    #[arg(short = 'm', long = "max-count", value_name = "N")]
    max: Option<u64>,
    /// Leave out the first N lines (with -m: the next page).
    #[arg(long, value_name = "N", default_value_t = 0)]
    skip: u64,
    /// grep's -n: line numbers are printed anyway (accepted, nothing to do).
    #[arg(short = 'n', long = "line-number", hide = true)]
    _line_number: bool,
    #[command(flatten)]
    json: JsonArgs,
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
    /// Parameter value, escaped where it is used: NAME=VALUE. Given again for
    /// a name: another value, and each query using it an instance per value.
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

    /// Queries come before labels, each in command-line order. A parameter
    /// given several times has several values: each query using it is an
    /// instance per value.
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
        let mut values = pack::ParamValues::new();
        for (list, raw) in [(&self.params, false), (&self.raw_params, true)] {
            for kv in list {
                let (k, v) = kv.split_once('=').with_context(|| format!("expected NAME=VALUE, got '{kv}'"))?;
                values.entry(k.to_owned()).or_default().push(ParamValue { value: v.to_owned(), raw });
            }
        }
        Ok(pack::expand_values(&sel, lib, &values)?)
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
        let kind = Kind::parse(k)
            .with_context(|| format!("--only {o}: '{k}' isn't pack, preset, plot, check, signal or profile"))?;
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
        return Err(status1(format!("{} invalid item(s) not imported", plan.invalid())));
    }
    Ok(())
}

/// Compile an NFD query for a source, with a caret under the error.
fn compile(query: &str, source: &dyn LogSource) -> Result<Query> {
    // `--signal` definitions, and `@time` on files that know their time.
    let signals = signals_for(source);
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

use agent::locator;

/// Lines as `filter`, `search` print them: `-m` and `--skip` applied, as
/// text (`LINE:` or `FILE:LINE:` prefixes, as grep prints them) or JSON
/// Lines ([`agent::line`]).
struct Printer<'a> {
    o: &'a OutputArgs,
    view: &'a Opened,
    at: Box<dyn Fn(LineNo) -> (usize, LineNo) + 'a>,
    names: Vec<String>,
    fields: agent::Fields,
    /// Lines offered so far, printed or not.
    seen: u64,
    printed: u64,
    /// The last line printed (for `--` between windows apart).
    last: Option<LineNo>,
}

impl<'a> Printer<'a> {
    fn new(o: &'a OutputArgs, view: &'a Opened) -> Result<Self> {
        let f = &*view.source;
        let fields = agent::Fields::parse(&o.json.fields, f).map_err(|e| anyhow::anyhow!("--fields: {e}"))?;
        let names = view.paths.iter().map(|p| p.display().to_string()).collect();
        Ok(Printer { o, view, at: Box::new(locator(f)), names, fields, seen: 0, printed: 0, last: None })
    }

    /// Print `row` if it is past `--skip` and within `-m`. `mark`: whether
    /// it is a match (`filter --before / --after`); `queries`: the pack
    /// queries that matched it.
    fn line(&mut self, out: &mut impl Write, row: LineNo, mark: Option<bool>, queries: &[&str]) -> Result<()> {
        let i = self.seen;
        self.seen += 1;
        if i < self.o.skip || self.o.max.is_some_and(|m| self.printed >= m) {
            return Ok(());
        }
        self.printed += 1;
        let f = &*self.view.source;
        let (k, line) = (self.at)(row);
        if self.o.json.json {
            let mut v = agent::line(f, row, &self.names[k], line, &self.fields);
            if let Some(m) = mark {
                v.push("match", m);
            }
            if !queries.is_empty() {
                v.push("queries", queries.iter().map(|q| Value::from(*q)).collect::<Vec<_>>());
            }
            writeln!(out, "{v}")?;
        } else {
            if let Some(m) = mark {
                if self.last.is_some_and(|p| row > p + 1) {
                    out.write_all(b"--\n")?;
                }
                out.write_all(if m { b">" } else { b" " })?;
            }
            if self.view.paths.len() > 1 {
                write!(out, "{}:", self.names[k])?;
                if !self.o.no_line_numbers {
                    write!(out, "{}:", line + 1)?;
                }
            } else if !self.o.no_line_numbers {
                write!(out, "{}:", row + 1)?;
            }
            if !queries.is_empty() {
                write!(out, "[{}] ", queries.join(","))?;
            }
            out.write_all(&f.line_bytes(row).unwrap_or_default())?;
            out.write_all(b"\n")?;
        }
        self.last = Some(row);
        Ok(())
    }

    /// How many lines `-m` left out: `{"more": N, "skip": K}` last in
    /// JSON, a note on stderr in text.
    fn finish(&self, out: &mut impl Write) -> Result<()> {
        let next = self.o.skip + self.printed;
        let more = self.seen.saturating_sub(next);
        if more > 0 {
            if self.o.json.json {
                writeln!(out, "{}", Value::object([("more", Value::from(more)), ("skip", next.into())]))?;
            } else {
                out.flush()?;
                eprintln!("nerka: {more} more line{}: --skip {next} for the next", if more == 1 { "" } else { "s" });
            }
        }
        Ok(())
    }
}

/// `-c`: the count, as text or `{"count": N}` (pack picks: and per query).
fn write_count(out: &mut impl Write, o: &OutputArgs, n: u64, per_query: &[(&str, u64)]) -> Result<()> {
    if o.json.json {
        let mut v = Value::object([("count", Value::from(n))]);
        if !per_query.is_empty() {
            v.push("queries", Value::object(per_query.iter().map(|&(q, n)| (q, Value::from(n)))));
        }
        writeln!(out, "{v}")?;
    } else {
        writeln!(out, "{n}")?;
        for (q, n) in per_query {
            writeln!(out, "{q}\t{n}")?;
        }
    }
    Ok(())
}

/// `build` gets the opened source (its fields, its time), so query errors about
/// unknown fields point into the query like syntax errors do. Returns the
/// number of matches.
fn run_filter(
    files: &[PathBuf],
    build: impl FnOnce(&dyn LogSource) -> Result<Filter>,
    o: &OutputArgs,
    out: &mut impl Write,
) -> Result<u64> {
    let view = open(files)?;
    let (f, t_index) = (&*view.source, view.took);
    let filter = with_marks(build(f)?, &view)?;
    let mut p = Printer::new(o, &view)?;
    let t = Instant::now();
    let res = search(f, &filter, &Progress::new())?;
    let t_search = t.elapsed();
    if o.count {
        write_count(out, o, res.lines.len() as u64, &[])?;
    } else {
        for &l in &res.lines {
            p.line(out, l, None, &[])?;
        }
        p.finish(out)?;
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
    Ok(res.lines.len() as u64)
}

/// `filter --before / --after`: every line within `before` before or
/// `after` after a match, by time (`around(…)`), the matches marked `>`
/// and `--` between windows apart, as grep's context. Returns the number
/// of matches (not of the lines around them).
fn run_window(
    files: &[PathBuf],
    query: &str,
    before: &str,
    after: &str,
    o: &OutputArgs,
    out: &mut impl Write,
) -> Result<u64> {
    let view = open(files)?;
    let (f, t_index) = (&*view.source, view.took);
    // The query alone first: its errors point into it as typed.
    let matches = with_marks(Filter::Query(compile(query, f)?), &view)?;
    let around = with_marks(Filter::Query(compile(&format!("around({before}, {after}, {query})"), f)?), &view)?;
    let mut p = Printer::new(o, &view)?;
    let t = Instant::now();
    let hits = search(f, &matches, &Progress::new())?;
    let res = search(f, &around, &Progress::new())?;
    let t_search = t.elapsed();
    if o.count {
        write_count(out, o, res.lines.len() as u64, &[])?;
    } else {
        for &l in &res.lines {
            p.line(out, l, Some(hits.lines.binary_search(&l).is_ok()), &[])?;
        }
        p.finish(out)?;
    }
    if o.stats {
        out.flush()?;
        eprintln!(
            "{} lines around {} matches in {} lines | index {t_index:.2?} | search {t_search:.2?}",
            res.lines.len(),
            hits.lines.len(),
            f.line_count()
        );
    }
    Ok(hits.lines.len() as u64)
}

/// Search inside the lines of `within` (every line without it). `names`
/// are the include queries' names, printed per hit; empty for a pattern.
/// Returns the number of hits.
fn run_search_within(
    files: &[PathBuf],
    within: Option<&str>,
    set: SearchSet,
    names: &[String],
    o: &OutputArgs,
    out: &mut impl Write,
) -> Result<u64> {
    let view = open(files)?;
    let (f, t_index) = (&*view.source, view.took);
    let filter = within.map(|q| compile(q, f).and_then(|q| with_marks(Filter::Query(q), &view))).transpose()?;
    let set = SearchSet {
        include: set.include.into_iter().map(|q| with_marks(q, &view)).collect::<Result<_>>()?,
        exclude: set.exclude.into_iter().map(|q| with_marks(q, &view)).collect::<Result<_>>()?,
    };
    let mut p = Printer::new(o, &view)?;
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
        // A pattern has no queries to count by.
        let per: Vec<(&str, u64)> = if names.is_empty() {
            Vec::new()
        } else {
            names.iter().zip(hits.counts(names.len())).map(|(q, n)| (q.as_str(), n as u64)).collect()
        };
        write_count(out, o, hits.len() as u64, &per)?;
    } else {
        for (i, &l) in hits.lines.iter().enumerate() {
            let ids: Vec<&str> =
                if names.is_empty() { Vec::new() } else { hits.matched(i).map(|q| names[q].as_str()).collect() };
            p.line(out, l, None, &ids)?;
        }
        p.finish(out)?;
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
    Ok(hits.len() as u64)
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
    let signals = signals_for(&*f);
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
    let signals = signals_for(&*f);
    // Every check runs (one may use another's signal); the named one shows.
    let t = Instant::now();
    let run = all.run_all(&*f, &signals, &lib, &Progress::new())?;
    let took = t.elapsed();
    let (mut issues, mut rows) = (0, Vec::new());
    // Time goes backward for a check on times: one warning (owner, 2026-11-10).
    let mut back = false;
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
        back |= r.times_go_back;
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
                     \"verdict\": {verdict}, \"times_go_back\": {}, \"evidence\": [{}]}}",
                    js(&r.name),
                    js(r.kind.as_str()),
                    js(&s.scope.label),
                    s.scope.from + 1,
                    s.scope.to,
                    r.times_go_back,
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
    if back {
        eprintln!(
            "nerka: warning: time goes backward in this view: rate, gap and then / missing expect lines in time \
             order, so their results may be wrong{}",
            if MERGE.get().copied().unwrap_or(false) { "" } else { " (--merge puts several files in time order)" }
        );
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

/// `nerka compare`: two files, or one with a filter per set.
#[allow(clippy::too_many_arguments)]
fn compare(
    files: &[PathBuf],
    filters: [Option<String>; 2],
    within: Option<String>,
    options: PatternOptions,
    min_score: f64,
    top: usize,
    queries: bool,
    json: bool,
    stats: bool,
    out: &mut impl Write,
) -> Result<()> {
    if files.len() == 1 && filters.iter().any(Option::is_none) {
        anyhow::bail!("one file: give a filter per set (--a FILTER --b FILTER), or two files");
    }
    let views: Vec<Opened> = files.iter().map(|f| open(std::slice::from_ref(f))).collect::<Result<_>>()?;
    let view = |side: usize| &views[side.min(views.len() - 1)];
    // A side's lines: its filter and --in, both.
    let mut lines: [Option<Vec<LineNo>>; 2] = [None, None];
    for (side, filter) in filters.iter().enumerate() {
        let q = match (filter, &within) {
            (Some(f), Some(w)) => Some(format!("({w}) AND ({f})")),
            (Some(f), None) => Some(f.clone()),
            (None, w) => w.clone(),
        };
        if let Some(q) = q {
            let v = view(side);
            let f = &*v.source;
            lines[side] = Some(search(f, &with_marks(Filter::Query(compile(&q, f)?), v)?, &Progress::new())?.lines);
        }
    }
    let set = |side: usize| pattern::Set { source: &*view(side).source, lines: lines[side].as_deref() };
    let t = Instant::now();
    let ps = pattern::compare(set(0), set(1), &options, &Progress::new())?;
    let diffs = pattern::differences(&ps, min_score);
    let took = t.elapsed();
    let name = |side: usize| {
        let v = view(side);
        let file = v.paths.first().map(|p| p.display().to_string()).unwrap_or_default();
        match &filters[side] {
            Some(f) => format!("{file} where {f}"),
            None => file,
        }
    };
    let shown = if top == 0 { diffs.len() } else { top.min(diffs.len()) };
    if json {
        let names = [views[0].names(), view(1).names()];
        let av = agent::View { source: &*view(0).source, files: &names[0], compact: false };
        let bv = agent::View { source: &*view(1).source, files: &names[1], compact: false };
        let side = |k: usize| {
            Value::object([
                ("file", Value::from(view(k).paths.first().map(|p| p.display().to_string()))),
                ("filter", filters[k].clone().into()),
                ("lines", ps.lines[k].into()),
            ])
        };
        let v = agent::compare(&ps, &diffs, [&av, &bv], [side(0), side(1)], min_score, top, queries);
        writeln!(out, "{v}")?;
        if stats {
            out.flush()?;
            eprintln!(
                "{} patterns, {} differences | grouped and compared in {took:.2?}",
                ps.patterns.len(),
                diffs.len()
            );
        }
        return Ok(());
    }
    writeln!(out, "A: {} ({} lines)", name(0), ps.lines[0])?;
    writeln!(out, "B: {} ({} lines)", name(1), ps.lines[1])?;
    let mut section = None;
    for d in &diffs[..shown] {
        if section != Some(d.change()) {
            section = Some(d.change());
            let title = match d.change() {
                pattern::Change::New => "New in B".to_owned(),
                pattern::Change::Gone => "Gone from B".to_owned(),
                pattern::Change::Changed => format!("Changed (score {min_score} or more)"),
            };
            writeln!(
                out,
                "
{title}
{:>9} {:>8} {:>8} {:>7}  {:<9} pattern",
                "score", "A", "B", "change", "first"
            )?;
        }
        let p = &ps.patterns[d.pattern];
        let change = match d.counts {
            [0, _] => "new".to_owned(),
            [_, 0] => "gone".to_owned(),
            _ => format!("×{:.2}", d.ratio()),
        };
        // Where to look: the side it's on, or grew on.
        let side = match d.change() {
            pattern::Change::New => 1,
            pattern::Change::Gone => 0,
            pattern::Change::Changed => usize::from(d.ratio() >= 1.0),
        };
        let (sides, what) = match d.value {
            None => (&p.sides, p.template()),
            Some((s, v)) => {
                let value = &p.slots().nth(s).expect("a slot").values[v];
                (&value.sides, format!("{}  → slot {}: {}", p.template(), s + 1, value.value()))
            }
        };
        let first = format!("{}:{}", ["A", "B"][side], sides[side].first + 1);
        let fields: String = p.fields.iter().map(|(n, v)| format!("{n}={v} ")).collect();
        writeln!(out, "{:>9.1} {:>8} {:>8} {change:>7}  {first:<9} {fields}{what}", d.score, d.counts[0], d.counts[1])?;
        if queries {
            writeln!(out, "{:47}{}", "", ps.query(p))?;
        }
    }
    if shown < diffs.len() {
        writeln!(out, "… {} more (-n 0: all)", diffs.len() - shown)?;
    }
    if diffs.is_empty() {
        writeln!(out, "nothing new or gone, and no change scoring {min_score} or more")?;
    }
    if stats {
        out.flush()?;
        eprintln!("{} patterns, {} differences | grouped and compared in {took:.2?}", ps.patterns.len(), diffs.len());
        eprintln!("{}", engine_stats(&ps.stats));
    }
    Ok(())
}

impl Opened {
    /// Its files' names, for [`agent::View`].
    fn names(&self) -> Vec<String> {
        self.paths.iter().map(|p| p.display().to_string()).collect()
    }
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
    let mut options = OPEN_OPTIONS.get_or_init(Default::default).clone();
    // `--offset`s by the files as given, or their names.
    for (file, ns) in offsets_mut().iter() {
        let name = |p: &PathBuf| p.file_name().is_some_and(|n| n.to_string_lossy() == file.as_str());
        match paths
            .iter()
            .find(|p| p.as_os_str().to_string_lossy() == file.as_str())
            .or_else(|| paths.iter().find(|p| name(p)))
        {
            Some(p) => options.offset_for.push((p.clone(), *ns)),
            None => anyhow::bail!("--offset {file}=…: no file given is {file}"),
        }
    }
    let options = &options;
    let progress = Progress::new();
    // `-`: text from stdin.
    if paths.iter().any(|p| p == Path::new("-")) {
        if paths.len() > 1 {
            anyhow::bail!("- (stdin) is read alone, not in a view with other files");
        }
        let mut bytes = Vec::new();
        std::io::Read::read_to_end(&mut std::io::stdin(), &mut bytes).context("reading stdin")?;
        let source = nerka_core::open_text_bytes("-", bytes, options, &progress)?;
        return Ok(Opened { source, took: t.elapsed(), paths: paths.to_vec() });
    }
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

/// `--offset` (file as given, nanoseconds), read in `main`.
static OFFSETS: std::sync::OnceLock<std::sync::Mutex<Vec<(String, i64)>>> = std::sync::OnceLock::new();

fn offsets_mut() -> std::sync::MutexGuard<'static, Vec<(String, i64)>> {
    OFFSETS.get_or_init(Default::default).lock().unwrap_or_else(|e| e.into_inner())
}

/// The saved profiles with `--profile` definitions first; a `--profile
/// NAME` goes into `forced` (it must be one of them, or `default`).
fn profiles(given: &[String], forced: &mut Option<String>) -> Result<nerka_core::input::profile::Profiles> {
    use nerka_core::input::profile::{DEFAULT, Profile, Profiles};
    let (mut ps, errors) = Profiles::load_saved();
    for e in errors {
        eprintln!("nerka: profiles: {e}");
    }
    for spec in given.iter().rev() {
        if spec.contains(':') {
            let p = Profile::parse_spec(spec).map_err(|e| {
                anyhow::anyhow!("--profile: {}\n  {spec}\n  {}^", e.msg, " ".repeat(e.column(spec) - 1))
            })?;
            ps.put_first(p);
        }
    }
    for name in given.iter().filter(|s| !s.contains(':')) {
        let name = name.trim();
        if ps.get(name).is_none() && !name.eq_ignore_ascii_case(DEFAULT) {
            let known: Vec<&str> = ps.iter().map(|p| p.name()).collect();
            anyhow::bail!(
                "--profile {name}: no such profile (there are: {}); define one with --profile '{name}: CLAUSES'",
                if known.is_empty() { "only default".into() } else { known.join(", ") + ", default" }
            );
        }
        if forced.replace(name.to_owned()).is_some() {
            anyhow::bail!("--profile {name}: one profile can be forced, not several");
        }
    }
    Ok(ps)
}

/// The `--signal` definitions and `@time` for `source`, with the signals its
/// files' profiles switch on (a `--signal` of the same name wins).
fn signals_for(source: &dyn LogSource) -> Signals {
    let mut s = SIGNALS.get_or_init(Default::default).for_source(source);
    let options = OPEN_OPTIONS.get_or_init(Default::default);
    let from = options.profiles.signals_for(&source.info().reading);
    for c in &from.clashes {
        eprintln!("nerka: note: {c}");
    }
    for d in from.signals {
        match s.get(d.name()) {
            None => s.set(d),
            Some(_) => eprintln!(
                "nerka: note: @{} of profile {} isn't used: a signal of that name is given already",
                d.name(),
                d.profiles().join(", ")
            ),
        }
    }
    s
}

fn mib_per_s(bytes: u64, d: std::time::Duration) -> f64 {
    bytes as f64 / 1024.0 / 1024.0 / d.as_secs_f64().max(1e-9)
}

/// Exit status 1 (grep's "nothing found", owner 2026-11-24): no line
/// found, an issue with `--strict`, an invalid item on import; the message
/// (if any) goes to stderr. Every other error exits 2.
#[derive(Debug)]
struct Status1(String);

impl std::fmt::Display for Status1 {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Status1 {}

/// Exit with status 1 and `msg` on stderr (empty: nothing said).
fn status1(msg: impl Into<String>) -> anyhow::Error {
    Status1(msg.into()).into()
}

fn main() -> std::process::ExitCode {
    match run() {
        Ok(()) => std::process::ExitCode::SUCCESS,
        // `nerka search … | head` closes the pipe early: not an error.
        Err(e) if e.downcast_ref::<std::io::Error>().is_some_and(|e| e.kind() == std::io::ErrorKind::BrokenPipe) => {
            std::process::ExitCode::SUCCESS
        }
        Err(e) if e.downcast_ref::<Status1>().is_some() => {
            let msg = e.to_string();
            if !msg.is_empty() {
                eprintln!("nerka: {msg}");
            }
            std::process::ExitCode::from(1)
        }
        Err(e) => {
            eprintln!("nerka: {e:#}");
            std::process::ExitCode::from(2)
        }
    }
}

fn run() -> Result<()> {
    // Git Bash turns an argument like `start=/^x/` or `/timeout/` into a
    // Windows path before nerka sees it (the evaluation).
    if cfg!(windows) && std::env::args().skip(1).any(|a| a.contains(":/Program Files/Git/")) {
        eprintln!(
            "nerka: warning: Git Bash turned an argument with /…/ into a Windows path (…/Program Files/Git/…): \
             set MSYS_NO_PATHCONV=1 first"
        );
    }
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
    options.profiles = std::sync::Arc::new(profiles(&cli.profile, &mut options.profile)?);
    for o in &cli.offset {
        let parsed = (o.rsplit_once('='))
            .filter(|(f, _)| !f.trim().is_empty())
            .and_then(|(f, d)| Some((f, nerka_core::time::parse_offset(d)?)));
        let Some((file, d)) = parsed else {
            anyhow::bail!("--offset {o}: expected FILE=OFFSET, e.g. 'ecu1.dlt=+3h' or 'app.log=-0.250s'");
        };
        offsets_mut().push((file.to_owned(), (d * 1e9).round() as i64));
    }
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

    // Filter and search: whether a line was found (exit status 1 if not).
    let mut found = true;
    match cli.cmd {
        Cmd::Help { topic } => help(&topic, &mut out)?,
        Cmd::Mcp { window, config, install } => {
            let exe = std::env::current_exe().context("finding nerka's own path")?;
            let exe = exe.display().to_string();
            if config {
                let server = Value::object([("command", Value::from(exe.as_str())), ("args", vec!["mcp"].into())]);
                let v = Value::object([("mcpServers", Value::object([("nerka", server)]))]);
                writeln!(out, "{v}")?;
            } else if let Some(client) = install {
                let args: Vec<&str> = match client.to_ascii_lowercase().as_str() {
                    "claude" => vec!["mcp", "add", "--scope", "user", "nerka", "--", &exe, "mcp"],
                    "codex" => vec!["mcp", "add", "nerka", "--", &exe, "mcp"],
                    "gemini" => vec!["mcp", "add", "--scope", "user", "nerka", &exe, "mcp"],
                    other => anyhow::bail!("--install {other}: claude, codex or gemini (others: nerka mcp --config)"),
                };
                let client = client.to_ascii_lowercase();
                eprintln!("nerka: running {client} {}", args.join(" "));
                let status = std::process::Command::new(&client)
                    .args(&args)
                    .status()
                    .with_context(|| format!("{client} isn't on PATH; run it yourself: {client} {}", args.join(" ")))?;
                if !status.success() {
                    anyhow::bail!("{client} mcp add failed ({status})");
                }
            } else {
                out.flush()?;
                drop(out);
                return mcp::serve(window);
            }
        }
        Cmd::Open { files, json: true } => {
            let view = open_silent(&files)?;
            let names: Vec<String> = view.paths.iter().map(|p| p.display().to_string()).collect();
            let v = agent::summary(&*view.source, &names, view.took.as_secs_f64(), &Progress::new())?;
            writeln!(out, "{v}")?;
        }
        Cmd::Open { files, json: false } => {
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
            // How each text file was read, and each file's time.
            for r in &info.reading {
                let file = if r.file.is_empty() { String::new() } else { format!("{}: ", r.file) };
                let profile =
                    if r.profile.is_empty() { String::new() } else { format!("profile {} ({}), ", r.profile, r.why) };
                let moved = if r.offset == 0.0 {
                    String::new()
                } else {
                    format!(", moved {}", nerka_core::time::format_offset(r.offset))
                };
                writeln!(out, "time:        {file}{profile}{}{moved}", r.time)?;
            }
            for n in &info.notes {
                writeln!(out, "note:        {n}")?;
            }
            for h in agent::hints(f) {
                writeln!(out, "hint:        {h}")?;
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
                        None => found = run_filter(&files, |_| Ok(Filter::Regex(m)), &o, &mut out)? > 0,
                        Some(w) => {
                            let set = SearchSet { include: vec![Filter::Regex(m)], exclude: vec![] };
                            found = run_search_within(&files, Some(&w), set, &[], &o, &mut out)? > 0;
                        }
                    }
                }
                (None, false) => {
                    if ignore_case || fixed || invert {
                        anyhow::bail!("-i, -F and -v apply to a PATTERN, not to pack picks");
                    }
                    let e = picks.expand(&library(&cli.pack)?)?;
                    let names: Vec<String> = e.parts_with(Role::Include).map(|p| p.query.clone()).collect();
                    found = run_search_within(&files, within.as_deref(), e.search_set()?, &names, &o, &mut out)? > 0;
                }
            }
        }
        Cmd::Filter { query, files, before, after, out: o } => {
            if before.is_none() && after.is_none() {
                found = run_filter(&files, |source| Ok(Filter::Query(compile(&query, source)?)), &o, &mut out)? > 0;
            } else {
                let dur = |d: &Option<String>, what: &str| -> Result<String> {
                    match d {
                        None => Ok("0s".into()),
                        Some(t) if nerka_core::lifecycle::parse_duration(t).is_some() => Ok(t.trim().to_owned()),
                        Some(t) => Err(anyhow::anyhow!("--{what}: '{t}' isn't a duration (30s, 500ms, 2m)")),
                    }
                };
                let (b, a) = (dur(&before, "before")?, dur(&after, "after")?);
                found = run_window(&files, &query, &b, &a, &o, &mut out)? > 0;
            }
        }
        Cmd::Patterns { files, within, engine, top, rare, values, queries, lines: show, json, stats } => {
            let options = engine.options(show.is_some())?;
            let view = open(&files)?;
            let f = &*view.source;
            let lines = match &within {
                Some(w) => Some(search(f, &with_marks(Filter::Query(compile(w, f)?), &view)?, &Progress::new())?.lines),
                None => None,
            };
            let t = Instant::now();
            let mut ps = nerka_core::pattern::detect(f, lines.as_deref(), &options, &Progress::new())?;
            let took = t.elapsed();
            if rare {
                ps.patterns.sort_by(|a, b| a.count().cmp(&b.count()).then(a.first().cmp(&b.first())));
            }
            let at = locator(f);
            let shown = if top == 0 { ps.patterns.len() } else { top.min(ps.patterns.len()) };
            if json {
                let names = view.names();
                let v = agent::View { source: f, files: &names, compact: false };
                let detail = agent::PatternDetail { values, queries, lines: show };
                writeln!(out, "{}", agent::patterns(&ps, &v, top, detail))?;
            }
            let place = |l: LineNo| {
                if view.paths.len() > 1 {
                    let (k, l) = at(l);
                    format!("{}:{}", view.paths[k].display(), l + 1)
                } else {
                    (l + 1).to_string()
                }
            };
            for p in ps.patterns[..shown].iter().filter(|_| !json) {
                let first = place(p.first());
                let share = 100.0 * p.count() as f64 / ps.total().max(1) as f64;
                write!(out, "{:>9} {share:>5.1}%  {first:>7}  ", p.count())?;
                for (name, value) in &p.fields {
                    write!(out, "{name}={value} ")?;
                }
                let unsorted = if p.unsorted { "  (not sorted: too many kinds of lines)" } else { "" };
                writeln!(out, "{}{unsorted}", p.template())?;
                if values {
                    for (k, s) in p.slots().enumerate() {
                        let kind = if s.is_state() { "state" } else { "values" };
                        let more = if s.overflow { ", more not kept" } else { "" };
                        writeln!(out, "{:28}slot {} ({kind}{more}):", "", k + 1)?;
                        for v in &s.values {
                            let lines = if v.first() == v.last() {
                                place(v.first())
                            } else {
                                format!("{}–{}", place(v.first()), place(v.last()))
                            };
                            writeln!(out, "{:30}{:>9}  {:<24} {lines}", "", v.count(), v.value())?;
                        }
                    }
                }
                if queries {
                    writeln!(out, "{:28}{}", "", ps.query(p))?;
                }
                if let Some(n) = show {
                    let n = if n == 0 { usize::MAX } else { n };
                    for &l in p.lines[0].iter().take(n) {
                        let text = f.line_text(l).unwrap_or_default();
                        writeln!(out, "{:28}{:>9}: {text}", "", place(l))?;
                    }
                    if p.lines[0].len() > n {
                        writeln!(out, "{:28}… {} more lines", "", p.lines[0].len() - n)?;
                    }
                }
            }
            if shown < ps.patterns.len() && !json {
                writeln!(out, "… {} more (-n 0: all)", ps.patterns.len() - shown)?;
            }
            if stats {
                out.flush()?;
                eprintln!(
                    "{} patterns in {} lines | index {:.2?} | patterns {took:.2?}",
                    ps.patterns.len(),
                    ps.total(),
                    view.took
                );
                eprintln!("{}", engine_stats(&ps.stats));
            }
        }
        Cmd::Anonymize { files, output, within, keep, seed, ids } => {
            let same = |p: &Path| {
                let c = |x: &Path| std::fs::canonicalize(x).unwrap_or_else(|_| x.to_owned());
                c(p) == c(&output)
            };
            if files.iter().any(|f| same(f)) {
                anyhow::bail!("{}: the output can't be one of the files read", output.display());
            }
            let view = open(&files)?;
            let f = &*view.source;
            let lines = match &within {
                Some(w) => Some(search(f, &with_marks(Filter::Query(compile(w, f)?), &view)?, &Progress::new())?.lines),
                None => None,
            };
            let n = lines.as_ref().map_or(f.line_count(), |l| l.len() as u64);
            let file = std::fs::File::create(&output).with_context(|| format!("creating {}", output.display()))?;
            let mut w = BufWriter::new(file);
            let mut a = nerka_core::anonymize::Anonymizer::new(seed.as_deref(), keep);
            let what = nerka_core::anonymize::anonymize(f, lines.as_deref(), &mut a, ids, &mut w, &Progress::new())?;
            w.flush().with_context(|| format!("writing {}", output.display()))?;
            let what = match what {
                nerka_core::anonymize::Written::Dlt => "DLT",
                nerka_core::anonymize::Written::Text => "text",
            };
            let key = if seed.is_none() { "; keyed at random: the words can't be mapped back" } else { "" };
            eprintln!("nerka: {n} lines anonymized into {} ({what}){key}", output.display());
        }
        Cmd::Compare { files, a, b, within, engine, min_score, top, queries, json, stats } => {
            compare(&files, [a, b], within, engine.options(false)?, min_score, top, queries, json, stats, &mut out)?;
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
                    // An instance: its own arguments.
                    let args = if p.args.is_empty() {
                        String::new()
                    } else {
                        let a: Vec<String> = p.args.iter().map(|(k, v)| format!("{k}={}", v.value)).collect();
                        format!("({})", a.join(", "))
                    };
                    writeln!(out, "{role} {}{args}{note}: {}", p.query, e.part_nfd(p))?;
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
        Cmd::Marks { file, json, add, note, name, by, remove } => {
            if file == Path::new("-") {
                anyhow::bail!("bookmarks are kept next to a file, not for stdin");
            }
            let f = open(std::slice::from_ref(&file))?.source;
            let mut marks = Bookmarks::load_for(&file, &*f)?;
            let row = |line: u64, what: &str| -> Result<LineNo> {
                if line == 0 || line > f.line_count() {
                    anyhow::bail!("--{what} {line}: the file has lines 1 to {}", f.line_count());
                }
                Ok(line - 1)
            };
            let shown: Vec<LineNo> = match (add, remove) {
                (Some(line), _) => {
                    let l = row(line, "add")?;
                    if !marks.contains(l) {
                        marks.toggle(l);
                        marks.set_author(l, by.as_deref().unwrap_or("ai"));
                    } else if let Some(by) = &by {
                        marks.set_author(l, by);
                    }
                    if let Some(n) = note {
                        marks.set_note(l, n);
                    }
                    if let Some(n) = name {
                        if marks.iter_named().any(|(o, x, _)| o != l && x.eq_ignore_ascii_case(n.trim())) {
                            anyhow::bail!("--name {n}: another bookmark is called so");
                        }
                        marks.set_name(l, n);
                    }
                    marks.save_for(&file, &*f).context("saving the bookmarks")?;
                    vec![l]
                }
                (None, Some(line)) => {
                    let l = row(line, "remove")?;
                    if !marks.remove(l) {
                        anyhow::bail!("--remove {line}: no bookmark on that line");
                    }
                    marks.save_for(&file, &*f).context("saving the bookmarks")?;
                    Vec::new()
                }
                (None, None) => marks.iter_named().map(|(l, _, _)| l).collect(),
            };
            for l in shown {
                let (name, note) = (marks.name(l).unwrap_or_default(), marks.note(l).unwrap_or_default());
                let text = f.line_text(l).unwrap_or_default();
                if json {
                    let mut v = Value::new_object();
                    v.push("file", file.display().to_string())
                        .push("line", l + 1)
                        .push("name", name)
                        .push("note", note)
                        .push("author", marks.author(l))
                        .push("unsure", marks.is_unsure(l))
                        .push("raw", text.into_owned());
                    writeln!(out, "{v}")?;
                    continue;
                }
                // Its anchor didn't find its line: it may be elsewhere.
                let unsure = if marks.is_unsure(l) { "?" } else { "" };
                let by = marks.author(l).map(|a| format!(" ({a})")).unwrap_or_default();
                let name = format!("{name}{unsure}{by}");
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
                return Err(status1(format!("{issues} issue{} found", if issues == 1 { "" } else { "s" })));
            }
        }
        Cmd::Lifecycles { files, name, json, strict, count } => {
            let found = lifecycles(&files, name.as_deref(), count.as_deref(), json, &mut out)?;
            if strict && found > 0 {
                out.flush()?;
                return Err(status1(format!("{found} finding{}", if found == 1 { "" } else { "s" })));
            }
        }
        Cmd::Lines { file, from, to, json } => {
            if from > to {
                anyhow::bail!("lines {from} to {to}: the first line is after the last");
            }
            let f = open(std::slice::from_ref(&file))?.source;
            let fields = agent::Fields::parse(&json.fields, &*f).map_err(|e| anyhow::anyhow!("--fields: {e}"))?;
            let name = file.display().to_string();
            let last = to.min(f.line_count());
            for l in from.max(1)..=last {
                if json.json {
                    writeln!(out, "{}", agent::line(&*f, l - 1, &name, l - 1, &fields))?;
                } else {
                    out.write_all(&f.line_bytes(l - 1).unwrap_or_default())?;
                    out.write_all(b"\n")?;
                }
            }
        }
    }
    out.flush()?;
    if !found {
        return Err(status1(""));
    }
    Ok(())
}

/// Examples of NFD per kind of file, after the grammar (`nerka help nfd`).
const NFD_EXAMPLES: &str = r#"
Examples
  text      level >= ERROR AND raw ~ /timeout|refused/i
            "disk full" OR /out of memory/        (a string or a regex alone: raw)
  JSON      level >= WARN AND user == "bob"       (a record's keys are fields)
  DLT       apid == NAV AND ctid == MAIN AND level >= WARN
            ecu == ECU1 AND payload ~ /gps (lost|weak)/
  captures  proto == SOMEIP AND type == ERROR     (--someip-port PORT)
  signals   @speed > 100                          (--signal 'speed=/speed=(?<speed>\d+)/')
  time      @time >= "2026-11-07 10:00:01"
  windows   before(30s, level >= ERROR)           around(10s, 2s, /crash/)
  marks     bookmark OR level >= ERROR            (the file's .nerka-marks)

A file's fields: nerka help fields FILE. More: docs/user/queries.md.
"#;

/// `nerka help agent`: how an AI agent (or a person) analyses logs with
/// nerka, in one read (`docs/dev/ai.md`, workflow A).
const AGENT_GUIDE: &str = include_str!("agent_guide.md");

/// `nerka help skill`: the guide as a skill file (`SKILL.md`) for AI
/// clients that read skills.
const SKILL_HEAD: &str = "---
name: nerka
description: Analyse logs and traces (text, JSON Lines, DLT, pcap, pcapng, MF4) with the nerka command line - what a file holds, filters, patterns, comparisons, lifecycles, checks - and leave the findings as bookmarks the user sees in nerka-gui. Use for root-cause analysis of a log or trace.
---

";

/// `nerka help [TOPIC]`: the commands, a command, `nfd`, `agent`, `skill`
/// or `fields FILE…`.
fn help(topic: &[String], out: &mut impl Write) -> Result<()> {
    let mut cmd = Cli::command();
    // Subcommands' usage says `nerka filter`, not `filter`.
    cmd.build();
    match topic {
        [] => write!(out, "{}", cmd.render_long_help())?,
        [t] if t.eq_ignore_ascii_case("agent") => write!(out, "{AGENT_GUIDE}")?,
        [t] if t.eq_ignore_ascii_case("skill") => write!(out, "{SKILL_HEAD}{AGENT_GUIDE}")?,
        [t] if t.eq_ignore_ascii_case("nfd") => {
            writeln!(out, "NFD, the query language of nerka:\n")?;
            write!(out, "{}{NFD_EXAMPLES}", nerka_core::dsl::GRAMMAR)?;
        }
        [t, files @ ..] if t.eq_ignore_ascii_case("fields") => {
            if files.is_empty() {
                anyhow::bail!("help fields FILE…: give the files");
            }
            let paths: Vec<PathBuf> = files.iter().map(PathBuf::from).collect();
            let f = open(&paths)?.source;
            writeln!(out, "field\tkind\tcostly")?;
            writeln!(out, "raw\ttext\tno\t(the line as shown)")?;
            writeln!(out, "level\tlevel\tno\t(TRACE DEBUG INFO NOTICE WARN ERROR FATAL)")?;
            for d in f.schema() {
                let costly = if d.costly { "yes" } else { "no" };
                writeln!(out, "{}\t{}\t{costly}", d.name, agent::kind_name(d.kind))?;
            }
            if f.has_time() {
                writeln!(out, "@time\ttime\tno\t(a signal: the line's time)")?;
            }
        }
        [t] => match cmd.find_subcommand_mut(t) {
            Some(sub) => write!(out, "{}", sub.render_long_help())?,
            None => {
                anyhow::bail!("no help on '{t}': agent, skill, nfd, fields FILE, or a command (nerka help lists them)")
            }
        },
        _ => anyhow::bail!("help takes one topic: agent, skill, nfd, fields FILE…, or a command"),
    }
    Ok(())
}
