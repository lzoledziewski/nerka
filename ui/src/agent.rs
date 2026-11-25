//! An agent's calls on this window (`nerka mcp`; `docs/dev/ai.md`,
//! workflow B): the agent reads what the user has open and changes it, and
//! the user sees each change and can undo it (owner, 2026-11-24: direct,
//! listed, undoable).
//!
//! Reading tools run on a thread of their own with a snapshot of the tab
//! (its source, signals, bookmarks and the filter's result), so a filter
//! over a big trace doesn't stop the window; the answers are built in core
//! (`nerka_core::agent`), as `nerka … --json` builds them. Changing tools
//! (`show`, `set_filter`, `set_search`, `bookmark`, `plot`) run on the
//! window's own thread and are listed in 🤖 Agent with ↶ Undo.

use crate::instance::AgentCall;
use crate::{App, Mode, plot};
use eframe::egui;
use nerka_core::agent::{self as ca, Fields, View};
use nerka_core::input::records::RecordSplit;
use nerka_core::json::Value;
use nerka_core::pattern::{self, PatternConfig, PatternOptions};
use nerka_core::{Bookmarks, Filter, LineNo, LogSource, Progress, Query, SearchResult, Signals, search};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

type Reply = mpsc::Sender<Result<Value, String>>;
/// Where a screenshot lands once drawn.
type Slot = Arc<Mutex<Option<egui::ColorImage>>>;

/// What agents did in this window.
#[derive(Default)]
pub struct AgentState {
    /// Newest last.
    pub log: Vec<Activity>,
    /// The 🤖 Agent window is open.
    pub open: bool,
    /// Screenshots asked for, answered once the picture is there.
    shots: Vec<(Slot, Reply, Instant)>,
}

/// One change an agent made.
pub struct Activity {
    pub when: String,
    pub tab: String,
    pub what: String,
    pub undo: Option<Undo>,
    pub undone: bool,
}

/// How to take a change back.
pub enum Undo {
    Filter {
        paths: Vec<PathBuf>,
        mode: Mode,
        pattern: String,
    },
    Search {
        paths: Vec<PathBuf>,
        mode: Mode,
        pattern: String,
    },
    /// The bookmark on `row` as it was: none, or (name, note, author).
    Bookmark {
        paths: Vec<PathBuf>,
        row: LineNo,
        before: Option<(String, String, String)>,
    },
    Plot {
        paths: Vec<PathBuf>,
        plotted: Vec<String>,
        shown: bool,
    },
    /// How the tab's files were read: their record splits.
    Records {
        paths: Vec<PathBuf>,
        before: Vec<(PathBuf, RecordSplit)>,
    },
}

/// Answers are bounded (an agent's context is small); the owner
/// (2026-11-25): settable, Settings → AI agents.
#[derive(Debug, Clone, Copy)]
pub struct Limits {
    /// Lines an answer gives when the agent doesn't say (`max`).
    pub lines: u64,
    /// Lines an answer gives at most, whatever the agent asks.
    pub most: u64,
    /// Patterns and differences an answer gives when the agent doesn't say.
    pub top: u64,
}

impl Default for Limits {
    fn default() -> Self {
        Limits { lines: 50, most: 1000, top: 30 }
    }
}

/// Each tool's arguments: any other is an error, not ignored (evaluation
/// round 1: `limit` and `query` were taken for `max` and `in`, silently).
const ARGS: &[(&str, &[&str])] = &[
    ("window", &[]),
    ("summary", &["tab"]),
    ("help", &["tab"]),
    ("count", &["tab", "query", "in_view"]),
    ("filter", &["tab", "query", "max", "skip", "fields", "before", "after", "in_view", "width"]),
    ("lines", &["tab", "file", "line", "before", "after", "from", "to", "fields", "width"]),
    ("bookmarks", &["tab"]),
    ("queries", &["tab", "counts", "library", "label"]),
    ("patterns", &["tab", "in", "rare", "top", "values", "queries", "lines", "in_view", "field"]),
    ("compare", &["tab", "a", "b", "in", "top", "min_score", "show", "rows", "queries", "in_view", "field"]),
    ("signals", &["tab"]),
    ("signal", &["tab", "name", "file", "from", "to", "points"]),
    ("lifecycles", &["tab", "name"]),
    ("show", &["tab", "file", "line"]),
    ("set_filter", &["tab", "query"]),
    ("set_search", &["tab", "query"]),
    ("set_records", &["tab", "records"]),
    ("bookmark", &["tab", "file", "line", "note", "name", "by"]),
    ("plot", &["tab", "signals", "only"]),
    ("screenshot", &["of"]),
];

/// An argument the tool doesn't take, said with the ones it does.
fn check_args(tool: &str, args: &Value) -> Result<(), String> {
    let Some((_, known)) = ARGS.iter().find(|(t, _)| *t == tool) else {
        let all: Vec<&str> = ARGS.iter().map(|(t, _)| *t).collect();
        return Err(format!("no tool '{tool}' (tools: {})", all.join(", ")));
    };
    match args {
        Value::Object(pairs) => match pairs.iter().find(|(k, _)| !known.contains(&k.as_str())) {
            Some((k, _)) if known.is_empty() => Err(format!("{tool} takes no arguments, not '{k}'")),
            Some((k, _)) => Err(format!("{tool} has no argument '{k}': it takes {}", known.join(", "))),
            None => Ok(()),
        },
        Value::Null => Ok(()),
        _ => Err(format!("{tool}'s arguments are an object of named values")),
    }
}

/// A tab as a reading tool sees it, taken on the window's thread.
struct Snap {
    source: Arc<dyn LogSource>,
    files: Vec<String>,
    title: String,
    signals: Signals,
    bookmarks: Arc<Bookmarks>,
    /// The lines the tab shows (its filter's result; none: all).
    view: Option<Arc<SearchResult>>,
    filter: String,
    search: String,
    plotted: Vec<String>,
    limits: Limits,
}

impl Snap {
    fn view(&self) -> View<'_> {
        View { source: &*self.source, files: &self.files, compact: true, width: ca::WIDTH }
    }

    /// `nfd` compiled for the tab: its signals and bookmarks; what can't
    /// match goes to `warnings`. An error points into the query.
    fn compile(&self, nfd: &str, warnings: &mut Vec<String>) -> Result<Filter, String> {
        let q = Query::compile_in(nfd, self.source.schema(), &self.signals)
            .map_err(|e| format!("{}\n  {nfd}\n  {}", e.at(nfd), e.caret(nfd)))?
            .with_bookmarks(&self.bookmarks);
        warnings.extend(nerka_core::query_warnings(&q, &*self.source));
        Ok(Filter::Query(q))
    }

    /// The rows `nfd` keeps (none: every row), only among the tab's view
    /// with `in_view`.
    fn rows(&self, nfd: Option<&str>, in_view: bool, warnings: &mut Vec<String>) -> Result<Vec<LineNo>, String> {
        let mut rows = match nfd.map(str::trim).filter(|q| !q.is_empty()) {
            Some(q) => {
                search(&*self.source, &self.compile(q, warnings)?, &Progress::new()).map_err(|e| e.to_string())?.lines
            }
            None => (0..self.source.line_count()).collect(),
        };
        if in_view && let Some(v) = &self.view {
            rows = intersect(&rows, &v.lines);
        }
        Ok(rows)
    }

    /// The row of `line` (from 1) in `file` (a name, or part of one;
    /// none: the first file).
    fn row(&self, args: &Value, key: &str) -> Result<LineNo, String> {
        let line = args.get(key).and_then(Value::as_u64).ok_or(format!("give \"{key}\": a line number, from 1"))?;
        let k = match args.get("file").and_then(Value::as_str) {
            None => 0,
            Some(f) => self
                .files
                .iter()
                .position(|n| n.ends_with(f) || n.contains(f))
                .ok_or(format!("no file '{f}' in this tab: {}", self.files.join(", ")))?,
        };
        let (inner, row_of) = match self.source.reordered() {
            Some(r) => (r.inner, Some(r.row_of)),
            None => (&*self.source, None),
        };
        let (start, count) = inner.part_files().get(k).copied().unwrap_or((0, inner.line_count()));
        if line == 0 || line > count {
            return Err(format!("{key} {line}: the file has lines 1 to {count}"));
        }
        let l = start + line - 1;
        Ok(row_of.map_or(l, |m| m[l as usize]))
    }
}

/// Rows in both sorted lists.
fn intersect(a: &[LineNo], b: &[LineNo]) -> Vec<LineNo> {
    let (mut i, mut j, mut out) = (0, 0, Vec::new());
    while i < a.len() && j < b.len() {
        match a[i].cmp(&b[j]) {
            std::cmp::Ordering::Less => i += 1,
            std::cmp::Ordering::Greater => j += 1,
            std::cmp::Ordering::Equal => {
                out.push(a[i]);
                i += 1;
                j += 1;
            }
        }
    }
    out
}

fn str_arg<'a>(args: &'a Value, key: &str) -> Option<&'a str> {
    args.get(key).and_then(Value::as_str)
}

fn bool_arg(args: &Value, key: &str, default: bool) -> bool {
    args.get(key).and_then(Value::as_bool).unwrap_or(default)
}

fn num_arg(args: &Value, key: &str, default: u64) -> u64 {
    args.get(key).and_then(Value::as_u64).unwrap_or(default)
}

/// `{…, "warnings": […]}` when there are any.
fn with_warnings(mut v: Value, warnings: Vec<String>) -> Value {
    if !warnings.is_empty() {
        v.push("warnings", warnings);
    }
    v
}

/// A reading tool's answer, on its own thread.
fn read(tool: &str, args: &Value, s: &Snap) -> Result<Value, String> {
    let progress = Progress::new();
    let mut warnings = Vec::new();
    let in_view = bool_arg(args, "in_view", false);
    let mut view = s.view();
    view.width = num_arg(args, "width", ca::WIDTH as u64) as usize;
    match tool {
        "summary" => {
            let mut v = ca::summary(&*s.source, &s.files, 0.0, &progress).map_err(|e| e.to_string())?;
            v.push("tab", s.title.as_str())
                .push("filter", s.filter.as_str())
                .push("view_lines", s.view.as_ref().map(|r| r.lines.len()))
                .push("search", s.search.as_str());
            Ok(v)
        }
        "help" => {
            let fields = s.source.schema().iter().map(|d| {
                Value::object([
                    ("name", Value::from(d.name.as_ref())),
                    ("kind", ca::kind_name(d.kind).into()),
                    ("costly", d.costly.into()),
                ])
            });
            // Where each comes from: the user's saved signals are for every
            // file, and may have nothing to do with this one.
            let signals = s.signals.iter().map(|d| {
                let origin = if d.is_from_source() {
                    "the file's own"
                } else if d.argument_index().is_some() {
                    "FIBEX"
                } else if d.as_lifecycle().is_some() {
                    "a lifecycle"
                } else if !d.queries().is_empty() {
                    "a library query the user picked"
                } else {
                    "saved by the user, for every file: may not match this one"
                };
                Value::object([
                    ("name", Value::from(d.name())),
                    ("kind", d.kind().as_str().into()),
                    ("from", origin.into()),
                ])
            });
            let mut v = Value::object([("grammar", Value::from(nerka_core::dsl::GRAMMAR))]);
            v.push("always", "raw (the line as shown), level (TRACE DEBUG INFO NOTICE WARN ERROR FATAL), bookmark")
                .push("fields", Value::Array(fields.collect()))
                .push("signals", Value::Array(signals.collect()));
            Ok(v)
        }
        "count" => {
            let rows = s.rows(str_arg(args, "query"), in_view, &mut warnings)?;
            let v = Value::object([("count", Value::from(rows.len())), ("lines", s.source.line_count().into())]);
            Ok(with_warnings(v, warnings))
        }
        "filter" => {
            let q = str_arg(args, "query");
            let fields = Fields::parse(&field_list(args), &*s.source)?;
            let (skip, max) = (num_arg(args, "skip", 0), num_arg(args, "max", s.limits.lines).min(s.limits.most));
            let (before, after) = (str_arg(args, "before"), str_arg(args, "after"));
            if before.is_some() || after.is_some() {
                let q = q.filter(|q| !q.trim().is_empty()).ok_or("before / after need a query")?;
                let hits = s.rows(Some(q), in_view, &mut warnings)?;
                let around = format!("around({}, {}, {q})", before.unwrap_or("0s"), after.unwrap_or("0s"));
                let rows = s.rows(Some(&around), false, &mut warnings)?;
                let matched = |r: LineNo| hits.binary_search(&r).is_ok();
                let mut v = view.page(&rows, skip, max, &fields, Some(&matched));
                v.push("matches", hits.len());
                return Ok(with_warnings(v, warnings));
            }
            let rows = s.rows(q, in_view, &mut warnings)?;
            Ok(with_warnings(view.page(&rows, skip, max, &fields, None), warnings))
        }
        "lines" => {
            let fields = Fields::parse(&field_list(args), &*s.source)?;
            let n = s.source.line_count();
            let (from, to) = if args.get("from").is_some() {
                (s.row(args, "from")?, s.row(args, "to").unwrap_or(n.saturating_sub(1)))
            } else {
                let at = s.row(args, "line")?;
                (
                    at.saturating_sub(num_arg(args, "before", 10)),
                    (at + num_arg(args, "after", 10)).min(n.saturating_sub(1)),
                )
            };
            // A range stays in its file (it ran on into
            // the tab's next file).
            let at = ca::locator(&*s.source);
            let file = at(from).0;
            let rows: Vec<LineNo> =
                (from.min(to)..=to.max(from)).filter(|&r| at(r).0 == file).take(s.limits.most as usize).collect();
            Ok(view.page(&rows, 0, s.limits.most, &fields, None))
        }
        "bookmarks" => {
            let list = s.bookmarks.iter_named().map(|(l, name, note)| {
                let mut v = view.place(l);
                v.push("name", name)
                    .push("note", note)
                    .push("author", s.bookmarks.author(l))
                    .push("raw", s.source.line_text(l).map(|t| t.into_owned()));
                v
            });
            Ok(Value::object([("bookmarks", Value::Array(list.collect()))]))
        }
        "patterns" => {
            let rows = match str_arg(args, "in") {
                Some(q) => Some(s.rows(Some(q), in_view, &mut warnings)?),
                None if in_view => s.view.as_ref().map(|r| r.lines.clone()),
                None => None,
            };
            let lines = args.get("lines").and_then(Value::as_u64).map(|n| n as usize);
            let options = pattern_options(args, lines.is_some())?;
            let mut ps =
                pattern::detect(&*s.source, rows.as_deref(), &options, &progress).map_err(|e| e.to_string())?;
            if bool_arg(args, "rare", false) {
                ps.patterns.sort_by(|a, b| a.count().cmp(&b.count()).then(a.first().cmp(&b.first())));
            }
            let detail = ca::PatternDetail {
                values: bool_arg(args, "values", false),
                queries: bool_arg(args, "queries", false),
                lines,
            };
            let top = num_arg(args, "top", s.limits.top) as usize;
            Ok(with_warnings(ca::patterns(&ps, &view, top, detail), warnings))
        }
        "compare" => {
            let side = |key: &str, warnings: &mut Vec<String>| -> Result<(String, Vec<LineNo>), String> {
                let f =
                    str_arg(args, key).ok_or(format!("give \"{key}\": an NFD query for set {}", key.to_uppercase()))?;
                let q = match str_arg(args, "in") {
                    Some(w) => format!("({w}) AND ({f})"),
                    None => f.to_owned(),
                };
                Ok((f.to_owned(), s.rows(Some(&q), in_view, warnings)?))
            };
            let (fa, a) = side("a", &mut warnings)?;
            let (fb, b) = side("b", &mut warnings)?;
            let options = pattern_options(args, false)?;
            let set = |lines| pattern::Set { source: &*s.source, lines: Some(lines) };
            let ps = pattern::compare(set(&a), set(&b), &options, &progress).map_err(|e| e.to_string())?;
            let listed = pattern::Listed {
                min_score: args.get("min_score").and_then(Value::as_f64).unwrap_or(pattern::MIN_SCORE),
                changes: match str_arg(args, "show") {
                    Some(t) => pattern::Listed::parse_changes(t).map_err(|e| format!("show: {e}"))?,
                    None => pattern::Listed::default().changes,
                },
                rows: match str_arg(args, "rows") {
                    Some(t) => pattern::Rows::parse(t)?,
                    None => pattern::Rows::All,
                },
            };
            let diffs = pattern::differences(&ps, &listed);
            let sides = [
                Value::object([("filter", Value::from(fa)), ("lines", ps.lines[0].into())]),
                Value::object([("filter", Value::from(fb)), ("lines", ps.lines[1].into())]),
            ];
            let top = num_arg(args, "top", s.limits.top) as usize;
            let v = ca::compare(&ps, &diffs, [&view, &view], sides, &listed, top, bool_arg(args, "queries", true));
            Ok(with_warnings(v, warnings))
        }
        "signals" => {
            let list = s.signals.iter().map(|d| {
                Value::object([
                    ("name", Value::from(d.name())),
                    ("kind", d.kind().as_str().into()),
                    ("plotted", s.plotted.iter().any(|p| p == d.name()).into()),
                ])
            });
            Ok(Value::object([("signals", Value::Array(list.collect()))]))
        }
        "signal" => {
            let name = str_arg(args, "name").ok_or("give \"name\": a signal of the tab (signals lists them)")?;
            let name = name.trim_start_matches('@');
            let def = s.signals.get(name).ok_or(format!("no signal '{name}' in this tab"))?;
            let series = match def.cached_series(&*s.source) {
                Some(x) => x,
                None => def.series(&*s.source, &progress).map_err(|e| e.to_string())?,
            };
            let n = s.source.line_count();
            let from = if args.get("from").is_some() { s.row(args, "from")? } else { 0 };
            let to = if args.get("to").is_some() { s.row(args, "to")? } else { n.saturating_sub(1) };
            Ok(ca::signal(def, &series, &view, from, to, num_arg(args, "points", 100).min(s.limits.most) as usize))
        }
        "lifecycles" => {
            let want = str_arg(args, "name");
            let mut out = Vec::new();
            for def in s.signals.lifecycles().filter(|d| want.is_none_or(|w| d.name().eq_ignore_ascii_case(w))) {
                let l = def.lifecycles(&*s.source, &progress).map_err(|e| e.to_string())?;
                out.push(ca::lifecycles(def.name(), &l, &view));
            }
            if out.is_empty() {
                return Err(match want {
                    Some(w) => format!("no lifecycle '{w}' in this tab"),
                    None => "this tab has no lifecycles (the user defines them: 🔄 in the Signals panel)".into(),
                });
            }
            Ok(Value::object([("lifecycles", Value::Array(out))]))
        }
        other => Err(format!("no tool '{other}'")),
    }
}

/// `fields`: a list, or one string `a,b`.
fn field_list(args: &Value) -> Vec<String> {
    match args.get("fields") {
        Some(Value::Array(a)) => a.iter().filter_map(Value::as_str).map(str::to_owned).collect(),
        Some(Value::Str(s)) => s.split(',').map(|x| x.trim().to_owned()).collect(),
        _ => Vec::new(),
    }
}

/// The saved pattern configuration, and the `field` asked for.
fn pattern_options(args: &Value, lines: bool) -> Result<PatternOptions, String> {
    let config = PatternConfig::load_saved().unwrap_or_default();
    Ok(PatternOptions { field: str_arg(args, "field").map(str::to_owned), lines, config })
}

/// `HH:MM:SS` now, for the activity list.
fn now() -> String {
    let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0.0, |d| d.as_secs_f64());
    let s = nerka_core::time::format_time(t, 0);
    s.rsplit(' ').next().unwrap_or(&s).to_owned()
}

impl App {
    /// An agent's call (`nerka mcp`): answered now, or by a thread, or (a
    /// screenshot) after the next frames.
    pub(crate) fn agent_call(&mut self, ctx: &egui::Context, call: AgentCall) {
        let AgentCall { tool, args, reply } = call;
        if let Err(e) = check_args(&tool, &args) {
            let _ = reply.send(Err(e));
            return;
        }
        match tool.as_str() {
            "window" => {
                let _ = reply.send(Ok(self.agent_window()));
            }
            "queries" => self.agent_queries(&args, reply),
            "summary" | "help" | "count" | "filter" | "lines" | "bookmarks" | "patterns" | "compare" | "signals"
            | "signal" | "lifecycles" => match self.agent_snap(&args) {
                Err(e) => {
                    let _ = reply.send(Err(e));
                }
                Ok(snap) => {
                    std::thread::spawn(move || {
                        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| read(&tool, &args, &snap)))
                            .unwrap_or_else(|_| Err(format!("{tool} failed inside nerka (a bug: please report it)")));
                        let _ = reply.send(r);
                    });
                }
            },
            "show" | "set_filter" | "set_search" | "set_records" | "bookmark" | "plot" => {
                let r = self.agent_change(ctx, &tool, &args);
                let _ = reply.send(r);
            }
            "screenshot" => self.agent_screenshot(ctx, &args, reply),
            other => {
                let _ = reply.send(Err(format!("no tool '{other}'")));
            }
        }
    }

    /// The tab a call names (`tab`: its number from 1, or part of its
    /// title), else the active one.
    fn agent_tab(&self, args: &Value) -> Result<usize, String> {
        if self.tabs.is_empty() {
            return Err("no file is open in this window".into());
        }
        let i = match args.get("tab") {
            None | Some(Value::Null) => self.active.min(self.tabs.len() - 1),
            Some(v) => match (v.as_u64(), v.as_str()) {
                (Some(n), _) if n >= 1 && (n as usize) <= self.tabs.len() => n as usize - 1,
                (_, Some(t)) => {
                    let t = t.to_lowercase();
                    self.tabs
                        .iter()
                        .position(|x| x.title().to_lowercase().contains(&t))
                        .ok_or(format!("no tab '{t}'"))?
                }
                _ => return Err(format!("tab: 1 to {}, or part of a tab's title", self.tabs.len())),
            },
        };
        if self.tabs[i].file.is_none() {
            return Err(format!("tab {} is still reading its files", i + 1));
        }
        Ok(i)
    }

    fn agent_snap(&self, args: &Value) -> Result<Snap, String> {
        let t = &self.tabs[self.agent_tab(args)?];
        Ok(Snap {
            source: t.file.clone().expect("checked"),
            files: t.paths.iter().map(|p| p.display().to_string()).collect(),
            title: t.title(),
            signals: t.signals.clone(),
            bookmarks: Arc::new(t.bookmarks.clone()),
            view: t.result().cloned(),
            filter: t.pattern.clone(),
            search: t.search.pattern.clone(),
            plotted: plot::lock(&t.plot).plotted.clone(),
            limits: Limits { lines: self.prefs.ai_lines, most: self.prefs.ai_most, top: self.prefs.ai_top },
        })
    }

    /// `queries`: the tab's picked library queries (each with its NFD and,
    /// on a thread, how many lines it matches) and the library's queries
    /// with their notes (owner, 2026-11-25: "available queries with their
    /// notes is where the knowledge is"). Read-only: an agent proposes, the
    /// user changes the library.
    fn agent_queries(&mut self, args: &Value, reply: Reply) {
        let snap = match self.agent_snap(args) {
            Ok(s) => s,
            Err(e) => {
                let _ = reply.send(Err(e));
                return;
            }
        };
        let t = &self.tabs[self.agent_tab(args).expect("checked by the snapshot")];
        let role = |r: nerka_core::pack::Role| match r {
            nerka_core::pack::Role::Include => "include",
            nerka_core::pack::Role::Require => "require",
            nerka_core::pack::Role::Exclude => "exclude",
            nerka_core::pack::Role::Signal => "signal",
        };
        let picked: Vec<(String, &'static str, String, String)> = match t.picks.expand(&self.library) {
            Ok(a) => a
                .exp
                .parts
                .iter()
                .map(|p| {
                    let args: Vec<String> = p.args.iter().map(|(k, v)| format!("{k}={}", v.value)).collect();
                    (p.query.clone(), role(p.role), args.join(", "), a.exp.part_nfd(p).to_owned())
                })
                .collect(),
            Err(_) => Vec::new(),
        };
        let label = str_arg(args, "label").map(str::to_lowercase);
        let library: Vec<Value> = if bool_arg(args, "library", true) {
            self.library
                .packs()
                .iter()
                .flat_map(|p| p.queries.iter().map(move |q| (p, q)))
                .filter(|(_, q)| {
                    label.as_ref().is_none_or(|l| q.labels.iter().any(|x| x.to_lowercase().starts_with(l.as_str())))
                })
                .map(|(p, q)| {
                    let mut v = Value::object([("query", Value::from(format!("{}.{}", p.name, q.id)))]);
                    if !q.params.is_empty() {
                        v.push("params", q.params.clone());
                    }
                    if !q.labels.is_empty() {
                        v.push("labels", q.labels.clone());
                    }
                    if let Some(parent) = p.parent_of(q) {
                        v.push("parent", parent);
                    }
                    v.push("note", q.note.as_str()).push("nfd", q.text.trim());
                    v
                })
                .collect()
        } else {
            Vec::new()
        };
        let counts = bool_arg(args, "counts", true);
        std::thread::spawn(move || {
            let picks: Vec<Value> = picked
                .into_iter()
                .map(|(query, role, args, nfd)| {
                    let mut v = Value::object([("query", Value::from(query)), ("role", role.into())]);
                    if !args.is_empty() {
                        v.push("args", args);
                    }
                    if counts {
                        let mut w = Vec::new();
                        match snap.rows(Some(&nfd), false, &mut w) {
                            Ok(rows) => v.push("count", rows.len()),
                            Err(e) => v.push("error", e),
                        };
                    }
                    v.push("nfd", nfd);
                    v
                })
                .collect();
            let mut v = Value::object([("picked", Value::Array(picks))]);
            v.push("filter", snap.filter.as_str()).push("library", Value::Array(library));
            let _ = reply.send(Ok(v));
        });
    }

    /// `window`: the tabs, what each shows, and the active one.
    fn agent_window(&self) -> Value {
        let tabs = self.tabs.iter().enumerate().map(|(i, t)| {
            let mut v = Value::object([("tab", Value::from(i + 1)), ("title", t.title().into())]);
            v.push("files", t.paths.iter().map(|p| Value::from(p.display().to_string())).collect::<Vec<_>>())
                .push("active", i == self.active)
                .push("loaded", t.file.is_some())
                .push("lines", t.file.as_ref().map(|f| f.line_count()))
                .push("filter", t.pattern.as_str())
                .push("view_lines", t.result().map(|r| r.lines.len()))
                .push("search", t.search.pattern.as_str())
                .push("bookmarks", t.bookmarks.len())
                .push("annotations", t.bookmarks.annotations())
                .push("plotted", plot::lock(&t.plot).plotted.clone())
                .push("signals", t.signals.len())
                .push("library_queries", self.library.packs().iter().map(|p| p.queries.len()).sum::<usize>());
            if let Some(f) = &t.file {
                let files: Vec<String> = t.paths.iter().map(|p| p.display().to_string()).collect();
                let row = t.current_line().min(f.line_count().saturating_sub(1));
                let mut cur = View { source: &**f, files: &files, compact: false, width: 0 }.place(row);
                cur.push("raw", f.line_text(row).map(|x| x.into_owned()));
                v.push("selected", cur);
            }
            v
        });
        Value::object([("tabs", Value::Array(tabs.collect()))])
    }

    /// A change to a tab, listed in 🤖 Agent with how to undo it.
    fn agent_change(&mut self, ctx: &egui::Context, tool: &str, args: &Value) -> Result<Value, String> {
        let i = self.agent_tab(args)?;
        let snap = self.agent_snap(args)?;
        let t = &mut self.tabs[i];
        let paths = t.paths.clone();
        let (what, undo, answer) = match tool {
            "show" => {
                let row = snap.row(args, "line")?;
                self.active = i;
                let t = &mut self.tabs[i];
                t.goto_line(row);
                let place = snap.view().place(row);
                (format!("showed line {}", row_text(&place)), None, place)
            }
            "set_filter" | "set_search" => {
                let q = str_arg(args, "query").ok_or("give \"query\": NFD (empty: none)")?.to_owned();
                if !q.trim().is_empty() {
                    snap.compile(&q, &mut Vec::new())?;
                }
                let filter = tool == "set_filter";
                let (mode, before) =
                    if filter { (t.mode, t.pattern.clone()) } else { (t.search.mode, t.search.pattern.clone()) };
                let undo = if filter {
                    Undo::Filter { paths: paths.clone(), mode, pattern: before }
                } else {
                    Undo::Search { paths: paths.clone(), mode, pattern: before }
                };
                if filter {
                    t.mode = Mode::Query;
                    t.pattern = q.clone();
                    t.start_search(ctx);
                } else {
                    t.search.mode = Mode::Query;
                    t.search.pattern = q.clone();
                    t.run_search_box(ctx);
                }
                self.active = i;
                let what = format!(
                    "set the {} to {}",
                    if filter { "filter" } else { "search" },
                    if q.is_empty() { "nothing".into() } else { q.clone() }
                );
                let note = if filter {
                    "the window filters now: count or filter with in_view reads its result"
                } else {
                    "the window searches now"
                };
                (what, Some(undo), Value::object([("ok", Value::from(true)), ("note", note.into())]))
            }
            "bookmark" => {
                let row = snap.row(args, "line")?;
                let before = t.bookmarks.name(row).map(|n| {
                    (
                        n.to_owned(),
                        t.bookmarks.note(row).unwrap_or_default().to_owned(),
                        t.bookmarks.author(row).unwrap_or_default().to_owned(),
                    )
                });
                if !t.bookmarks.contains(row) {
                    t.bookmarks.toggle(row);
                    t.bookmarks.set_author(row, str_arg(args, "by").unwrap_or("ai"));
                }
                if let Some(n) = str_arg(args, "note") {
                    t.bookmarks.set_note(row, n);
                }
                if let Some(n) = str_arg(args, "name") {
                    t.bookmarks.set_name(row, n);
                }
                t.save_bookmarks();
                let mut v = snap.view().place(row);
                v.push("name", t.bookmarks.name(row))
                    .push("note", t.bookmarks.note(row))
                    .push("author", t.bookmarks.author(row));
                let what = format!("bookmarked line {}: {}", row_text(&v), t.bookmarks.note(row).unwrap_or_default());
                (what, Some(Undo::Bookmark { paths: paths.clone(), row, before }), v)
            }
            "set_records" => {
                let spec = str_arg(args, "records").ok_or(
                    "give \"records\": start=/REGEX/ (a line matching it starts a record), sep=/REGEX/, blank, \
                     or lines (a row a line)",
                )?;
                let split = match spec.trim() {
                    "lines" => None,
                    s => Some(RecordSplit::parse(s).map_err(|e| format!("records {s}: {e}"))?),
                };
                let before = t.records_for.clone();
                for p in &paths {
                    t.records_for.retain(|(q, _)| q != p);
                    if let Some(s) = &split {
                        t.records_for.push((p.clone(), s.clone()));
                    }
                }
                t.reopen(ctx, &self.prefs, self.fibex.clone());
                self.active = i;
                let note = if split.is_some() {
                    "the tab reads its files again: its rows are records now (line numbers count records; \
                     first_line is where each starts in the file); filter and patterns take whole records"
                } else {
                    "the tab reads its files again: a row a line, as before"
                };
                let undo = Undo::Records { paths: paths.clone(), before };
                (
                    format!("read the tab as records {spec}"),
                    Some(undo),
                    Value::object([("ok", Value::from(true)), ("note", note.into())]),
                )
            }
            "plot" => {
                let names: Vec<String> = match args.get("signals") {
                    Some(Value::Array(a)) => {
                        a.iter().filter_map(Value::as_str).map(|n| n.trim_start_matches('@').to_owned()).collect()
                    }
                    Some(Value::Str(s)) => s.split(',').map(|n| n.trim().trim_start_matches('@').to_owned()).collect(),
                    _ => {
                        return Err(
                            "give \"signals\": the names of the tab's signals to plot (signals lists them)".into()
                        );
                    }
                };
                if let Some(bad) = names.iter().find(|n| snap.signals.get(n).is_none()) {
                    return Err(format!("no signal '{bad}' in this tab (signals lists them)"));
                }
                let mut p = plot::lock(&t.plot);
                let undo = Undo::Plot { paths: paths.clone(), plotted: p.plotted.clone(), shown: p.shown };
                if bool_arg(args, "only", false) {
                    p.plotted.clear();
                }
                for n in &names {
                    if !p.is_plotted(n) {
                        p.toggle(n);
                    }
                }
                p.shown = true;
                let plotted = p.plotted.clone();
                drop(p);
                self.active = i;
                (
                    format!("plotted {}", names.join(", ")),
                    Some(undo),
                    Value::object([("plotted", Value::from(plotted))]),
                )
            }
            other => return Err(format!("no tool '{other}'")),
        };
        let tab = self.tabs[i].title();
        let undoable = if undo.is_some() { " (↶ in 🤖 Agent)" } else { "" };
        self.tabs[i].status = format!("🤖 Agent {what}{undoable}");
        self.agent.log.push(Activity { when: now(), tab, what, undo, undone: false });
        ctx.request_repaint();
        Ok(answer)
    }

    /// `screenshot`: the window, or the plot's own window, as a PNG once
    /// drawn (answered from [`Self::agent_frame`]).
    fn agent_screenshot(&mut self, ctx: &egui::Context, args: &Value, reply: Reply) {
        let slot: Slot = Default::default();
        let plot_shown = self.tabs.get(self.active).is_some_and(|t| plot::lock(&t.plot).shown);
        match str_arg(args, "of").unwrap_or("window") {
            "plot" if !plot_shown => {
                let _ = reply.send(Err("no plot is shown: plot a signal first (plot)".into()));
                return;
            }
            "plot" if self.plot_window => {
                self.plot_win.lock().unwrap_or_else(|e| e.into_inner()).shoot = Some(slot.clone())
            }
            "plot" | "window" => crate::shot::request_into(ctx, slot.clone()),
            other => {
                let _ = reply.send(Err(format!("of: window or plot, not '{other}'")));
                return;
            }
        }
        self.agent.shots.push((slot, reply, Instant::now()));
        ctx.request_repaint();
    }

    /// Each frame: screenshots that are ready are answered; the 🤖 Agent
    /// window, if open.
    pub(crate) fn agent_frame(&mut self, ctx: &egui::Context) {
        let mut waiting = Vec::new();
        for (slot, reply, asked) in std::mem::take(&mut self.agent.shots) {
            let image = slot.lock().unwrap_or_else(|e| e.into_inner()).take();
            match image {
                Some(img) => {
                    let png = crate::shot::png(&img);
                    let v = Value::object([
                        ("mime", Value::from("image/png")),
                        ("width", img.size[0].into()),
                        ("height", img.size[1].into()),
                        ("data", ca::base64(&png).into()),
                    ]);
                    let _ = reply.send(Ok(v));
                }
                None if asked.elapsed() > Duration::from_secs(20) => {
                    let _ = reply.send(Err("the window didn't draw a picture (minimized?)".into()));
                }
                None => {
                    waiting.push((slot, reply, asked));
                    ctx.request_repaint();
                }
            }
        }
        self.agent.shots = waiting;
        if self.agent.open {
            self.agent_window_ui(ctx);
        }
    }

    /// 🤖 Agent: what agents changed here, newest first, each with ↶ Undo.
    fn agent_window_ui(&mut self, ctx: &egui::Context) {
        let mut open = self.agent.open;
        let mut undo = None;
        egui::Window::new("🤖 Agent").open(&mut open).default_width(420.0).show(ctx, |ui| {
            ui.weak("What an agent changed in this window (nerka mcp). Its bookmarks are annotations: accept them in 🔖 Bookmarks.");
            ui.separator();
            egui::ScrollArea::vertical().max_height(360.0).show(ui, |ui| {
                for (k, a) in self.agent.log.iter().enumerate().rev() {
                    ui.horizontal_wrapped(|ui| {
                        ui.weak(&a.when);
                        let text = format!("{} · {}", a.tab, a.what);
                        if a.undone {
                            ui.label(egui::RichText::new(text).strikethrough().weak());
                            ui.weak("(undone)");
                        } else {
                            ui.label(text);
                            if a.undo.is_some() && ui.small_button("↶ Undo").clicked() {
                                undo = Some(k);
                            }
                        }
                    });
                }
            });
        });
        self.agent.open = open;
        if let Some(k) = undo {
            self.agent_undo(ctx, k);
        }
    }

    /// Take change `k` back, if its tab is still open.
    pub(crate) fn agent_undo(&mut self, ctx: &egui::Context, k: usize) {
        let Some(a) = self.agent.log.get_mut(k) else { return };
        let Some(u) = a.undo.take() else { return };
        a.undone = true;
        let paths = match &u {
            Undo::Filter { paths, .. }
            | Undo::Search { paths, .. }
            | Undo::Bookmark { paths, .. }
            | Undo::Plot { paths, .. }
            | Undo::Records { paths, .. } => paths.clone(),
        };
        let Some(t) = self.tabs.iter_mut().find(|t| t.paths == paths) else { return };
        match u {
            Undo::Filter { mode, pattern, .. } => {
                t.mode = mode;
                t.pattern = pattern;
                t.start_search(ctx);
            }
            Undo::Search { mode, pattern, .. } => {
                t.search.mode = mode;
                t.search.pattern = pattern;
                t.run_search_box(ctx);
            }
            Undo::Bookmark { row, before, .. } => {
                t.bookmarks.remove(row);
                if let Some((name, note, author)) = before {
                    t.bookmarks.toggle(row);
                    t.bookmarks.set_name(row, name);
                    t.bookmarks.set_note(row, note);
                    t.bookmarks.set_author(row, &author);
                }
                t.save_bookmarks();
            }
            Undo::Plot { plotted, shown, .. } => {
                let mut p = plot::lock(&t.plot);
                p.plotted = plotted;
                p.shown = shown;
            }
            Undo::Records { before, .. } => {
                t.records_for = before;
                t.reopen(ctx, &self.prefs, self.fibex.clone());
            }
        }
        t.status = "🤖 Agent's change undone".into();
    }
}

/// `file:line` (or `line`) of a place.
fn row_text(place: &Value) -> String {
    let line = place.get("line").and_then(Value::as_u64).unwrap_or(0);
    line.to_string()
}
