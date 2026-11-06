//! nerka-gui — desktop log viewer: one tab per open file. The main view
//! shows the lines the Filter keeps (every line without a filter); the
//! Search looks inside them and lists its hits in the pane below; bookmarks
//! and the library are panels on the right. Filter, scroll position, bookmarks and column widths belong to
//! the tab. Plain text and DLT files are recognised by their content; DLT
//! is shown as a table.
//!
//! Open with Ctrl+O, drag & drop, or from a shell:
//!     nerka-gui app.log other.log
//!     nerka-gui app.log --query 'level >= ERROR AND raw ~ /SSL/'
//!     nerka-gui app.log --text 'connection reset'
//!     nerka-gui app.log --regex 'timeout|refused'
//!     nerka-gui --new-window --name 'OTA crash' trace.dlt   (else files go to the window focused last)
//!     nerka-gui app.log --pack packs/ --pick vision.cam_scope --search-pick vision.frame_drops --param cam=CAM1 --highlight vision.cam_scope
//! Keys: Ctrl+F filter · Enter run · n/N next/prev search hit · Ctrl+G go to
//! line (the nearest kept line when filtered) · Ctrl+Shift+L library
//! · m toggle bookmark · b/B next/prev bookmark · Ctrl+C copy selection
//! (or line) · Ctrl+A select all · Ctrl+Home/End
//! · Ctrl+Tab / Ctrl+Shift+Tab next/prev tab · Ctrl+W close tab · F1 about.
//!
//! Recent filters are kept in `<config dir>/nerka/history`, recent
//! searches in `<config dir>/nerka/search-history`; the 🕘 button next to
//! each box lists them, and ↑ / ↓ in the box steps through them.
//!
//! Search manager (`doc/search-manager.md`): the Library pane lists the
//! queries of the packs in `<config dir>/nerka/packs` (and `--pack`); the
//! Selection pane holds the ones in use, in order, editable. Apply expands
//! them (in core) into plain NFD in the Filter and Search boxes. The
//! Search box looks only inside the filter result and colours its hits per
//! query.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod icon;
mod icon_art;
mod instance;
mod job;
mod library;
mod plot;
mod plots;
mod prefs;
mod presets;
mod recall;
mod records;
mod shot;
mod signals;
mod view;

use eframe::egui::{self, Color32, Key, KeyboardShortcut, Modifiers, RichText};
use job::{Job, Poll};
use library::{Applied, LibraryView, Picks, Target, Term, Use};
use nerka_core::dsl::{AutoRead, Plain, read_auto_in, read_auto_with};
use nerka_core::pack::{self, Library, Role};
use nerka_core::preset::{BoxText, Preset};
use nerka_core::{
    Bookmarks, FieldDef, Filter, Highlighter, LineNo, LogSource, Matcher, MatcherOptions, Query, SearchHistory,
    SearchHits, SearchResult, SearchSet, SignalKind, SignalScope, Signals, text_to_nfd,
};
use prefs::SearchPrefs;
use std::borrow::Cow;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use view::{Columns, Highlight, NoteMode, SearchMarks, ViewOutput, ViewState, log_view};

fn main() -> eframe::Result {
    let mut initial: Vec<PathBuf> = Vec::new();
    // (history tag, pattern): the same tags the search history uses.
    let mut preset: Option<(&str, String)> = None;
    // Library options: packs to load, selected queries and parameters for
    // every file.
    let (mut packs, mut show_library) = (Vec::new(), false);
    let mut startup: Vec<(Use, String)> = Vec::new();
    let mut searches: Vec<String> = Vec::new();
    let mut highlight: Vec<String> = Vec::new();
    let mut params = std::collections::BTreeMap::new();
    // `--signal SPEC` (as for the CLI): signals for this session (saved
    // with the others once the Signals panel changes them).
    let mut signals = Signals::default();
    // `--plot NAME`: plot these signals.
    let mut plotted: Vec<String> = Vec::new();
    // `--fibex FILE`: FIBEX files for DLT non-verbose messages.
    let mut fibex: Vec<PathBuf> = Vec::new();
    // `--cursor TIME`: time cursors in the plot.
    let mut cursors: Vec<f64> = Vec::new();
    // `--marker NAME=VALUE[:LABEL]`: horizontal lines in a plotted lane.
    let mut markers: Vec<(String, plot::Marker)> = Vec::new();
    // `--plot-window`: the plot starts in a window of its own.
    let mut plot_window = false;
    let (mut new_window, mut window_name, mut preset_file) = (false, None, None);
    let mut args = std::env::args_os().skip(1);
    while let Some(a) = args.next() {
        let mut value = || args.next().map(|v| v.to_string_lossy().into_owned()).unwrap_or_default();
        let tag = match a.to_str() {
            Some("--query" | "-q") => "query",
            Some("--regex" | "-r") => "regex",
            Some("--text" | "-t") => "text",
            Some("--pack") => {
                packs.push(PathBuf::from(value()));
                continue;
            }
            Some("--pick") => {
                startup.push((Use::Include, value()));
                continue;
            }
            Some("--exclude") => {
                startup.push((Use::Exclude, value()));
                continue;
            }
            Some("--signal-pick") => {
                startup.push((Use::Signal, value()));
                continue;
            }
            Some("--highlight") => {
                highlight.push(value());
                continue;
            }
            Some("--search-pick") => {
                searches.push(value());
                continue;
            }
            Some("--param") => {
                if let Some((k, v)) = value().split_once('=') {
                    params.insert(k.to_owned(), (v.to_owned(), false));
                }
                continue;
            }
            Some("--library") => {
                show_library = true;
                continue;
            }
            Some("--plot") => {
                plotted.push(value());
                continue;
            }
            Some("--fibex") => {
                fibex.push(PathBuf::from(value()));
                continue;
            }
            Some("--marker") => {
                // NAME=VALUE[:LABEL]
                let v = value();
                let parsed = v.split_once('=').and_then(|(name, rest)| {
                    let (num, label) = rest.split_once(':').unwrap_or((rest, ""));
                    Some((name.to_owned(), plot::Marker { value: num.trim().parse().ok()?, label: label.to_owned() }))
                });
                match parsed {
                    Some(m) => markers.push(m),
                    None => eprintln!("nerka-gui: --marker {v}: expected NAME=VALUE[:LABEL]"),
                }
                continue;
            }
            Some("--cursor") => {
                // A time (2026-10-17 10:00:01.5, 10:00:01, seconds) or a line number.
                match nerka_core::time::parse_time(&value()) {
                    Some(x) => cursors.push(x),
                    None => eprintln!("nerka-gui: --cursor: expected a time or a number"),
                }
                continue;
            }
            Some("--plot-window") => {
                plot_window = true;
                continue;
            }
            Some("--signal") => {
                let spec = value();
                match nerka_core::SignalDef::parse_spec(&spec) {
                    Ok(d) => signals.set(d),
                    Err(e) => eprintln!("nerka-gui: --signal {spec}: {e}"),
                }
                continue;
            }
            Some("--new-window") => {
                new_window = true;
                continue;
            }
            Some("--name") => {
                window_name = Some(value());
                continue;
            }
            // A tab moved from another window: its filter, search and
            // selection, as a preset file (deleted once read).
            Some("--preset-file") => {
                preset_file = Some(PathBuf::from(value()));
                continue;
            }
            _ => {
                initial.push(PathBuf::from(a));
                continue;
            }
        };
        preset = args.next().map(|v| (tag, v.to_string_lossy().into_owned()));
    }
    // Plain `nerka-gui FILE…`: the window focused last opens them.
    let plain = preset.is_none()
        && packs.is_empty()
        && startup.is_empty()
        && searches.is_empty()
        && highlight.is_empty()
        && params.is_empty()
        && signals.is_empty()
        && plotted.is_empty()
        && fibex.is_empty()
        && markers.is_empty()
        && cursors.is_empty()
        && !show_library
        && preset_file.is_none()
        && window_name.is_none();
    if plain && !new_window && !initial.is_empty() && instance::hand_off(&initial) {
        return Ok(());
    }
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1280.0, 800.0])
            .with_title("nerka")
            .with_icon(icon::icon()),
        wgpu_options: wgpu_options(),
        ..Default::default()
    };
    eframe::run_native(
        "nerka",
        options,
        Box::new(move |cc| {
            let show_signals = !signals.is_empty();
            let mut app =
                App { pack_paths: packs, show_library, show_signals, window_name, plot_window, ..Default::default() };
            let ctx = cc.egui_ctx.clone();
            app.server = instance::Server::start(move || ctx.request_repaint());
            app.load_history();
            app.load_prefs();
            app.load_saved_signals();
            if !fibex.is_empty() {
                app.set_fibex(&cc.egui_ctx, fibex);
            }
            for d in signals.iter() {
                app.saved_signals.set((**d).clone());
            }
            app.load_library();
            app.presets.load();
            app.plots.load();
            let mut picks = Picks::default();
            picks.params = params;
            for (role, id) in &startup {
                picks.add(&app.library, id, *role, None);
            }
            for id in &highlight {
                picks.light_source(id);
            }
            let moved = preset_file.and_then(|f| {
                let text = std::fs::read_to_string(&f).ok();
                let _ = std::fs::remove_file(&f);
                nerka_core::preset::parse_preset("moved tab", &text?).ok()
            });
            // The files named open as one view (separate tabs if their
            // kinds differ).
            if !initial.is_empty() {
                app.open_files(&cc.egui_ctx, initial);
                if let Some(tab) = app.tabs.last_mut() {
                    plotted.iter().for_each(|n| plot::lock(&tab.plot).toggle(n));
                    plot::lock(&tab.plot).cursors.extend(&cursors);
                    for (name, m) in &markers {
                        plot::lock(&tab.plot).markers.entry(name.clone()).or_default().push(m.clone());
                    }
                    // A plotted name can be a FIBEX argument: it becomes a signal.
                    if let Some(f) = &app.fibex {
                        tab.arg_picks = f.arguments().into_iter().filter(|a| plotted.contains(&a.name)).collect();
                    }
                }
                if let (Some(p), Some(tab)) = (&moved, app.tabs.last_mut()) {
                    tab.use_preset(&cc.egui_ctx, &app.library, p);
                    tab.preset = None;
                }
                if let (Some((tag, pattern)), Some(tab)) = (&preset, app.tabs.last_mut())
                    && moved.is_none()
                {
                    tab.set_filter(tag, pattern);
                }
                if let Some(tab) = app.tabs.last_mut().filter(|_| moved.is_none()) {
                    tab.picks = picks.clone();
                    if !picks.nodes.is_empty() {
                        tab.apply_selection(&cc.egui_ctx, &app.library);
                    }
                    for id in &searches {
                        tab.search_for(&cc.egui_ctx, &app.library, &Target::Library(id.clone()), true);
                    }
                }
            }
            app.active = 0;
            Ok(Box::new(app))
        }),
    )
}

/// The graphics setup: wgpu's own, except that on Windows it draws with
/// DirectX 12. wgpu picked Vulkan there, and NVIDIA's Vulkan driver
/// (`nvoglv64.dll`) crashed when a window was maximized while the plot had
/// a window of its own (owner, 2026-10-24); DirectX 12 didn't. Linux and
/// macOS keep wgpu's choice; `WGPU_BACKEND` (e.g. `vulkan`) still decides
/// when set.
fn wgpu_options() -> eframe::WgpuConfiguration {
    let mut options = eframe::WgpuConfiguration::default();
    if cfg!(windows)
        && std::env::var_os("WGPU_BACKEND").is_none()
        && let eframe::egui_wgpu::WgpuSetup::CreateNew(setup) = &mut options.wgpu_setup
    {
        setup.instance_descriptor.backends = eframe::wgpu::Backends::DX12;
    }
    options
}

/// How a Filter or Search box reads its text (`doc/search-modes.md`).
#[derive(Debug, Default, PartialEq, Clone, Copy)]
enum Mode {
    /// Plain words are an exact phrase, NFD syntax a query, mixed; a box
    /// that isn't a valid query is literal text (core: `read_auto`).
    #[default]
    Auto,
    /// klogg-style search box: plain text, or a regex when "Regex" is ticked.
    Text,
    /// Strict NFD query: `level >= ERROR AND raw ~ /SSL/`.
    Query,
}

impl Mode {
    fn label(self) -> &'static str {
        match self {
            Mode::Auto => "Auto",
            Mode::Text => "Text",
            Mode::Query => "NFD",
        }
    }

    fn describe(self) -> &'static str {
        match self {
            Mode::Auto => "Words are text, NFD is a query, both mix; anything else is text",
            Mode::Text => "Always literal text (or a regex with Regex ticked)",
            Mode::Query => "Strict NFD: level >= ERROR AND raw ~ /SSL|CERT/i AND NOT /healthcheck/",
        }
    }
}

/// The mode list of a box; true: a mode was picked in it.
fn mode_combo(ui: &mut egui::Ui, id: &str, mode: &mut Mode) -> bool {
    let mut picked = false;
    egui::ComboBox::from_id_salt(id)
        .width(58.0)
        .selected_text(mode.label())
        .show_ui(ui, |ui| {
            for m in [Mode::Auto, Mode::Text, Mode::Query] {
                picked |= ui.selectable_value(mode, m, m.label()).on_hover_text(m.describe()).clicked();
            }
        })
        .response
        .on_hover_text(mode.describe());
    picked
}

/// How a box will be read, for the line under it.
#[derive(Debug, Clone, PartialEq)]
enum Preview {
    /// Nothing to say (empty box, or Text / valid NFD: the mode says it).
    None,
    /// Auto: literal text (or, with Regex, a regex), with why it isn't a
    /// query, if it looked like one.
    AutoText { text: String, why: Option<String>, regex: bool },
    /// Auto: a query that is only a regex (`/…/flags`).
    AutoRegex(String),
    /// Auto: a query, as rewritten into NFD.
    AutoQuery(String),
    /// NFD mode: the query doesn't compile.
    QueryError(String),
}

/// What a box with `mode`, `pattern` and `options` runs, and its preview.
/// `options.fixed_string` is the Regex checkbox (Text mode only);
/// `invert` applies in Auto and Text mode.
fn read_box(
    mode: Mode,
    pattern: &str,
    options: &MatcherOptions,
    schema: &[FieldDef],
    signals: &Signals,
) -> (Result<Filter, String>, Preview) {
    let text = |t: &str, o: MatcherOptions| {
        Matcher::new(t, o).map(Filter::Regex).map_err(|e| {
            let detail = std::error::Error::source(&e).map(|s| s.to_string()).unwrap_or_default();
            format!("Invalid regex: {}", detail.lines().last().unwrap_or(""))
        })
    };
    match mode {
        Mode::Text => (text(pattern, options.clone()), Preview::None),
        Mode::Query => match Query::compile_in(pattern, schema, signals) {
            Ok(q) => (Ok(Filter::Query(q)), Preview::None),
            Err(e) if pattern.trim().is_empty() => (Err(format!("Query error: {e}")), Preview::None),
            Err(e) => (Err(format!("Query error: {e}")), Preview::QueryError(e.to_string())),
        },
        Mode::Auto => {
            // Phrases and plain text are literal, or regexes with Regex ticked.
            let regex = !options.fixed_string;
            match read_auto_in(pattern, options, schema, signals) {
                AutoRead::Text(t) => {
                    let preview = if t.is_empty() {
                        Preview::None
                    } else {
                        Preview::AutoText { text: t.clone(), why: None, regex }
                    };
                    (text(&t, options.clone()), preview)
                }
                AutoRead::NotQuery { text: t, err } => {
                    let why = Some(err.to_string());
                    (text(&t, options.clone()), Preview::AutoText { text: t, why, regex })
                }
                AutoRead::Query { nfd, query, plain } => {
                    // A query that is only a text search is shown as one.
                    let preview = match plain {
                        Some(Plain::Text(text)) => Preview::AutoText { text, why: None, regex: false },
                        Some(Plain::Regex { pattern, ignore_case }) => {
                            Preview::AutoRegex(format!("/{pattern}/{}", if ignore_case { "i" } else { "" }))
                        }
                        None => Preview::AutoQuery(nfd.clone()),
                    };
                    if !options.invert {
                        return (Ok(Filter::Query(query)), preview);
                    }
                    let not = Query::compile_in(&format!("NOT ({nfd})"), schema, signals).map(Filter::Query);
                    (not.map_err(|e| format!("Query error: {e}")), preview)
                }
            }
        }
    }
}

/// The line under a box. Returns true when "Search as text instead" was
/// clicked.
fn preview_ui(ui: &mut egui::Ui, preview: &Preview) -> bool {
    let mut as_text = false;
    let small = |s: String| RichText::new(s).small();
    match preview {
        Preview::None => {}
        Preview::AutoText { text, why, regex } => {
            ui.horizontal(|ui| {
                ui.label(small(if *regex { "→ regex:" } else { "→ text:" }.into()).weak());
                ui.add(egui::Label::new(small(text.clone()).monospace()).truncate());
                if let Some(why) = why {
                    ui.label(small("(not a query)".into()).weak()).on_hover_text(why);
                }
            });
        }
        Preview::AutoRegex(re) => {
            ui.horizontal(|ui| {
                ui.label(small("→ regex:".into()).weak());
                ui.add(egui::Label::new(small(re.clone()).monospace()).truncate());
            });
        }
        Preview::AutoQuery(nfd) => {
            ui.horizontal(|ui| {
                ui.label(small("→ query:".into()).weak());
                ui.add(egui::Label::new(small(nfd.clone()).monospace()).truncate());
            });
        }
        Preview::QueryError(e) => {
            ui.horizontal(|ui| {
                as_text =
                    ui.small_button("Search as text instead").on_hover_text("Switch to Text mode and run it").clicked();
                ui.add(egui::Label::new(small(format!("✗ {e}")).color(ui.visuals().error_fg_color)).truncate());
            });
        }
    }
    as_text
}

/// A box's preview, worked out again only when what it depends on changed.
#[derive(Default)]
struct PreviewCache {
    key: String,
    preview: Option<Preview>,
}

impl PreviewCache {
    /// `signals_gen` changes whenever the tab's signals do.
    fn get(
        &mut self,
        mode: Mode,
        pattern: &str,
        options: &MatcherOptions,
        schema: &[FieldDef],
        (signals, signals_gen): (&Signals, u64),
    ) -> Preview {
        let key = format!(
            "{mode:?}|{}|{}|{}|{signals_gen}|{pattern}",
            options.fixed_string, options.case_insensitive, options.invert
        );
        if self.key != key || self.preview.is_none() {
            self.preview = Some(read_box(mode, pattern, options, schema, signals).1);
            self.key = key;
        }
        self.preview.clone().unwrap_or(Preview::None)
    }
}

/// One open file (or several of one kind, one after another) with its own
/// filter, view positions and bookmarks.
struct Tab {
    /// The first file.
    path: PathBuf,
    /// Every file, in order (one for a plain tab).
    paths: Vec<PathBuf>,
    /// `(first line, line count)` of each file in the view, once loaded.
    part_files: Vec<(LineNo, LineNo)>,
    /// The files turned out to be of different kinds: why. The app then
    /// opens them as separate tabs.
    mixed: Option<String>,
    file: Option<Arc<dyn LogSource>>,
    loading: Option<Job<Box<dyn LogSource>>>,
    /// The lines the filter keeps (all lines without a filter).
    main_view: ViewState,
    /// The search hits.
    hits_view: ViewState,
    columns: Columns,

    mode: Mode,
    /// The mode last picked in the Filter box's list: the one remembered
    /// for new tabs. A mode set for the box (a selection, 🔍, the history,
    /// "Search as text instead") isn't the user's choice of a default.
    picked_mode: Mode,
    pattern: String,
    options: MatcherOptions,
    result: Option<Arc<SearchResult>>,
    searching: Option<(Arc<Filter>, Job<SearchResult>)>,

    /// The library expansion the Filter box holds, while its text is
    /// unchanged: shown grouped, with chips.
    filter_exp: Option<Arc<Applied>>,
    /// The Selection pane's queries.
    picks: Picks,
    /// The preset last used or made in this tab.
    preset: Option<String>,
    /// The selection as last expanded, for line colours: they follow the
    /// selection, not the Filter box text, so editing the box keeps them.
    colors_exp: Option<Arc<Applied>>,
    /// Whole-line colours of the filter queries switched on in `picks`.
    line_colors: Option<(Arc<ActiveSearch>, Arc<SearchHits>)>,
    colors_job: Option<(Arc<ActiveSearch>, Job<SearchHits>)>,
    search: SearchBox,
    /// ↑ / ↓ in the Filter box.
    filter_recall: recall::Recall,
    /// The chips under the Filter and Search boxes are unfolded (one line
    /// per query with its text).
    chips_open: bool,
    filter_preview: PreviewCache,

    bookmarks: Bookmarks,
    /// The bookmarks changed since the last frame: what uses them runs again.
    marks_changed: bool,
    /// Where bookmark notes show in the views (the app's setting).
    notes: NoteMode,
    /// The filter behind `result` (rerun when it uses the bookmarks).
    applied: Option<Arc<Filter>>,
    /// Bookmarks whose line is unfolded in the panel.
    marks_open: std::collections::HashSet<LineNo>,
    /// A bookmark's name being typed in the panel; set when the field
    /// loses focus (so it can be emptied and retyped).
    mark_rename: Option<(LineNo, String)>,
    /// The line shown whole in the line window (Enter), if open.
    detail: Option<LineDetail>,
    /// The tab's signals (`@name` in its queries), the generation that
    /// changes with them, and their panel.
    signals: Signals,
    signals_gen: u64,
    signals_ui: signals::SignalsUi,
    /// Collects the signals' events for the panel (the engine keeps them
    /// with each definition).
    signals_job: Option<Job<()>>,
    /// Shared with the plot's own window.
    plot: plot::SharedPlot,
    /// What `signals` was built from: (saved signals' generation, choices'
    /// generation, the selection's fingerprint); none: build again.
    signals_key: Option<(u64, u64, String)>,
    /// Kinds and scopes chosen for query signals, and their generation.
    query_kind: std::collections::HashMap<String, SignalKind>,
    query_scope: std::collections::HashMap<String, SignalScope>,
    choices_gen: u64,
    /// The events each signal shows (filtered ones per filter result).
    views: signals::Views,
    /// Read again: the line to show once it is loaded.
    reopen_at: Option<LineNo>,
    /// FIBEX arguments used as signals (picked in the Signals panel).
    arg_picks: Vec<nerka_core::Argument>,
    /// The Files window: the tab's files as they are being edited.
    files_edit: Option<Vec<PathBuf>>,
    /// How the tab's text files are split into rows (remembered per file;
    /// `None`: automatic, JSON if a file looks like JSON).
    records: Option<nerka_core::input::records::RecordSplit>,
    /// The Records window, while it is open.
    records_edit: Option<records::Edit>,
    /// The files are being read in a new order or set.
    files_undo: Option<FilesUndo>,
    status: String,
}

/// A tab's files being read in a new order or set (`Tab::set_files`).
struct FilesUndo {
    /// The files shown before: put back if the new ones can't be one view.
    old: Vec<PathBuf>,
    /// The current line's file and its line in that file: where the view
    /// goes once the files are read.
    at: Option<(PathBuf, LineNo)>,
}

/// One line shown whole: its cells as the source formats them (a DLT
/// payload keeps its own line breaks), read when the window opened.
struct LineDetail {
    line: LineNo,
    cells: Vec<String>,
    /// Column names of a table source; empty for plain text.
    names: Vec<&'static str>,
    /// Every field the source supplies for the line: name, value (`None`:
    /// the line has none), kind. Shown with the Fields checkbox.
    fields: Vec<(String, Option<String>, &'static str)>,
    wrap: bool,
    /// The tab's signals on this line (filled by the tab every frame:
    /// values appear once their events are collected).
    signals: Vec<LineSignal>,
    /// A line the window asks to go to (an event that set a state).
    goto: Option<LineNo>,
    /// ↑ / ↓ pressed over the window: step to the next (true) or previous
    /// line of the view it follows.
    step: Option<bool>,
    /// It follows the search hits (opened or moved there), else the main view.
    in_hits: bool,
}

/// One signal in the line window: its value on the line, and whether the
/// line itself carries it (an event) or it holds from an earlier one.
#[derive(Debug, Clone, PartialEq)]
struct LineSignal {
    name: String,
    kind: &'static str,
    /// Where it comes from: built in, saved, query, FIBEX.
    origin: &'static str,
    /// The value, or none (no event yet, or still collecting).
    value: Option<String>,
    /// The line of the event that set the value; this line for an event.
    from: Option<LineNo>,
}

/// The line window's Signals section: the signals the line carries (its
/// events); the others' state on it, with the line that set it, folded
/// away below. Returns a line to go to (one of those was clicked).
fn signals_section(ui: &mut egui::Ui, line: LineNo, signals: &[LineSignal]) -> Option<LineNo> {
    let (events, held): (Vec<&LineSignal>, Vec<&LineSignal>) =
        signals.iter().filter(|s| s.from.is_some()).partition(|s| s.from == Some(line));
    let pending = signals.iter().filter(|s| s.from.is_none() && s.value.is_some()).count();
    let mut goto = None;
    egui::CollapsingHeader::new(RichText::new(format!("Signals on this line ({})", events.len())).strong())
        .id_salt("line-detail-signals")
        .default_open(true)
        .show(ui, |ui| {
            if events.is_empty() {
                ui.weak("None: this line carries no signal's value.");
            }
            egui::Grid::new("line-detail-signals-grid").num_columns(3).spacing([12.0, 2.0]).striped(true).show(
                ui,
                |ui| {
                    for s in &events {
                        ui.label(RichText::new(format!("@{}", s.name)).monospace().strong());
                        ui.monospace(s.value.as_deref().unwrap_or("—"));
                        ui.weak(format!("{} · {}", s.kind, s.origin));
                        ui.end_row();
                    }
                },
            );
            if pending > 0 {
                ui.weak(format!("{pending} more still being collected…"));
            }
            if held.is_empty() {
                return;
            }
            egui::CollapsingHeader::new(format!("Held from earlier lines ({})", held.len()))
                .id_salt("line-detail-held")
                .default_open(false)
                .show(ui, |ui| {
                    egui::Grid::new("line-detail-held-grid").num_columns(4).spacing([12.0, 2.0]).striped(true).show(
                        ui,
                        |ui| {
                            for s in &held {
                                ui.monospace(format!("@{}", s.name));
                                ui.monospace(s.value.as_deref().unwrap_or("—"));
                                let from = s.from.expect("held");
                                let r = ui.link(format!("↑ line {}", from + 1));
                                if r.on_hover_text("The state: set by the event on that line. Click to go there.")
                                    .clicked()
                                {
                                    goto = Some(from);
                                }
                                ui.weak(format!("{} · {}", s.kind, s.origin));
                                ui.end_row();
                            }
                        },
                    );
                });
        });
    goto
}

/// Every field `file` supplies for `line`: name, value as text (`None`
/// if the line has none), kind. Numbers show in hex too where that's how
/// they're written (SOME/IP ids).
fn field_values(file: &dyn LogSource, line: LineNo) -> Vec<(String, Option<String>, &'static str)> {
    file.schema()
        .iter()
        .enumerate()
        .map(|(id, def)| {
            let value = file.field(line, id).map(|v| match v {
                nerka_core::FieldValue::Str(b) => String::from_utf8_lossy(&b).into_owned(),
                nerka_core::FieldValue::U64(n) if n > 9 => format!("{n} (0x{n:x})"),
                nerka_core::FieldValue::U64(n) => n.to_string(),
                nerka_core::FieldValue::F64(x) => x.to_string(),
            });
            let kind = match def.kind {
                nerka_core::FieldKind::Str => "text",
                nerka_core::FieldKind::Name => "name",
                nerka_core::FieldKind::U64 => "number",
                nerka_core::FieldKind::F64 => "number",
            };
            (def.name.to_string(), value, kind)
        })
        .collect()
}

/// The line window's Fields section: every field, its value and kind; a
/// field the line has no value for is dimmed.
fn fields_section(ui: &mut egui::Ui, fields: &[(String, Option<String>, &'static str)]) {
    egui::CollapsingHeader::new(RichText::new(format!("Fields ({})", fields.len())).strong())
        .id_salt("line-detail-fields")
        .default_open(true)
        .show(ui, |ui| {
            egui::Grid::new("line-detail-fields-grid").num_columns(3).spacing([12.0, 2.0]).striped(true).show(
                ui,
                |ui| {
                    for (name, value, kind) in fields {
                        match value {
                            Some(v) => {
                                ui.label(RichText::new(name).monospace().strong());
                                // Long values (a payload) are cut: the message below has all of it.
                                let shown: String = v.chars().take(200).collect();
                                ui.monospace(if v.chars().count() > 200 { format!("{shown}…") } else { shown });
                            }
                            None => {
                                ui.label(RichText::new(name).monospace().weak());
                                ui.weak("—");
                            }
                        }
                        ui.weak(*kind);
                        ui.end_row();
                    }
                },
            );
        });
}

/// Text shown in the line window at most; Copy still copies all of it.
const DETAIL_MAX_BYTES: usize = 1 << 20;

impl LineDetail {
    fn of(file: &dyn LogSource, line: LineNo) -> Option<Self> {
        let cells = file.line_detail(line)?;
        let names = file.columns().iter().map(|c| c.name).collect();
        let fields = field_values(file, line);
        Some(LineDetail {
            line,
            cells,
            names,
            fields,
            wrap: true,
            signals: Vec::new(),
            goto: None,
            step: None,
            in_hits: false,
        })
    }

    /// The same window on another line, keeping Wrap and the view it follows.
    fn moved(&self, file: &dyn LogSource, line: LineNo) -> Option<Self> {
        Self::of(file, line).map(|d| LineDetail { wrap: self.wrap, in_hits: self.in_hits, ..d })
    }

    /// The window: the cells before the message as a name / value grid,
    /// the message in a selectable box with its own line breaks. Returns
    /// whether it is still open and the text to copy, if asked.
    fn show(&mut self, ctx: &egui::Context) -> (bool, Option<String>) {
        let mut open = true;
        let mut copied = None;
        let table = !self.names.is_empty();
        egui::Window::new(format!("Line {}", self.line + 1))
            .id(egui::Id::unique("line-detail"))
            .open(&mut open)
            .resizable(true)
            .collapsible(false)
            .default_size([640.0, 360.0])
            .show(ctx, |ui| {
                // ↑ / ↓ over the window, or with a widget in it focused, step
                // through the log, not through the window's widgets (egui
                // moves focus with arrows). Taken before the widgets see them.
                let layer = ui.layer_id();
                let here = ctx.pointer_hover_pos().is_some_and(|p| ctx.layer_id_at(p) == Some(layer))
                    || ctx
                        .memory(|m| m.focused())
                        .and_then(|id| ctx.read_response(id))
                        .is_some_and(|r| r.layer_id == layer);
                if here {
                    let none = egui::Modifiers::NONE;
                    let (up, down) = ctx.input_mut(|i| {
                        (i.consume_key(none, egui::Key::ArrowUp), i.consume_key(none, egui::Key::ArrowDown))
                    });
                    if up || down {
                        ctx.memory_mut(|m| m.move_focus(egui::FocusDirection::None));
                        self.step = Some(down);
                    }
                }
                let msg = self.cells.last().map_or("", String::as_str);
                ui.horizontal(|ui| {
                    if table {
                        let tip = format!("The {} column, with its own line breaks", self.names.last().unwrap_or(&""));
                        if ui.button("Copy message").on_hover_text(tip).clicked() {
                            copied = Some(msg.to_owned());
                        }
                        if ui.button("Copy line").on_hover_text("Every column, tab-separated").clicked() {
                            copied = Some(self.cells.join("	"));
                        }
                    } else if ui.button("Copy").clicked() {
                        copied = Some(msg.to_owned());
                    }
                    ui.checkbox(&mut self.wrap, "Wrap");
                    if !self.fields.is_empty() {
                        let id = egui::Id::unique("line-detail-all-fields");
                        let mut all = ctx.data(|d| d.get_temp::<bool>(id)).unwrap_or(false);
                        let tip = "Every field of the line (what queries can use), not only the columns";
                        if ui.checkbox(&mut all, format!("Fields ({})", self.fields.len())).on_hover_text(tip).changed()
                        {
                            ctx.data_mut(|d| d.insert_temp(id, all));
                        }
                    }
                    let lines = msg.lines().count().max(1);
                    let s = if lines == 1 { "" } else { "s" };
                    ui.weak(format!("{} characters, {lines} line{s}", msg.chars().count()));
                });
                // Everything below the buttons scrolls: many columns or
                // signals pushed the message out of a window that couldn't.
                let scroll = if self.wrap { egui::ScrollArea::vertical() } else { egui::ScrollArea::both() };
                scroll.id_salt("line-detail-scroll").auto_shrink([false, false]).show(ui, |ui| {
                    if table {
                        egui::Grid::new("line-detail-cells").num_columns(2).spacing([12.0, 2.0]).show(ui, |ui| {
                            for (name, value) in self.names.iter().zip(&self.cells[..self.cells.len() - 1]) {
                                ui.label(RichText::new(*name).strong());
                                ui.monospace(value);
                                ui.end_row();
                            }
                        });
                    }
                    if ctx.data(|d| d.get_temp::<bool>(egui::Id::unique("line-detail-all-fields"))).unwrap_or(false) {
                        fields_section(ui, &self.fields);
                    }
                    if !self.signals.is_empty() {
                        self.goto = signals_section(ui, self.line, &self.signals).or(self.goto);
                    }
                    ui.separator();
                    let mut shown = msg;
                    if shown.len() > DETAIL_MAX_BYTES {
                        let mut end = DETAIL_MAX_BYTES;
                        while !shown.is_char_boundary(end) {
                            end -= 1;
                        }
                        shown = &shown[..end];
                        ui.weak(format!("Showing the first {} KiB; Copy copies all of it.", DETAIL_MAX_BYTES >> 10));
                    }
                    {
                        // A `&str` buffer: selectable and copyable, not editable.
                        let mut text = shown;
                        // A text box never gets wider than the space it is in, so
                        // without Wrap the layout ignores that width and the box
                        // takes the text's own width (the scroll area scrolls it).
                        let wrap = self.wrap;
                        let mut layouter = |ui: &egui::Ui, buf: &dyn egui::TextBuffer, wrap_width: f32| {
                            let font = egui::TextStyle::Monospace.resolve(ui.style());
                            let width = if wrap { wrap_width } else { f32::INFINITY };
                            let job = egui::text::LayoutJob::simple(
                                buf.as_str().to_owned(),
                                font,
                                ui.visuals().text_color(),
                                width,
                            );
                            ui.fonts_mut(|f| f.layout_job(job))
                        };
                        let edit = egui::TextEdit::multiline(&mut text)
                            .font(egui::TextStyle::Monospace)
                            .desired_width(f32::INFINITY)
                            .clip_text(false)
                            .layouter(&mut layouter);
                        ui.add(edit);
                    }
                });
            });
        (open, copied)
    }
}

/// The Search box: looks only inside the filter result (the whole file
/// when no filter is applied) and colours hits per query.
#[derive(Default)]
struct SearchBox {
    mode: Mode,
    /// The mode last picked in the box's list: the one remembered for new
    /// tabs (`Tab::picked_mode`).
    picked: Mode,
    pattern: String,
    regex: bool,
    ignore_case: bool,
    /// The queries the box holds (from 🔍 Search / Add to search), while
    /// its text is unchanged: each is searched and coloured on its own.
    terms: Vec<Term>,
    /// The search that ran last. It runs again when the filter result changes.
    active: Option<Arc<ActiveSearch>>,
    hits: Option<(Arc<ActiveSearch>, Arc<SearchHits>)>,
    job: Option<(Arc<ActiveSearch>, Job<SearchHits>)>,
    /// ↑ / ↓ in the box.
    recall: recall::Recall,
    preview: PreviewCache,
}

/// A search ready to run: its queries, names, colours and highlighters.
struct ActiveSearch {
    set: SearchSet,
    names: Vec<String>,
    colors: Vec<Color32>,
    highlighters: Vec<Highlighter>,
}

impl ActiveSearch {
    fn uses_bookmarks(&self) -> bool {
        self.set.include.iter().chain(&self.set.exclude).any(Filter::uses_bookmarks)
    }

    /// The same search with `marks` for its `bookmark` conditions.
    fn with_bookmarks(&self, marks: &Arc<Bookmarks>) -> ActiveSearch {
        let bind = |fs: &[Filter]| fs.iter().map(|f| f.clone().with_bookmarks(marks)).collect();
        ActiveSearch {
            set: SearchSet { include: bind(&self.set.include), exclude: bind(&self.set.exclude) },
            names: self.names.clone(),
            colors: self.colors.clone(),
            highlighters: self.highlighters.clone(),
        }
    }
}

impl SearchBox {
    /// History tag for the box: `auto`, `text`, `regex` or `query`.
    fn tag(&self) -> &'static str {
        match self.mode {
            Mode::Auto => "auto",
            Mode::Text if self.regex => "regex",
            Mode::Text => "text",
            Mode::Query => "query",
        }
    }

    /// Fill the box from a history tag and pattern; unknown tags are ignored.
    fn set(&mut self, tag: &str, pattern: &str) {
        match tag {
            "text" | "regex" => {
                self.mode = Mode::Text;
                self.regex = tag == "regex";
            }
            "query" => self.mode = Mode::Query,
            "auto" => self.mode = Mode::Auto,
            _ => return,
        }
        self.pattern = pattern.to_owned();
    }

    /// The box's options as matcher options (Regex unticked = literal).
    fn options(&self) -> MatcherOptions {
        MatcherOptions { fixed_string: !self.regex, case_insensitive: self.ignore_case, invert: false }
    }

    /// The box's text for `terms`: OR'ed (AND binds tighter, and each term
    /// is a parenthesized chain, a string, a regex or `NOT` one of them).
    fn text_of(terms: &[Term]) -> String {
        terms.iter().map(|t| t.nfd.as_str()).collect::<Vec<_>>().join(" OR ")
    }

    /// The terms, if the box still holds their text.
    fn current_terms(&self) -> Option<&[Term]> {
        (!self.terms.is_empty() && Self::text_of(&self.terms) == self.pattern).then_some(self.terms.as_slice())
    }

    /// What the box holds as one term: typed text becomes the query that
    /// finds the same lines.
    fn typed_term(&self) -> Option<Term> {
        let text = self.pattern.trim();
        if text.is_empty() {
            return None;
        }
        let nfd = match self.mode {
            Mode::Text => text_to_nfd(text, &self.options()),
            Mode::Query => format!("({text})"),
            // Field names don't matter here: the term is compiled later.
            Mode::Auto => match read_auto_with(text, &self.options(), &[]) {
                AutoRead::Query { nfd, .. } => format!("({nfd})"),
                AutoRead::Text(t) | AutoRead::NotQuery { text: t, .. } => text_to_nfd(&t, &self.options()),
            },
        };
        let name: String = text.chars().take(24).collect();
        Some(Term { name, nfd, color: library::palette(0) })
    }
}

impl Tab {
    #[cfg(test)]
    fn open(ctx: &egui::Context, path: PathBuf, prefs: &SearchPrefs) -> Self {
        Self::open_many(ctx, vec![path], prefs, None)
    }

    /// One tab for `paths`: one file, or several of one kind one after
    /// another (nerka_core::open_many checks the kind).
    /// `fibex` describes DLT non-verbose messages.
    fn open_many(
        ctx: &egui::Context,
        paths: Vec<PathBuf>,
        prefs: &SearchPrefs,
        fibex: Option<Arc<nerka_core::Fibex>>,
    ) -> Self {
        let ps = paths.clone();
        let mut options = prefs.open_options();
        options.fibex = fibex;
        // As the first file was read last time: lines or records.
        let split = records::remembered(&paths[0]);
        options.records = split.clone();
        Self {
            path: paths[0].clone(),
            paths,
            part_files: Vec::new(),
            mixed: None,
            file: None,
            loading: Some(Job::spawn(ctx, move |prog| nerka_core::open_many(&ps, &options, prog))),
            main_view: ViewState::with_free_keys(),
            hits_view: ViewState::default(),
            columns: Columns::default(),
            mode: prefs.mode,
            picked_mode: prefs.mode,
            pattern: String::new(),
            options: prefs.options.clone(),
            result: None,
            searching: None,
            filter_exp: None,
            picks: Picks::default(),
            preset: None,
            colors_exp: None,
            line_colors: None,
            colors_job: None,
            search: SearchBox {
                mode: prefs.search_mode,
                picked: prefs.search_mode,
                regex: prefs.search_regex,
                ignore_case: prefs.search_ignore_case,
                ..SearchBox::default()
            },
            filter_recall: recall::Recall::default(),
            chips_open: false,
            filter_preview: PreviewCache::default(),
            bookmarks: Bookmarks::default(),
            marks_changed: false,
            notes: NoteMode::default(),
            applied: None,
            marks_open: Default::default(),
            mark_rename: None,
            detail: None,
            signals: Signals::default(),
            signals_gen: 0,
            signals_ui: signals::SignalsUi::default(),
            signals_job: None,
            plot: Default::default(),
            signals_key: None,
            query_kind: Default::default(),
            query_scope: Default::default(),
            choices_gen: 0,
            views: signals::Views::default(),
            reopen_at: None,
            arg_picks: Vec::new(),
            files_edit: None,
            records: split,
            records_edit: None,
            files_undo: None,
            status: String::new(),
        }
    }

    fn title(&self) -> String {
        let name =
            |p: &Path| p.file_name().map_or_else(|| p.display().to_string(), |n| n.to_string_lossy().into_owned());
        match self.paths.len() {
            0 | 1 => name(&self.path),
            n => format!("{} + {}", name(&self.path), n - 1),
        }
    }

    /// Every file of the tab, one per line (hover text).
    fn files_text(&self) -> String {
        self.paths.iter().map(|p| p.display().to_string()).collect::<Vec<_>>().join("\n")
    }

    fn is_busy(&self) -> bool {
        self.loading.is_some()
            || self.searching.is_some()
            || self.search.job.is_some()
            || self.colors_job.is_some()
            || self.signals_job.is_some()
    }

    fn cancel_jobs(&mut self) {
        if let Some(j) = self.loading.take() {
            j.cancel();
        }
        if let Some((_, j)) = self.searching.take() {
            j.cancel();
        }
        if let Some((_, j)) = self.search.job.take() {
            j.cancel();
        }
        if let Some((_, j)) = self.colors_job.take() {
            j.cancel();
        }
        if let Some(j) = self.signals_job.take() {
            j.cancel();
        }
    }

    fn search_prefs(&self) -> SearchPrefs {
        SearchPrefs {
            mode: self.picked_mode,
            options: self.options.clone(),
            search_mode: self.search.picked,
            search_regex: self.search.regex,
            search_ignore_case: self.search.ignore_case,
            ..SearchPrefs::default()
        }
    }

    /// History tag for the current filter: `auto`, `text`, `regex` or `query`.
    fn filter_tag(&self) -> &'static str {
        match self.mode {
            Mode::Auto => "auto",
            Mode::Text if self.options.fixed_string => "text",
            Mode::Text => "regex",
            Mode::Query => "query",
        }
    }

    /// Set the filter from a history tag and pattern; unknown tags are ignored.
    fn set_filter(&mut self, tag: &str, pattern: &str) -> bool {
        match tag {
            "text" | "regex" => {
                self.mode = Mode::Text;
                self.options.fixed_string = tag == "text";
            }
            "query" => self.mode = Mode::Query,
            "auto" => self.mode = Mode::Auto,
            _ => return false,
        }
        self.pattern = pattern.to_owned();
        true
    }

    /// The tab's signals were edited: queries that use them run again.
    fn signals_changed(&mut self, ctx: &egui::Context) {
        self.signals_gen += 1;
        self.collect_signals(ctx);
        if self.pattern.contains('@') {
            self.start_search(ctx);
        } else if self.search.pattern.contains('@') {
            self.rerun_search(ctx);
        }
    }

    /// Plotted lanes whose signal the tab doesn't have but the FIBEX files
    /// describe (a plot kept in another tab): their arguments become the
    /// tab's signals, as ticking them in the Signals panel does. DLT tabs
    /// only. Returns whether any was picked.
    fn pick_plotted_arguments(&mut self, args: &[nerka_core::Argument]) -> bool {
        if args.is_empty() || !self.file.as_ref().is_some_and(|f| !f.columns().is_empty()) {
            return false;
        }
        let missing: Vec<String> = plot::lock(&self.plot)
            .plotted
            .iter()
            .filter(|n| self.signals.get(n).is_none() && !self.arg_picks.iter().any(|a| a.name == **n))
            .cloned()
            .collect();
        let found: Vec<_> = args.iter().filter(|a| missing.contains(&a.name)).cloned().collect();
        if found.is_empty() {
            return false;
        }
        self.arg_picks.extend(found);
        self.choices_gen += 1;
        true
    }

    /// Read the tab's files again (other FIBEX files): filter, search and
    /// selection stay, and so does the current line.
    fn reopen(&mut self, ctx: &egui::Context, prefs: &SearchPrefs, fibex: Option<Arc<nerka_core::Fibex>>) {
        let line = self.current_line();
        self.cancel_jobs();
        let ps = self.paths.clone();
        let mut options = prefs.open_options();
        options.fibex = fibex;
        options.records = self.records.clone();
        self.file = None;
        self.result = None;
        self.reopen_at = Some(line);
        self.loading = Some(Job::spawn(ctx, move |prog| nerka_core::open_many(&ps, &options, prog)));
    }

    /// Show `paths` in this tab instead (the Files window: reordered, added,
    /// removed). The tab keeps its filter, search, selection and signals;
    /// the view goes to the same line of the same file if it is still
    /// there. The current files stay shown until the new set is read, and
    /// stay if it can't be one view (files of different kinds).
    fn set_files(
        &mut self,
        ctx: &egui::Context,
        paths: Vec<PathBuf>,
        prefs: &SearchPrefs,
        fibex: Option<Arc<nerka_core::Fibex>>,
    ) {
        if paths.is_empty() || paths == self.paths {
            return;
        }
        let line = self.current_line();
        let at = self
            .part_files
            .iter()
            .position(|&(first, n)| line >= first && line < first + n)
            .and_then(|i| Some((self.paths.get(i)?.clone(), line - self.part_files[i].0)));
        if let Some(j) = self.loading.take() {
            j.cancel();
        }
        let ps = paths.clone();
        let old = std::mem::replace(&mut self.paths, paths);
        // A list changed twice before it was read goes back to the first.
        let old = self.files_undo.take().map_or(old, |u| u.old);
        self.files_undo = Some(FilesUndo { old, at });
        let mut options = prefs.open_options();
        options.fibex = fibex;
        options.records = self.records.clone();
        self.loading = Some(Job::spawn(ctx, move |prog| nerka_core::open_many(&ps, &options, prog)));
    }

    /// Build the tab's signals again if what they come from changed: the
    /// file's `@time`, the user's `saved` signals, the named groups of the
    /// selection's included and signal queries (with the kinds and scopes
    /// chosen here).
    fn refresh_signals(&mut self, ctx: &egui::Context, lib: &Library, saved: &Signals, saved_gen: u64) {
        let Some(file) = self.file.clone() else { return };
        let fingerprint = format!(
            "{:?}{:?}",
            self.picks
                .nodes
                .iter()
                .map(|n| (n.role, &n.text, &n.parent, &n.source, n.edited, n.detached, &n.signals))
                .collect::<Vec<_>>(),
            self.picks.params
        );
        let key = (saved_gen, self.choices_gen, fingerprint);
        if self.signals_key.as_ref() == Some(&key) {
            return;
        }
        self.signals_key = Some(key);
        let mut s = Signals::default().for_source(&*file);
        for d in saved.iter() {
            s.set((**d).clone());
        }
        // FIBEX arguments picked in the Signals panel.
        for a in &self.arg_picks {
            let query = match &a.ecu {
                Some(ecu) => format!("mid == {} AND ecu == \"{ecu}\"", a.mid),
                None => format!("mid == {}", a.mid),
            };
            let from = format!("FIBEX message {}: {}", a.mid, a.message);
            let kind = self.query_kind.get(&a.name).copied().unwrap_or(a.ty.kind());
            if s.get(&a.name).is_none()
                && let Ok(d) = nerka_core::SignalDef::argument(&a.name, kind, &query, a.index, &from)
            {
                s.set(d.with_scope(self.query_scope.get(&a.name).copied().unwrap_or_default()));
            }
        }
        if let Ok(a) = self.picks.expand(lib) {
            let parts: Vec<nerka_core::QueryPart<'_>> = a
                .exp
                .parts
                .iter()
                .enumerate()
                .filter(|(_, p)| p.role != Role::Exclude)
                .map(|(i, p)| nerka_core::QueryPart { name: &a.names[i], nfd: a.exp.part_nfd(p), pins: &a.pins[i] })
                .collect();
            let chosen = |g: &str| self.query_kind.get(g).copied();
            for d in nerka_core::query_signals(&parts, &*file, &chosen) {
                // A saved signal of the same name wins.
                if s.get(d.name()).is_none() {
                    let scope = self.query_scope.get(d.name()).copied().unwrap_or_default();
                    s.set(d.with_scope(scope));
                }
            }
        }
        self.signals = s;
        self.signals_changed(ctx);
    }

    /// Collect the events of every signal in the background, so the panel
    /// shows states without waiting for a filter to need them.
    fn collect_signals(&mut self, ctx: &egui::Context) {
        if let Some(j) = self.signals_job.take() {
            j.cancel();
        }
        let Some(file) = self.file.clone() else { return };
        if self.signals.is_empty() {
            return;
        }
        let defs: Vec<_> = self.signals.iter().cloned().collect();
        self.signals_job = Some(Job::spawn(ctx, move |prog| {
            for d in defs {
                // A definition that doesn't fit the file says so in the panel.
                let _ = d.series(&*file, prog);
            }
            Ok(())
        }));
    }

    fn build_filter(&self) -> Result<Filter, String> {
        let schema = self.file.as_ref().map(|f| f.schema()).unwrap_or_default();
        read_box(self.mode, &self.pattern, &self.options, schema, &self.signals).0
    }

    /// Start searching with the current filter; returns whether a search
    /// was started (so the caller can record it in the history).
    fn start_search(&mut self, ctx: &egui::Context) -> bool {
        let Some(file) = self.file.clone() else { return false };
        if let Some((_, j)) = self.searching.take() {
            j.cancel();
        }
        if self.filter_exp.as_ref().is_some_and(|a| a.exp.nfd != self.pattern) {
            self.filter_exp = None;
        }
        if self.pattern.trim().is_empty() {
            let cur = self.current_line();
            self.result = None;
            self.applied = None;
            self.goto_line(cur);
            self.status.clear();
            self.recolor(ctx);
            self.rerun_search(ctx);
            return false;
        }
        match self.build_filter() {
            Ok(f) => {
                self.run_filter(ctx, file, f.with_bookmarks(&self.marks()));
                true
            }
            Err(e) => {
                self.status = e;
                false
            }
        }
    }

    fn run_filter(&mut self, ctx: &egui::Context, file: Arc<dyn LogSource>, f: Filter) {
        let f = Arc::new(f);
        let ff = f.clone();
        let job = Job::spawn(ctx, move |prog| nerka_core::search(&*file, &ff, prog));
        self.searching = Some((f, job));
    }

    /// The tab's bookmarks as queries take them.
    fn marks(&self) -> Arc<Bookmarks> {
        Arc::new(self.bookmarks.clone())
    }

    /// The bookmarks changed: a filter, search or line colour that uses
    /// `bookmark` runs again with them (the filter's own, not the box's
    /// text, which may have been edited since).
    fn bookmarks_changed(&mut self, ctx: &egui::Context) {
        let marks = self.marks();
        let filter = match &self.searching {
            Some((f, _)) => Some(f.clone()),
            None => self.applied.clone(),
        };
        let refilter = filter.filter(|f| f.uses_bookmarks());
        if let Some(a) = self.search.active.clone().filter(|a| a.uses_bookmarks()) {
            self.search.active = Some(Arc::new(a.with_bookmarks(&marks)));
            if refilter.is_none() {
                self.rerun_search(ctx);
            }
        }
        if let (Some(f), Some(file)) = (refilter, self.file.clone()) {
            if let Some((_, j)) = self.searching.take() {
                j.cancel();
            }
            // The search and the colours follow the new result.
            self.run_filter(ctx, file, (*f).clone().with_bookmarks(&marks));
        } else if self.colors_exp.as_ref().is_some_and(|a| Query::compile(&a.exp.nfd).is_ok_and(|q| q.uses_bookmarks()))
        {
            self.recolor(ctx);
        }
    }

    /// Advance background jobs. Returns `false` if the file failed to open
    /// (the tab should then be closed; `status` holds the reason).
    fn poll_jobs(&mut self, ctx: &egui::Context) -> bool {
        if std::mem::take(&mut self.marks_changed) {
            self.bookmarks_changed(ctx);
        }
        if let Some(job) = &self.signals_job {
            match job.poll() {
                Poll::Running => ctx.request_repaint_after(Duration::from_millis(50)),
                Poll::Done(..) => self.signals_job = None,
            }
        }
        if let Some(job) = &self.loading {
            match job.poll() {
                Poll::Running => ctx.request_repaint_after(Duration::from_millis(50)),
                Poll::Done(r, took) => {
                    self.loading = None;
                    match r {
                        Ok(f) => {
                            if let Some(FilesUndo { at, .. }) = self.files_undo.take() {
                                // Other files: what referred to the old lines goes.
                                self.cancel_jobs();
                                self.path = self.paths[0].clone();
                                self.result = None;
                                self.applied = None;
                                self.line_colors = None;
                                self.search.hits = None;
                                self.detail = None;
                                self.main_view = ViewState::with_free_keys();
                                self.hits_view = ViewState::default();
                                let parts = f.part_files();
                                self.reopen_at = Some(
                                    at.and_then(|(path, off)| {
                                        let i = self.paths.iter().position(|p| *p == path)?;
                                        let (first, n) = *parts.get(i)?;
                                        Some(first + off.min(n.saturating_sub(1)))
                                    })
                                    .unwrap_or(0),
                                );
                            }
                            self.status = match self.paths.len() {
                                1 => format!("Indexed {} lines in {took:.2?}", f.line_count()),
                                n => format!("Indexed {} lines of {n} files in {took:.2?}", f.line_count()),
                            };
                            self.part_files = f.part_files();
                            self.bookmarks = self.load_bookmarks(&*f).unwrap_or_else(|e| {
                                self.status = format!("Could not read bookmarks: {e}");
                                Bookmarks::default()
                            });
                            self.columns = Columns::from_defs(f.columns());
                            self.file = Some(Arc::from(f));
                            // Built with the next frame (`@time` if the
                            // file knows its lines' times).
                            self.signals_key = None;
                            if let Some(line) = self.reopen_at.take() {
                                self.goto_line(line);
                            }
                            // Library picks put in the Search box before the
                            // file was read couldn't know its fields (`apid`):
                            // they run now (`--search-pick` at startup).
                            if self.search.active.is_none() && !self.search.terms.is_empty() {
                                self.run_search_box(ctx);
                            }
                            if !self.pattern.trim().is_empty() {
                                self.start_search(ctx);
                            } else {
                                self.recolor(ctx);
                                self.rerun_search(ctx);
                            }
                        }
                        // The Files window's list can't be read: the files
                        // shown stay.
                        Err(e) if self.files_undo.is_some() && !matches!(e, nerka_core::Error::Cancelled) => {
                            self.paths = self.files_undo.take().expect("checked").old;
                            self.status = format!("Files not changed: {e}");
                        }
                        Err(nerka_core::Error::Cancelled) => return false,
                        // Several files of different kinds: opened as tabs.
                        Err(nerka_core::Error::Format(why)) if self.paths.len() > 1 && why.contains("one kind") => {
                            self.mixed = Some(why);
                            return false;
                        }
                        Err(e) => {
                            let src = std::error::Error::source(&e).map(|s| format!(" ({s})")).unwrap_or_default();
                            self.status = format!("Cannot open {}: {e}{src}", self.path.display());
                            return false;
                        }
                    }
                }
            }
        }
        if let Some((f, job)) = &self.searching {
            let f = f.clone();
            match job.poll() {
                Poll::Running => ctx.request_repaint_after(Duration::from_millis(50)),
                Poll::Done(r, took) => {
                    match r {
                        Ok(res) => {
                            self.applied = Some(f);
                            self.status = format!("Filter keeps {} lines ({took:.2?})", res.lines.len());
                            let cur = self.current_line();
                            self.result = Some(Arc::new(res));
                            self.goto_line(cur);
                            self.searching = None;
                            // Colours and the search follow the filter result.
                            self.recolor(ctx);
                            self.rerun_search(ctx);
                        }
                        Err(nerka_core::Error::Cancelled) => self.searching = None,
                        Err(e) => {
                            self.status = format!("Filter failed: {e}");
                            self.searching = None;
                        }
                    }
                }
            }
        }
        if let Some((a, job)) = &self.colors_job {
            match job.poll() {
                Poll::Running => ctx.request_repaint_after(Duration::from_millis(50)),
                Poll::Done(r, _) => {
                    match r {
                        Ok(hits) => self.line_colors = Some((a.clone(), Arc::new(hits))),
                        Err(nerka_core::Error::Cancelled) => {}
                        Err(e) => self.status = format!("Colouring failed: {e}"),
                    }
                    self.colors_job = None;
                }
            }
        }
        if let Some((a, job)) = &self.search.job {
            match job.poll() {
                Poll::Running => ctx.request_repaint_after(Duration::from_millis(50)),
                Poll::Done(r, took) => {
                    match r {
                        Ok(hits) => {
                            let mut msg = format!("{} search hits in {took:.2?}", hits.len());
                            if a.names.len() > 1 {
                                let counts = hits.counts(a.names.len());
                                let per: Vec<String> =
                                    a.names.iter().zip(counts).map(|(n, c)| format!("{} {c}", short(n))).collect();
                                msg += &format!(" ({})", per.join(" · "));
                            }
                            self.status = msg;
                            self.search.hits = Some((a.clone(), Arc::new(hits)));
                            self.hits_view = ViewState::default();
                        }
                        Err(nerka_core::Error::Cancelled) => {}
                        Err(e) => self.status = format!("Search failed: {e}"),
                    }
                    self.search.job = None;
                }
            }
        }
        true
    }

    /// Apply the Selection pane: its queries, expanded, go into the Filter
    /// bar and the filter runs. Returns the text to record in the history.
    fn apply_selection(&mut self, ctx: &egui::Context, lib: &Library) -> Option<String> {
        self.apply_filter_picks(ctx, lib)
    }

    /// Put the selection's filter queries, expanded, into the Filter bar
    /// and run it. Returns the text to record in the history.
    fn apply_filter_picks(&mut self, ctx: &egui::Context, lib: &Library) -> Option<String> {
        match self.picks.expand(lib) {
            Ok(a) => {
                self.picks.error = None;
                self.mode = Mode::Query;
                self.pattern = a.exp.nfd.clone();
                let a = Arc::new(a);
                self.colors_exp = Some(a.clone());
                self.filter_exp = Some(a);
                self.start_search(ctx).then(|| self.pattern.clone())
            }
            Err(e) => {
                self.picks.error = Some(format!("Filter: {e}"));
                None
            }
        }
    }

    /// 🔍 on a query: replace the Search box with it, or add it (OR). When
    /// adding to typed text, the text becomes a query first, so nothing in
    /// the box is lost.
    fn search_for(&mut self, ctx: &egui::Context, lib: &Library, target: &Target, add: bool) {
        let mut terms: Vec<Term> = if add {
            match self.search.current_terms() {
                Some(t) => t.to_vec(),
                None => self.search.typed_term().into_iter().collect(),
            }
        } else {
            Vec::new()
        };
        match self.picks.search_term(lib, target, terms.len()) {
            Ok(t) => {
                terms.push(t);
                self.picks.error = None;
                self.search.mode = Mode::Query;
                self.search.pattern = SearchBox::text_of(&terms);
                self.search.terms = terms;
                self.run_search_box(ctx);
            }
            Err(e) => self.picks.error = Some(format!("Search: {e}")),
        }
    }

    /// A search chip's click: the box without its `i`-th query (OR'ed with
    /// the others), searched again; empty, the search is cleared.
    fn remove_search_term(&mut self, ctx: &egui::Context, i: usize) {
        let Some(terms) = self.search.current_terms() else { return };
        let mut terms = terms.to_vec();
        if i < terms.len() {
            terms.remove(i);
        }
        if terms.is_empty() {
            self.clear_search();
            return;
        }
        self.search.pattern = SearchBox::text_of(&terms);
        self.search.terms = terms;
        self.run_search_box(ctx);
    }

    /// What the Search box asks for: one query per picked library query, or
    /// the box's text as one query.
    fn build_search(&self) -> Result<ActiveSearch, String> {
        if let Some(terms) = self.search.current_terms() {
            let schema = self.file.as_ref().map(|f| f.schema()).unwrap_or_default();
            let include = terms
                .iter()
                .map(|t| {
                    Query::compile_in(&t.nfd, schema, &self.signals)
                        .map(Filter::Query)
                        .map_err(|e| format!("Search: {}: {e}", t.name))
                })
                .collect::<Result<Vec<_>, _>>()?;
            let highlighters = include.iter().map(Filter::highlighter).collect();
            return Ok(ActiveSearch {
                set: SearchSet { include, exclude: vec![] },
                names: terms.iter().map(|t| t.name.clone()).collect(),
                colors: terms.iter().map(|t| t.color).collect(),
                highlighters,
            });
        }
        let schema = self.file.as_ref().map(|f| f.schema()).unwrap_or_default();
        let f = read_box(self.search.mode, &self.search.pattern, &self.search.options(), schema, &self.signals)
            .0
            .map_err(|e| format!("Search: {e}"))?;
        Ok(ActiveSearch {
            highlighters: vec![f.highlighter()],
            set: SearchSet { include: vec![f], exclude: vec![] },
            names: vec![self.search.pattern.clone()],
            colors: vec![library::palette(0)],
        })
    }

    /// Run the Search box (Enter, Find, or library picks).
    fn run_search_box(&mut self, ctx: &egui::Context) {
        if self.search.pattern.trim().is_empty() {
            self.clear_search();
            return;
        }
        match self.build_search() {
            Ok(a) => {
                self.search.active = Some(Arc::new(a.with_bookmarks(&self.marks())));
                self.rerun_search(ctx);
            }
            Err(e) => self.status = e,
        }
    }

    /// Issue #16: the Clear button. Empties the Filter box, so the main
    /// view shows every line again (↑ in the box brings the text back).
    fn clear_filter(&mut self, ctx: &egui::Context) {
        self.pattern.clear();
        self.start_search(ctx);
    }

    fn clear_search(&mut self) {
        if let Some((_, j)) = self.search.job.take() {
            j.cancel();
        }
        self.search.active = None;
        self.search.hits = None;
        self.search.terms.clear();
        self.search.pattern.clear();
    }

    /// Work out (again) the whole-line colours: for the filter queries
    /// switched on, which kept lines each one matches (every line without
    /// a filter: issue #8, a selection of only signal queries). Runs after
    /// the filter result is in, and when a colour is switched on or off.
    fn recolor(&mut self, ctx: &egui::Context) {
        if let Some((_, j)) = self.colors_job.take() {
            j.cancel();
        }
        self.line_colors = None;
        let (Some(a), Some(file)) = (self.colors_exp.clone(), self.file.clone()) else {
            return;
        };
        let res = self.result.clone();
        if self.searching.is_some() {
            return;
        }
        let marks = self.marks();
        let (mut include, mut names, mut colors) = (Vec::new(), Vec::new(), Vec::new());
        for (i, p) in a.exp.parts.iter().enumerate() {
            if p.role == Role::Exclude || !self.picks.is_lit(a.keys[i]) {
                continue;
            }
            let Ok(q) = Query::compile(a.exp.part_nfd(p)).and_then(|q| q.with_signals(&self.signals)) else { continue };
            include.push(Filter::Query(q.with_bookmarks(&marks)));
            names.push(a.names[i].clone());
            colors.push(a.colors[i]);
        }
        if include.is_empty() {
            return;
        }
        let a =
            Arc::new(ActiveSearch { set: SearchSet { include, exclude: vec![] }, names, colors, highlighters: vec![] });
        let aa = a.clone();
        let job = Job::spawn(ctx, move |prog| {
            nerka_core::search_within(&*file, res.as_ref().map(|r| r.lines.as_slice()), &aa.set, prog)
        });
        self.colors_job = Some((a, job));
    }

    /// This tab as a preset: the selection, parameters, and the boxes'
    /// typed text (the Filter box as "the selection" when it holds the
    /// selection's expansion; the Search box only when typed).
    fn to_preset(&self, name: &str) -> Preset {
        let mut p = self.picks.to_preset(name);
        p.filter = if self.filter_exp.as_ref().is_some_and(|a| a.exp.nfd == self.pattern) {
            Some(BoxText::Selection)
        } else if !self.pattern.trim().is_empty() {
            Some(BoxText::Typed { tag: self.filter_tag().to_owned(), text: self.pattern.trim().to_owned() })
        } else {
            None
        };
        p.search = (self.search.current_terms().is_none() && !self.search.pattern.trim().is_empty())
            .then(|| BoxText::Typed { tag: self.search.tag().to_owned(), text: self.search.pattern.trim().to_owned() });
        p
    }

    /// Use a preset: its selection replaces this tab's, then the Filter and
    /// Search boxes get its text and run.
    fn use_preset(&mut self, ctx: &egui::Context, lib: &Library, p: &Preset) {
        self.picks.use_preset(lib, p);
        self.colors_exp = self.picks.expand(lib).ok().map(Arc::new);
        self.filter_exp = None;
        match &p.filter {
            Some(BoxText::Selection) => {
                let err = self.picks.error.take();
                self.apply_filter_picks(ctx, lib);
                self.picks.error = self.picks.error.take().or(err);
            }
            Some(BoxText::Typed { tag, text }) => {
                self.set_filter(tag, text);
                self.start_search(ctx);
            }
            None => {
                self.pattern.clear();
                self.start_search(ctx);
            }
        }
        match &p.search {
            Some(BoxText::Typed { tag, text }) => {
                self.search.terms.clear();
                self.search.set(tag, text);
                self.run_search_box(ctx);
            }
            _ => self.clear_search(),
        }
        self.preset = Some(p.name.clone());
    }

    /// The selection changed (a colour switched, a node added or edited):
    /// expand it again for the line colours and recolour. A selection that
    /// doesn't expand keeps the last colours.
    fn recolor_from(&mut self, ctx: &egui::Context, lib: &Library) {
        if let Ok(a) = self.picks.expand(lib) {
            self.colors_exp = Some(Arc::new(a));
        }
        self.recolor(ctx);
    }

    /// Start (again) the active search over the current filter result.
    /// Waits while the file loads or the filter runs; they call it again.
    fn rerun_search(&mut self, ctx: &egui::Context) {
        let (Some(a), Some(file)) = (self.search.active.clone(), self.file.clone()) else { return };
        if let Some((_, j)) = self.search.job.take() {
            j.cancel();
        }
        if self.searching.is_some() {
            return;
        }
        let within = self.result.clone();
        let aa = a.clone();
        let job = Job::spawn(ctx, move |prog| {
            nerka_core::search_within(&*file, within.as_ref().map(|r| r.lines.as_slice()), &aa.set, prog)
        });
        self.search.job = Some((a, job));
    }

    fn total_lines(&self) -> LineNo {
        self.file.as_ref().map_or(0, |f| f.line_count())
    }

    /// Rows of the main view: the lines the filter keeps, or all lines.
    fn main_rows(&self) -> u64 {
        self.result.as_ref().map_or(self.total_lines(), |r| r.lines.len() as u64)
    }

    fn line_of_row(&self, row: u64) -> LineNo {
        match &self.result {
            Some(r) => r.lines.get(row as usize).copied().unwrap_or(0),
            None => row,
        }
    }

    /// The main-view row of `line`, or of the next kept line after it (the
    /// last one if there is none).
    fn row_of_line(&self, line: LineNo) -> u64 {
        match &self.result {
            Some(r) => (r.position_at_or_after(line) as u64).min((r.lines.len() as u64).saturating_sub(1)),
            None => line.min(self.total_lines().saturating_sub(1)),
        }
    }

    fn current_line(&self) -> LineNo {
        self.line_of_row(self.main_view.selected.unwrap_or(self.main_view.top))
    }

    /// Centre the main view on `line`, or the nearest kept line after it.
    fn goto_line(&mut self, line: LineNo) {
        let row = self.row_of_line(line);
        self.main_view.center_on(row, self.main_rows());
    }

    /// Jump to the next/previous search hit after the main view's cursor.
    fn jump_match(&mut self, forward: bool) {
        let Some((_, hits)) = self.search.hits.clone() else { return };
        if hits.is_empty() {
            return;
        }
        let cur = self.current_line();
        let i = hits.position_at_or_after(if forward { cur + 1 } else { cur });
        let i = if forward { i.min(hits.len() - 1) } else { i.saturating_sub(1) };
        self.goto_line(hits.lines[i]);
        self.hits_view.select(i as u64, hits.len() as u64);
    }

    fn jump_bookmark(&mut self, forward: bool) {
        let cur = self.current_line();
        let next = if forward { self.bookmarks.next_after(cur) } else { self.bookmarks.prev_before(cur) };
        if let Some(l) = next {
            self.goto_line(l);
        }
    }

    fn toggle_bookmark(&mut self) {
        self.toggle_bookmark_at(self.current_line());
    }

    fn toggle_bookmark_at(&mut self, line: LineNo) {
        if self.file.is_none() {
            return;
        }
        let on = self.bookmarks.toggle(line);
        self.status = format!("Bookmark {} line {}", if on { "added on" } else { "removed from" }, line + 1);
        self.save_bookmarks();
    }

    /// The tab's signals on `line` for the line window: events (the line
    /// carries them) first, then the others' state there.
    ///
    /// `args` are the FIBEX catalog's arguments: a non-verbose message it
    /// describes lists all of its own arguments with their values, also
    /// those not used as signals.
    fn line_signals(&mut self, line: LineNo, args: &[nerka_core::Argument]) -> Vec<LineSignal> {
        let Some(file) = self.file.clone() else { return Vec::new() };
        let kept = self.result.clone();
        let mut out: Vec<LineSignal> = Vec::new();
        for d in self.signals.iter() {
            let origin = if d.is_from_source() {
                "built in"
            } else if d.argument_index().is_some() {
                "FIBEX"
            } else if !d.queries().is_empty() {
                "query"
            } else {
                "saved"
            };
            let base =
                LineSignal { name: d.name().to_owned(), kind: d.kind().as_str(), origin, value: None, from: None };
            if d.is_from_source() {
                let value = file.time(line).map(|t| nerka_core::time::format_time(t, 6));
                out.push(LineSignal { from: value.as_ref().map(|_| line), value, ..base });
                continue;
            }
            let Some(series) = self.views.series(d, &*file, kept.as_ref(), self.signals_gen) else {
                out.push(LineSignal { value: Some("…".into()), ..base });
                continue;
            };
            match series.state(line) {
                Some((row, v)) => out.push(LineSignal { value: Some(d.format(v)), from: Some(row), ..base }),
                None => out.push(base),
            }
        }
        // The message's own FIBEX arguments that aren't signals of the tab.
        let mid = file.schema().iter().position(|f| f.name == "mid").and_then(|id| match file.field(line, id) {
            Some(nerka_core::FieldValue::U64(m)) => u32::try_from(m).ok(),
            _ => None,
        });
        let ecu = file.schema().iter().position(|f| f.name == "ecu").and_then(|id| match file.field(line, id) {
            Some(nerka_core::FieldValue::Str(e)) => Some(String::from_utf8_lossy(&e).into_owned()),
            _ => None,
        });
        if let Some(mid) = mid {
            let own = args
                .iter()
                .filter(|a| a.mid == mid && a.ecu.as_ref().is_none_or(|e| Some(e) == ecu.as_ref()))
                .filter(|a| self.signals.get(&a.name).is_none());
            for a in own {
                let value = file.argument(line, a.index).map(|v| v.to_string());
                out.push(LineSignal {
                    name: a.name.clone(),
                    kind: a.ty.kind().as_str(),
                    origin: "FIBEX, not used",
                    from: value.as_ref().map(|_| line),
                    value,
                });
            }
        }
        // Events first; otherwise the tab's order.
        out.sort_by_key(|s| s.from != Some(line));
        out
    }

    /// Bookmark `line` with `note` (a bookmark already there gets the note).
    fn set_bookmark(&mut self, line: LineNo, note: String) {
        if !self.bookmarks.contains(line) {
            self.bookmarks.toggle(line);
        }
        self.bookmarks.set_note(line, note);
        self.status = format!("Bookmark on line {}", line + 1);
        self.save_bookmarks();
    }

    /// Each file keeps its own sidecar, anchored on its own lines; a view
    /// of several maps lines.
    fn load_bookmarks(&self, file: &dyn LogSource) -> std::io::Result<Bookmarks> {
        let Some(parts) = file.parts() else { return Bookmarks::load_for(&self.path, file) };
        let files: Vec<(&Path, LineNo, &dyn LogSource)> =
            self.paths.iter().zip(&parts).map(|(p, part)| (p.as_path(), part.first, part.source.as_ref())).collect();
        Bookmarks::load_for_parts(&files)
    }

    fn save_bookmarks(&mut self) {
        let Some(file) = self.file.clone() else { return };
        let res = match file.parts() {
            None => self.bookmarks.save_for(&self.path, &*file),
            Some(parts) => {
                let files: Vec<(&Path, LineNo, LineNo, &dyn LogSource)> = self
                    .paths
                    .iter()
                    .zip(&parts)
                    .map(|(p, part)| (p.as_path(), part.first, part.source.line_count(), part.source.as_ref()))
                    .collect();
                self.bookmarks.save_for_parts(&files)
            }
        };
        if let Err(e) = res {
            self.status = format!("Could not save bookmarks: {e}");
        }
        self.marks_changed = true;
    }

    /// Status messages and context-menu actions from a log view.
    /// `hits`: the output is the search hits list's, else the main view's.
    fn view_output(&mut self, out: &ViewOutput, hits: bool, line_of_row: impl Fn(u64) -> LineNo) {
        if let Some((n, cut)) = out.copied {
            self.status = if cut {
                format!("Copied the first {n} lines (copy is limited to {n} lines)")
            } else {
                format!("Copied {n} line{}", if n == 1 { "" } else { "s" })
            };
        }
        if let Some(row) = out.toggle_bookmark {
            self.toggle_bookmark_at(line_of_row(row));
        }
        if let Some(row) = out.detail
            && let Some(file) = &self.file
        {
            self.detail = LineDetail::of(&**file, line_of_row(row)).map(|d| LineDetail { in_hits: hits, ..d });
        }
        // An open line window follows the line selected in either view
        // (owner, 2026-10-24), keeping its Wrap setting.
        if let (Some(row), Some(d), Some(file)) = (out.moved_to, &self.detail, &self.file)
            && out.detail.is_none()
            && line_of_row(row) != d.line
            && let Some(next) = d.moved(&**file, line_of_row(row))
        {
            self.detail = Some(LineDetail { in_hits: hits, ..next });
        }
    }

    /// ↑ / ↓ in the line window: the next or previous line of the view it
    /// follows (the search hits, else the main view), which moves there too.
    fn step_detail(&mut self, down: bool) {
        let Some(d) = &self.detail else { return };
        let hits = self.search.hits.clone().filter(|(_, h)| d.in_hits && !h.is_empty());
        let line = if let Some((_, hits)) = hits {
            let n = hits.len() as u64;
            let i = hits.position_at_or_after(d.line) as u64;
            let i = if down { (i + 1).min(n - 1) } else { i.saturating_sub(1) };
            self.hits_view.select(i, n);
            let line = hits.lines[i as usize];
            self.goto_line(line);
            line
        } else {
            let rows = self.main_rows();
            if rows == 0 {
                return;
            }
            let row = self.row_of_line(d.line);
            let row = if down { (row + 1).min(rows - 1) } else { row.saturating_sub(1) };
            self.main_view.select(row, rows);
            self.line_of_row(row)
        };
        if let Some(file) = &self.file
            && let Some(next) = self.detail.as_ref().and_then(|d| d.moved(&**file, line))
        {
            self.detail = Some(next);
        }
    }

    /// Returns true when the panel's close button was clicked.
    /// `notes`: where the notes show in the views (the app's setting).
    fn bookmarks_panel(&mut self, ui: &mut egui::Ui, file: &dyn LogSource, notes: &mut NoteMode) -> bool {
        let mut close = false;
        ui.horizontal(|ui| {
            ui.heading("Bookmarks");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                close = close_button(ui).on_hover_text("Close panel").clicked();
            });
        });
        ui.horizontal(|ui| {
            ui.label("Notes in the views:").on_hover_text(
                "Where a bookmark's note shows next to its line. Column: a last column in tables (DLT);                  plain text logs show it on the line before.",
            );
            egui::ComboBox::from_id_salt("note-mode").selected_text(notes.label()).show_ui(ui, |ui| {
                for m in NoteMode::ALL {
                    ui.selectable_value(notes, m, m.label());
                }
            });
        });
        self.bookmarks_list(ui, file);
        close
    }

    fn bookmarks_list(&mut self, ui: &mut egui::Ui, file: &dyn LogSource) {
        ui.weak("m toggle · b / B next / prev");
        ui.separator();
        if self.bookmarks.is_empty() {
            ui.weak("No bookmarks yet.");
            return;
        }
        let mut jump = None;
        let mut edits: Vec<(LineNo, String)> = Vec::new();
        let mut remove = None;
        let mut rename = None;
        let mut fold = None;
        egui::ScrollArea::vertical().show(ui, |ui| {
            for (line, name, note) in self.bookmarks.iter_named() {
                let open = self.marks_open.contains(&line);
                // Right-to-left, as the note row below: the name field takes
                // the width left after the link and the fold button, so it
                // grows with the panel (issue #5).
                let row = egui::vec2(ui.available_width(), ui.spacing().interact_size.y);
                ui.allocate_ui_with_layout(row, egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let tip = if open { "Fold the line" } else { "Show the whole line" };
                    if ui.small_button(if open { "▾" } else { "▸" }).on_hover_text(tip).clicked() {
                        fold = Some(line);
                    }
                    if ui.link(format!("line {}", line + 1)).on_hover_text("Go to the line").clicked() {
                        jump = Some(line);
                    }
                    if self.bookmarks.is_unsure(line) {
                        ui.colored_label(ui.visuals().warn_fg_color, "?").on_hover_text(UNSURE_MARK);
                    }
                    // The name (the plot shows it); edited in place.
                    let mut text = match &self.mark_rename {
                        Some((l, t)) if *l == line => t.clone(),
                        _ => name.to_owned(),
                    };
                    let r = ui.add(
                        egui::TextEdit::singleline(&mut text)
                            .font(egui::TextStyle::Monospace)
                            .desired_width(ui.available_width()),
                    );
                    if r.changed() {
                        self.mark_rename = Some((line, text));
                    }
                    if r.lost_focus() && self.mark_rename.as_ref().is_some_and(|(l, _)| *l == line) {
                        rename = self.mark_rename.take();
                    }
                    r.on_hover_text("The bookmark's name (the plot shows it); type to rename");
                });
                // The line wrapped to two rows ("…" if longer); unfolded, all
                // of it. Wrapping uses the width the panel has, so it never
                // makes the panel grow.
                let text = bookmark_text(&file.line_text(line).unwrap_or_default());
                let font = egui::FontId::monospace(11.5);
                let mut job = egui::text::LayoutJob::single_section(
                    text,
                    egui::TextFormat { font_id: font, color: ui.visuals().weak_text_color(), ..Default::default() },
                );
                job.wrap = egui::text::TextWrapping {
                    max_width: ui.available_width(),
                    max_rows: if open { usize::MAX } else { BOOKMARK_ROWS },
                    break_anywhere: true,
                    overflow_character: Some('…'),
                };
                let r = ui.add(egui::Label::new(job).sense(egui::Sense::click()));
                if r.on_hover_text(if open { "Click to fold" } else { "Click to show the whole line" }).clicked() {
                    fold = Some(line);
                }
                // Right-to-left so the note field takes exactly the width left
                // after the button. A guessed width overshoots by the item
                // spacing, and a resizable panel grows to fit its content
                // every frame until it fills the window.
                let row = egui::vec2(ui.available_width(), ui.spacing().interact_size.y);
                ui.allocate_ui_with_layout(row, egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if close_button(ui).on_hover_text("Remove").clicked() {
                        remove = Some(line);
                    }
                    let mut text = note.to_owned();
                    let r = ui.add(
                        egui::TextEdit::singleline(&mut text).hint_text("note…").desired_width(ui.available_width()),
                    );
                    if r.changed() {
                        edits.push((line, text));
                    }
                });
                ui.add_space(4.0);
            }
        });
        if let Some(l) = fold
            && !self.marks_open.remove(&l)
        {
            self.marks_open.insert(l);
        }
        let changed = !edits.is_empty() || rename.is_some() || remove.is_some();
        for (l, t) in edits {
            self.bookmarks.set_note(l, t);
        }
        if let Some((l, t)) = rename {
            // Left empty: a free name again.
            self.bookmarks.set_name(l, t);
        }
        if let Some(l) = remove {
            self.bookmarks.toggle(l);
        }
        if changed {
            self.save_bookmarks();
        }
        if let Some(l) = jump {
            self.goto_line(l);
        }
    }

    /// The Filter bar at the top. Returns the filter that was applied, if
    /// one started.
    fn filter_bar(
        &mut self,
        ui: &mut egui::Ui,
        focus_search: &mut bool,
        history: &SearchHistory,
    ) -> Option<(&'static str, String)> {
        let mut searched = None;
        ui.horizontal(|ui| {
            let (mut go, mut clear) = (false, false);
            ui.add_enabled_ui(!history.is_empty(), |ui| {
                ui.menu_button("🕘", |ui| {
                    egui::ScrollArea::vertical().max_height(400.0).show(ui, |ui| {
                        for (tag, pattern) in history.iter() {
                            let label = egui::RichText::new(format!("{tag:<5}  {pattern}")).monospace();
                            if ui.add(egui::Button::new(label).wrap_mode(egui::TextWrapMode::Truncate)).clicked() {
                                // Fills the box only; Enter or Search runs it.
                                self.set_filter(tag, pattern);
                                *focus_search = true;
                                ui.close();
                            }
                        }
                    });
                })
                .response
                .on_hover_text("Recent filters (or ↑ / ↓ in the box)");
            });
            ui.label(RichText::new("Filter").strong())
                .on_hover_text("Decides which lines the main view shows; Enter applies it");
            // Mode and checkboxes only change how the box is read; only
            // Enter or the Filter button runs it.
            if mode_combo(ui, "filter-mode", &mut self.mode) {
                self.picked_mode = self.mode;
            }
            let hint = match self.mode {
                Mode::Auto => "filter: text, or level >= ERROR AND timeout — Enter to apply",
                Mode::Text if self.options.fixed_string => "filter: lines to keep (text) — Enter to apply",
                Mode::Text => "filter: lines to keep (regex) — Enter to apply",
                Mode::Query => "filter: level >= ERROR AND raw ~ /SSL/ — Enter to apply",
            };
            // Right to left: buttons first, the text box takes what is left,
            // so the row never grows wider than the panel.
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                go |= ui.button("Filter").on_hover_text("Show only the matching lines in the main view").clicked();
                let on = self.result.is_some() || self.searching.is_some();
                clear = ui
                    .add_enabled(on, egui::Button::new("Clear"))
                    .on_hover_text("Show every line again (↑ in the box brings the filter back)")
                    .clicked();
                if self.mode != Mode::Query {
                    ui.checkbox(&mut self.options.invert, "Invert");
                    ui.checkbox(&mut self.options.case_insensitive, "Ignore case")
                        .on_hover_text("For the text; a /regex/ or \"string\" written out says itself");
                }
                if self.mode != Mode::Query {
                    let mut regex = !self.options.fixed_string;
                    if ui.checkbox(&mut regex, "Regex").changed() {
                        self.options.fixed_string = !regex;
                    }
                }
                let r = ui.add(
                    egui::TextEdit::singleline(&mut self.pattern)
                        .hint_text(hint)
                        .font(egui::TextStyle::Monospace)
                        .desired_width(ui.available_width()),
                );
                if *focus_search {
                    r.request_focus();
                    *focus_search = false;
                }
                let tag = self.filter_tag();
                if let Some((tag, p)) = recall::keys(ui, &r, &mut self.filter_recall, history, (tag, &self.pattern)) {
                    self.set_filter(&tag, &p);
                }
                go |= r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
            });
            if clear {
                self.clear_filter(ui.ctx());
            } else if go && self.start_search(ui.ctx()) {
                searched = Some((self.filter_tag(), self.pattern.clone()));
            }
        });
        if let Some(a) = self.filter_exp.clone().filter(|a| a.exp.nfd == self.pattern) {
            let picks = &self.picks;
            let lines = expansion_lines(&a, &|k| picks.is_lit(k));
            if let Some(ChipClick::Lit(key)) = chips_view(ui, &lines, &mut self.chips_open) {
                self.picks.toggle_lit(key);
                self.recolor(ui.ctx());
            }
        } else {
            let schema = self.file.as_ref().map(|f| f.schema()).unwrap_or_default();
            let sig = (&self.signals, self.signals_gen);
            let preview = self.filter_preview.get(self.mode, &self.pattern, &self.options, schema, sig);
            if preview_ui(ui, &preview) {
                self.mode = Mode::Text;
                self.options.fixed_string = true;
                if self.start_search(ui.ctx()) {
                    searched = Some((self.filter_tag(), self.pattern.clone()));
                }
            }
        }
        searched
    }

    /// The Search box and the list of its hits (bottom panel). Returns the
    /// search that was run from the box, to record in its history.
    fn search_panel(
        &mut self,
        ui: &mut egui::Ui,
        file: &dyn LogSource,
        history: &SearchHistory,
    ) -> Option<(&'static str, String)> {
        let searched = self.search_row(ui, history);
        ui.add_space(2.0);
        let marks = self.search.hits.as_ref().map(|(a, h)| SearchMarks {
            hits: h,
            colors: &a.colors,
            highlighters: &a.highlighters,
        });
        let lines =
            self.line_colors.as_ref().map(|(a, h)| SearchMarks { hits: h, colors: &a.colors, highlighters: &[] });
        if let Some((_, hits)) = self.search.hits.clone() {
            let out = log_view(
                ui,
                "hits",
                &mut self.hits_view,
                hits.len() as u64,
                &|row| hits.lines[row as usize],
                file,
                &mut self.columns,
                &Highlight {
                    highlighter: None,
                    bookmarks: Some(&self.bookmarks),
                    search: marks.as_ref(),
                    // The filter queries' line colours, as in the main view.
                    lines: lines.as_ref(),
                    notes: self.notes,
                },
            );
            self.view_output(&out, true, |row| hits.lines[row as usize]);
            // Selecting a hit (a click, the keys) moves the main view to it;
            // only then: a hit selected earlier doesn't pull the main view
            // back (it still has the focus while a click there is pressed).
            let picked = out.clicked.or(out.moved_to);
            if let Some(row) = picked {
                let line = hits.lines[row as usize];
                if self.current_line() != line {
                    self.goto_line(line);
                }
            }
        } else {
            ui.weak(if self.result.is_some() {
                "No search. Type in the Search box to find lines among the filtered ones."
            } else {
                "No search. Type in the Search box to find lines."
            });
            // Fill the pane: panels remember their content size.
            ui.allocate_space(ui.available_size());
        }
        searched
    }

    /// The Search box, under the Filter box: same modes, Enter runs it.
    /// Returns the search it ran, to record in the history.
    fn search_row(&mut self, ui: &mut egui::Ui, history: &SearchHistory) -> Option<(&'static str, String)> {
        let mut go = false;
        let mut clear = false;
        ui.horizontal(|ui| {
            ui.label("🔍").on_hover_text(
                "Search the lines in the main view; hits are listed below and coloured, n / N jump between them",
            );
            ui.add_enabled_ui(!history.is_empty(), |ui| {
                ui.menu_button("🕘", |ui| {
                    egui::ScrollArea::vertical().max_height(400.0).show(ui, |ui| {
                        for (tag, pattern) in history.iter() {
                            let label = egui::RichText::new(format!("{tag:<5}  {pattern}")).monospace();
                            if ui.add(egui::Button::new(label).wrap_mode(egui::TextWrapMode::Truncate)).clicked() {
                                // Fills the box only; Enter or Find runs it.
                                self.search.set(tag, pattern);
                                ui.close();
                            }
                        }
                    });
                })
                .response
                .on_hover_text("Recent searches (or ↑ / ↓ in the box)");
            });
            if mode_combo(ui, "search-mode", &mut self.search.mode) {
                self.search.picked = self.search.mode;
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                go |= ui.button("Find").clicked();
                clear = ui.add_enabled(self.search.active.is_some(), egui::Button::new("Clear")).clicked();
                if self.search.mode != Mode::Query {
                    ui.checkbox(&mut self.search.ignore_case, "Ignore case");
                }
                if self.search.mode != Mode::Query {
                    ui.checkbox(&mut self.search.regex, "Regex")
                        .on_hover_text("Text, and Auto's plain words, as a regex (mix with NFD: /…/)");
                }
                let hint = match self.search.mode {
                    Mode::Auto => "search: text, or level >= ERROR AND timeout — Enter to find",
                    Mode::Text => "search: lines to find among the filtered ones — Enter to find",
                    Mode::Query => "NFD query, or tick queries in the library — Enter to find",
                };
                let r = ui.add(
                    egui::TextEdit::singleline(&mut self.search.pattern)
                        .hint_text(hint)
                        .font(egui::TextStyle::Monospace)
                        .desired_width(ui.available_width()),
                );
                let tag = self.search.tag();
                let pattern = self.search.pattern.clone();
                if let Some((tag, p)) = recall::keys(ui, &r, &mut self.search.recall, history, (tag, &pattern)) {
                    self.search.set(&tag, &p);
                }
                go |= r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
            });
        });
        let mut searched = None;
        if clear {
            self.clear_search();
        } else if go {
            self.run_search_box(ui.ctx());
            if self.search.active.is_some() {
                searched = Some((self.search.tag(), self.search.pattern.clone()));
            }
        }
        if let Some(terms) = self.search.current_terms() {
            let lines = term_lines(terms);
            if let Some(ChipClick::Remove(i)) = chips_view(ui, &lines, &mut self.chips_open) {
                self.remove_search_term(ui.ctx(), i);
            }
        } else {
            let schema = self.file.as_ref().map(|f| f.schema()).unwrap_or_default();
            let s = &mut self.search;
            let preview = s.preview.get(s.mode, &s.pattern, &s.options(), schema, (&self.signals, self.signals_gen));
            if preview_ui(ui, &preview) {
                self.search.mode = Mode::Text;
                self.search.regex = false;
                self.run_search_box(ui.ctx());
                if self.search.active.is_some() {
                    searched = Some((self.search.tag(), self.search.pattern.clone()));
                }
            }
        }
        searched
    }

    fn main_panel(&mut self, ui: &mut egui::Ui, file: &dyn LogSource) {
        let marks = self.search.hits.as_ref().map(|(a, h)| SearchMarks {
            hits: h,
            colors: &a.colors,
            highlighters: &a.highlighters,
        });
        let lines =
            self.line_colors.as_ref().map(|(a, h)| SearchMarks { hits: h, colors: &a.colors, highlighters: &[] });
        let result = self.result.clone();
        let line_of_row = |row: u64| match &result {
            Some(r) => r.lines[row as usize],
            None => row,
        };
        let rows = self.main_rows();
        let out = log_view(
            ui,
            "main",
            &mut self.main_view,
            rows,
            &line_of_row,
            file,
            &mut self.columns,
            &Highlight {
                // Every row matches the filter: only search hits are coloured.
                highlighter: None,
                bookmarks: Some(&self.bookmarks),
                search: marks.as_ref(),
                lines: lines.as_ref(),
                notes: self.notes,
            },
        );
        self.view_output(&out, false, line_of_row);
    }
}

/// Why a bookmark shows a `?`.
const UNSURE_MARK: &str = "Not found where it was saved: the file changed, or a capture was read with other \
                           ports. It stays on its old row, which may be another line now. Edit its name or note \
                           to keep it here, or remove it.";

/// Rows of a bookmark's line shown while it is folded.
const BOOKMARK_ROWS: usize = 2;
/// Characters of a bookmark's line shown at most (unfolded).
const BOOKMARK_CHARS: usize = 2000;

/// A bookmarked line for the panel: tabs (DLT columns) as spaces, cut at
/// [`BOOKMARK_CHARS`].
fn bookmark_text(line: &str) -> String {
    let mut s: String = line.chars().take(BOOKMARK_CHARS).map(|c| if c == '\t' { ' ' } else { c }).collect();
    if line.chars().nth(BOOKMARK_CHARS).is_some() {
        s.push('…');
    }
    s
}

/// `pack.query` -> `query`.
fn short(id: &str) -> &str {
    id.split_once('.').map_or(id, |(_, q)| q)
}

/// One picked query under a box: how it joins the others, its name chip
/// and colour, its NFD, (filter queries) the key of its selection node
/// with whether its lines are coloured, and (search queries) whether a
/// click removes it from the box.
struct ChipLine<'a> {
    joiner: &'static str,
    name: &'a str,
    color: Color32,
    nfd: &'a str,
    origin: Cow<'a, str>,
    lit: Option<(u64, bool)>,
    removable: bool,
}

/// A click on a chip: switch a filter node's line colour, or remove the
/// `i`-th query from the box.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ChipClick {
    Lit(u64),
    Remove(usize),
}

/// The picked queries under a box. Folded (the default): one row of name
/// chips, the NFD on hover. Unfolded (▸ / ▾): one line per query with its
/// NFD. A chip with `lit` switches its line colour (filled when on); a
/// removable one shows ✖ while hovered and a click removes it. Returns
/// what was clicked.
fn chips_view(ui: &mut egui::Ui, lines: &[ChipLine<'_>], open: &mut bool) -> Option<ChipClick> {
    const MAX_LINES: usize = 8;
    let mut clicked = None;
    let mut chip = |ui: &mut egui::Ui, i: usize, l: &ChipLine<'_>, folded: bool| {
        let tip = |extra: &str| {
            let nfd = if folded { format!("\n{}", l.nfd) } else { String::new() };
            format!("{}{nfd}{extra}", l.origin)
        };
        match l.lit {
            Some((key, on)) => {
                let what = if on { "\nLines coloured: click to stop" } else { "\nClick to colour its lines" };
                let r = library::chip(ui, l.name, l.color, on).interact(egui::Sense::click());
                if r.on_hover_text(tip(what)).clicked() {
                    clicked = Some(ChipClick::Lit(key));
                }
            }
            None if l.removable => {
                let r = library::removable_chip(ui, l.name, l.color);
                if r.on_hover_text(tip("\nClick to remove it from the search")).clicked() {
                    clicked = Some(ChipClick::Remove(i));
                }
            }
            None => {
                library::chip(ui, l.name, l.color, false).on_hover_text(tip(""));
            }
        }
    };
    let fold = |ui: &mut egui::Ui, open: &mut bool| {
        let (icon, tip) = if *open { ("▾", "Show the names only") } else { ("▸", "Show each query's text") };
        if ui.add(egui::Button::new(RichText::new(icon).small()).frame(false)).on_hover_text(tip).clicked() {
            *open = !*open;
        }
    };
    if !*open {
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            fold(ui, open);
            for (i, l) in lines.iter().enumerate() {
                if !l.joiner.is_empty() {
                    ui.label(RichText::new(l.joiner).small().weak());
                }
                chip(ui, i, l, true);
            }
        });
        return clicked;
    }
    for (i, l) in lines.iter().enumerate().take(MAX_LINES) {
        ui.horizontal(|ui| {
            if i == 0 {
                fold(ui, open);
            }
            let w = if i == 0 { 38.0 } else { 52.0 };
            ui.add_sized([w, 14.0], egui::Label::new(RichText::new(l.joiner).small().weak()));
            chip(ui, i, l, false);
            ui.add(egui::Label::new(RichText::new(l.nfd).monospace().small()).truncate());
        });
    }
    if lines.len() > MAX_LINES {
        ui.weak(format!("… {} more (the full text is in the box above)", lines.len() - MAX_LINES));
    }
    clicked
}

/// The chip lines of a Filter box expansion; `lit` says whether a node's
/// lines are coloured (included queries only).
fn expansion_lines<'a>(a: &'a Applied, lit: &dyn Fn(u64) -> bool) -> Vec<ChipLine<'a>> {
    let mut includes = 0;
    a.exp
        .parts
        .iter()
        .enumerate()
        .map(|(i, p)| {
            let joiner = match p.role {
                Role::Include if includes == 0 => "",
                Role::Include => "OR",
                Role::Exclude => "AND NOT",
                // Not in the box: signals and colours only.
                Role::Signal => "signals:",
            };
            if p.role == Role::Include {
                includes += 1;
            }
            let mut origin: Cow<'_, str> = if p.query.starts_with("_sel.") {
                "local query (not in a pack)".into()
            } else {
                p.query.as_str().into()
            };
            // Issue #2: picked with an ancestor, it adds nothing to the box.
            if let Some(by) = &p.covered {
                let by = a
                    .exp
                    .parts
                    .iter()
                    .position(|q| q.query == *by && q.role == p.role)
                    .map_or(by.as_str(), |j| &a.names[j]);
                origin = format!(
                    "{origin}
In '{by}', which is picked too: not repeated in the box"
                )
                .into();
            }
            ChipLine {
                joiner,
                name: &a.names[i],
                color: a.colors[i],
                nfd: a.exp.part_nfd(p),
                origin,
                lit: (p.role != Role::Exclude).then(|| (a.keys[i], lit(a.keys[i]))),
                removable: false,
            }
        })
        .collect()
}

/// The chip lines of the Search box's queries (OR'ed).
fn term_lines(terms: &[Term]) -> Vec<ChipLine<'_>> {
    terms
        .iter()
        .enumerate()
        .map(|(i, t)| ChipLine {
            joiner: if i == 0 { "" } else { "OR" },
            name: &t.name,
            color: t.color,
            nfd: &t.nfd,
            origin: "search query".into(),
            lit: None,
            removable: true,
        })
        .collect()
}

#[derive(Default)]
struct App {
    tabs: Vec<Tab>,
    active: usize,
    show_bookmarks: bool,
    show_signals: bool,
    /// Text of the Ctrl+G dialog while it is open.
    goto: Option<String>,
    /// The About dialog is open.
    about: bool,
    /// The app icon as a texture for the About dialog, made on first use.
    about_icon: Option<egui::TextureHandle>,
    history: SearchHistory,
    /// Recent searches (the Search box), kept apart from the filters.
    search_history: SearchHistory,
    /// Search mode and options for new tabs: the last ones the user picked.
    prefs: SearchPrefs,
    /// Shown when no tab is open (e.g. why the last open failed).
    status: String,
    focus_search: bool,
    window_title: String,
    /// Query packs (search manager) and where they come from besides the
    /// per-user folder.
    library: Library,
    library_errors: Vec<String>,
    pack_paths: Vec<PathBuf>,
    show_library: bool,
    library_view: LibraryView,
    /// The ⚙ menu's ports field while it is edited.
    /// The Settings window's UDP port fields as typed: DLT, SOME/IP, syslog.
    ports_text: Option<[String; 3]>,
    /// The unpack cache's limit as typed, while Settings is open.
    limit_text: Option<String>,
    /// This window's name (title, Alt+Tab).
    window_name: Option<String>,
    /// The Settings window is open.
    settings_open: bool,
    /// Listener for files other `nerka-gui` starts hand to this window.
    server: Option<instance::Server>,
    /// Tabs moved to new windows so far (temp file names).
    moves: u32,
    presets: presets::Presets,
    /// Kept plots (every tab can switch to them).
    plots: plots::Plots,
    /// Signals the user defined, for every tab; saved in
    /// `<config>/nerka/signals`. The generation changes with them.
    saved_signals: Signals,
    saved_gen: u64,
    /// FIBEX files for DLT non-verbose messages (this session), read into
    /// one catalog that every DLT tab is opened with.
    fibex: Option<Arc<nerka_core::Fibex>>,
    fibex_paths: Vec<PathBuf>,
    /// The catalog's arguments (for the line window), by catalog.
    fibex_args: Option<(usize, Arc<Vec<nerka_core::Argument>>)>,
    /// The plot is in a window of its own, and what that window shares.
    plot_window: bool,
    plot_win: Arc<std::sync::Mutex<plot::Detached>>,
    /// What the plot's window was last asked to repaint for.
    plot_win_key: Option<plot::WindowKey>,
}

/// What the Settings window changed.
#[derive(Default)]
struct SettingsChange {
    /// The UDP ports of DLT, SOME/IP and syslog, each changed to a valid list.
    ports: [Option<Vec<u16>>; 3],
    /// The unpack cache's limit, changed to a valid number of GiB.
    limit_gib: Option<u64>,
    /// Empty the unpack cache.
    clear: bool,
    /// Local time or UTC, or how MF4 local times are read, changed.
    times: bool,
}

/// What the Settings window shows: this window's name, the UDP ports of
/// each protocol in captures (`ports` as typed: DLT, SOME/IP, syslog), the unpack cache (`limit` as typed, in GiB) and the
/// time settings.
struct SettingsFields<'a> {
    name: &'a mut String,
    ports: &'a mut [String; 3],
    limit: &'a mut String,
    local_time: &'a mut bool,
    mf4_local_time: &'a mut bool,
}

/// The Settings window's fields ([`SettingsFields`]).
fn settings_ui(ui: &mut egui::Ui, f: SettingsFields<'_>) -> SettingsChange {
    let SettingsFields { name, ports, limit, local_time, mf4_local_time } = f;
    let mut save = SettingsChange::default();
    egui::Grid::new("settings-grid").num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
        ui.label(RichText::new("Times").strong());
        ui.end_row();
        ui.label("Show");
        ui.horizontal(|ui| {
            let a = ui.radio_value(local_time, true, "Local time").on_hover_text(
                "This computer's time zone, with the daylight saving time of each date (as DLT Viewer and \
                 Wireshark show times). Times typed in queries without a zone are local too.",
            );
            let b = ui.radio_value(local_time, false, "UTC");
            save.times |= a.changed() || b.changed();
        });
        ui.end_row();
        ui.label("MF4");
        let r = ui.checkbox(mf4_local_time, "Start time marked as local time: read in this zone").on_hover_text(
            "An MF4 file can say its start time is the recorder's local time, without saying the zone. \
             On: read it in this computer's zone. Off: read it as UTC.",
        );
        save.times |= r.changed();
        ui.end_row();
        ui.label(RichText::new("This window").strong());
        ui.end_row();
        ui.label("Name");
        ui.add(egui::TextEdit::singleline(name).hint_text("e.g. OTA crash").desired_width(200.0))
            .on_hover_text("Shown first in the title, so Alt+Tab tells windows apart");
        ui.end_row();
        ui.label(RichText::new("Captures: UDP ports").strong()).on_hover_text(
            "Destination ports whose UDP payloads are read as each protocol (pcap, pcapng, MF4), separated by \
             spaces. A port listed for two protocols goes to the first. Used for captures opened from now on.",
        );
        ui.end_row();
        let rows = [
            ("DLT", "Default: 3490"),
            ("SOME/IP", "Default: 30490 (service discovery); add your services' ports"),
            ("syslog", "Default: 514"),
        ];
        for (k, (name, tip)) in rows.into_iter().enumerate() {
            ui.label(name);
            ui.horizontal(|ui| {
                let r = ui.add(egui::TextEdit::singleline(&mut ports[k]).desired_width(200.0)).on_hover_text(tip);
                match prefs::parse_ports(&ports[k]) {
                    Some(p) => {
                        if r.changed() {
                            save.ports[k] = Some(p);
                        }
                    }
                    None => {
                        ui.colored_label(ui.visuals().error_fg_color, "Port numbers 1–65535, separated by spaces");
                    }
                }
            });
            ui.end_row();
        }
        ui.label(RichText::new("Compressed MF4").strong()).on_hover_text(
            "Compressed data is unpacked once into the unpack cache and read from there, as fast as an \
             uncompressed file. Opening the file again uses it.",
        );
        ui.end_row();
        ui.label("Unpack cache");
        match nerka_core::cache_dir() {
            Some(dir) => {
                let (files, bytes) = nerka_core::input::capture::cache_usage(&dir);
                ui.horizontal(|ui| {
                    ui.label(format!("{files} file{}, {}", if files == 1 { "" } else { "s" }, size_text(bytes)))
                        .on_hover_text(dir.display().to_string());
                    let r = ui.add_enabled(files > 0, egui::Button::new("Clear"));
                    save.clear = r.on_hover_text("Remove the unpacked files (those open in a tab stay)").clicked();
                });
            }
            None => {
                ui.weak("No cache folder: unpacked into memory");
            }
        }
        ui.end_row();
        ui.label("Size limit");
        ui.horizontal(|ui| {
            let r = ui
                .add(egui::TextEdit::singleline(limit).desired_width(48.0))
                .on_hover_text("The least recently used unpacked files are removed when the cache grows past this");
            ui.label("GiB");
            match prefs::parse_gib(limit) {
                Some(n) if r.changed() => save.limit_gib = Some(n),
                Some(_) => {}
                None => {
                    ui.colored_label(ui.visuals().error_fg_color, "1–1024");
                }
            }
        });
        ui.end_row();
    });
    save
}

/// A byte count for people: `512 B`, `3.4 MiB`, `1.2 GiB`.
fn size_text(n: u64) -> String {
    match n {
        n if n >= 1 << 30 => format!("{:.1} GiB", n as f64 / (1u64 << 30) as f64),
        n if n >= 1 << 20 => format!("{:.1} MiB", n as f64 / (1u64 << 20) as f64),
        n if n >= 1 << 10 => format!("{:.1} KiB", n as f64 / 1024.0),
        n => format!("{n} B"),
    }
}

/// A window-style close button: a thin painted "×" with a square hover
/// background. The "✖" glyph isn't in egui's text font and falls back to the
/// heavy emoji/icon font.
fn close_button(ui: &mut egui::Ui) -> egui::Response {
    let side = ui.spacing().interact_size.y;
    let (rect, response) = ui.allocate_exact_size(egui::vec2(side, side), egui::Sense::click());
    if ui.is_rect_visible(rect) {
        let visuals = ui.style().interact(&response);
        if response.hovered() {
            ui.painter().rect_filled(rect, 2.0, visuals.weak_bg_fill);
        }
        let r = egui::Rect::from_center_size(rect.center(), egui::vec2(side * 0.36, side * 0.36));
        let stroke = egui::Stroke::new(1.2, visuals.fg_stroke.color);
        ui.painter().line_segment([r.left_top(), r.right_bottom()], stroke);
        ui.painter().line_segment([r.right_top(), r.left_bottom()], stroke);
    }
    response
}

/// Paths compare equal if they name the same file.
fn same_file(a: &Path, b: &Path) -> bool {
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    }
}

fn history_path() -> Option<PathBuf> {
    nerka_core::config_dir().map(|d| d.join("history"))
}

fn search_history_path() -> Option<PathBuf> {
    nerka_core::config_dir().map(|d| d.join("search-history"))
}

fn signals_path() -> Option<PathBuf> {
    // Tests never touch the user's signals.
    if cfg!(test) {
        return None;
    }
    nerka_core::config_dir().map(|d| d.join("signals"))
}

fn prefs_path() -> Option<PathBuf> {
    // Tests never touch the user's settings.
    if cfg!(test) {
        return None;
    }
    nerka_core::config_dir().map(|d| d.join("prefs"))
}

impl App {
    /// (Re)load the packs. Picks and running filters keep their expanded
    /// text; only the next expansion sees the change.
    fn load_library(&mut self) {
        let mut lib = Library::new();
        let mut errors = Vec::new();
        for p in pack::default_dir().filter(|d| d.is_dir()).iter().chain(&self.pack_paths) {
            errors.extend(lib.load_lenient(p).into_iter().map(|e| e.to_string()));
        }
        for t in &mut self.tabs {
            t.picks.refresh(&lib);
        }
        self.library = lib;
        self.library_errors = errors;
    }

    fn preset_action(&mut self, ctx: &egui::Context, action: presets::Action) {
        use presets::Action as A;
        let Some(tab) = self.tabs.get_mut(self.active) else { return };
        let res = match action {
            A::None => Ok(()),
            A::Use(i) => {
                let p = self.presets.entries[i].preset.clone();
                tab.use_preset(ctx, &self.library, &p);
                Ok(())
            }
            A::New { name, store } => {
                let p = tab.to_preset(&name);
                self.presets.put(p, store).map(|_| tab.preset = Some(name))
            }
            A::Update(i) => {
                let p = tab.to_preset(&self.presets.entries[i].preset.name);
                self.presets.put(p, false).map(|_| ())
            }
            A::Store(i) => {
                let p = self.presets.entries[i].preset.clone();
                self.presets.put(p, true).map(|_| ())
            }
            A::Delete(i) => {
                let name = self.presets.entries[i].preset.name.clone();
                self.presets.delete(i).map(|()| {
                    for t in &mut self.tabs {
                        if t.preset.as_deref() == Some(name.as_str()) {
                            t.preset = None;
                        }
                    }
                })
            }
        };
        match res {
            Ok(()) => self.presets.errors.retain(|e| !e.starts_with("Preset: ")),
            Err(e) => self.presets.errors.push(format!("Preset: {e}")),
        }
    }

    fn library_action(&mut self, ctx: &egui::Context, action: library::Action) {
        match action {
            library::Action::None => {}
            library::Action::Close => self.show_library = false,
            library::Action::Reload => self.load_library(),
            library::Action::Apply => {
                let lib = std::mem::take(&mut self.library);
                let recorded = self.tab().and_then(|t| t.apply_selection(ctx, &lib));
                self.library = lib;
                if let Some(nfd) = recorded {
                    self.record_search("query", &nfd);
                }
            }
            library::Action::Search { target, add } => {
                let lib = std::mem::take(&mut self.library);
                let searched = self.tab().and_then(|t| {
                    t.search_for(ctx, &lib, &target, add);
                    t.search.active.is_some().then(|| (t.search.tag(), t.search.pattern.clone()))
                });
                self.library = lib;
                if let Some((tag, pattern)) = searched {
                    self.record_search_box(tag, &pattern);
                }
            }
            library::Action::Edited { renamed, removed } => {
                for t in &mut self.tabs {
                    if let Some((old, new)) = &renamed {
                        t.picks.renamed(old, new);
                    }
                    t.picks.refresh(&self.library);
                }
                let changed = self
                    .presets
                    .query_changed(renamed.as_ref().map(|(o, n)| (o.as_str(), n.as_str())), removed.as_deref());
                if !changed.is_empty() {
                    self.status = format!("Presets updated: {}", changed.join(", "));
                }
            }
        }
    }
    fn load_saved_signals(&mut self) {
        let Some(path) = signals_path() else { return };
        let Ok(text) = std::fs::read_to_string(&path) else { return };
        let (s, errors) = Signals::parse_text(&text);
        self.saved_signals = s;
        if !errors.is_empty() {
            self.status = format!("{}: {}", path.display(), errors.join("; "));
        }
    }

    fn save_saved_signals(&mut self) {
        self.saved_gen += 1;
        let Some(path) = signals_path() else { return };
        let write = path
            .parent()
            .map_or(Ok(()), std::fs::create_dir_all)
            .and_then(|_| std::fs::write(&path, self.saved_signals.to_text()));
        if let Err(e) = write {
            self.status = format!("Could not save signals to {}: {e}", path.display());
        }
    }

    fn load_history(&mut self) {
        if let Some(path) = history_path() {
            match SearchHistory::load(&path) {
                Ok(h) => self.history = h,
                Err(e) => self.status = format!("Could not read filter history: {e}"),
            }
        }
        if let Some(path) = search_history_path() {
            match SearchHistory::load(&path) {
                Ok(h) => self.search_history = h,
                Err(e) => self.status = format!("Could not read search history: {e}"),
            }
        }
    }

    fn load_prefs(&mut self) {
        if let Some(path) = prefs_path() {
            match SearchPrefs::load(&path) {
                Ok(p) => self.prefs = p,
                Err(e) => self.status = format!("Could not read preferences: {e}"),
            }
        }
        nerka_core::time::set_local(self.prefs.local_time);
    }

    /// Remember the active tab's mode and options if the user changed them.
    /// ⚙: settings that aren't per tab (the DLT ports of captures).
    /// The Settings window (⚙, Ctrl+,): this window's name and the DLT
    /// ports of captures. A window, not a menu: a menu closes on any click,
    /// so its text fields couldn't be typed in.
    fn settings_window(&mut self, ctx: &egui::Context) {
        if !self.settings_open {
            return;
        }
        let mut open = true;
        let mut save = SettingsChange::default();
        egui::Window::new("Settings")
            .id(egui::Id::unique("settings"))
            .open(&mut open)
            .resizable(false)
            .collapsible(false)
            .show(ctx, |ui| {
                let p = &self.prefs;
                let ports = self.ports_text.get_or_insert_with(|| {
                    [&p.dlt_ports, &p.someip_ports, &p.syslog_ports].map(|v| prefs::ports_text(v))
                });
                let limit = self.limit_text.get_or_insert_with(|| self.prefs.unpack_limit_gib.to_string());
                save = settings_ui(
                    ui,
                    SettingsFields {
                        name: self.window_name.get_or_insert_with(String::new),
                        ports,
                        limit,
                        local_time: &mut self.prefs.local_time,
                        mf4_local_time: &mut self.prefs.mf4_local_time,
                    },
                );
            });
        if !open {
            self.settings_open = false;
            self.ports_text = None;
            self.limit_text = None;
        }
        if save.clear
            && let Some(dir) = nerka_core::cache_dir()
        {
            let (removed, in_use) = nerka_core::input::capture::clear_cache(&dir);
            self.status = match in_use {
                0 => format!("Removed {removed} unpacked file{}", if removed == 1 { "" } else { "s" }),
                n => format!("Removed {removed} unpacked files; {n} in use stay"),
            };
        }
        let mut changed = false;
        let p = &mut self.prefs;
        for (new, ports) in save.ports.into_iter().zip([&mut p.dlt_ports, &mut p.someip_ports, &mut p.syslog_ports]) {
            if let Some(new) = new
                && new != *ports
            {
                *ports = new;
                changed = true;
            }
        }
        if let Some(n) = save.limit_gib
            && n != self.prefs.unpack_limit_gib
        {
            self.prefs.unpack_limit_gib = n;
            changed = true;
        }
        if save.times {
            // Rendered lines and collected time values change: every tab
            // reads its files again (filter, search and line stay).
            nerka_core::time::set_local(self.prefs.local_time);
            for t in &mut self.tabs {
                if t.file.is_some() {
                    t.reopen(ctx, &self.prefs, self.fibex.clone());
                }
            }
            changed = true;
        }
        if changed
            && let Some(path) = prefs_path()
            && let Err(e) = self.prefs.save(&path)
        {
            self.status = format!("Could not save preferences: {e}");
        }
    }

    fn save_prefs_from(&mut self, prefs: SearchPrefs) {
        // A tab knows its search settings only; the ports, the cache and
        // the times stay.
        let prefs = SearchPrefs {
            dlt_ports: self.prefs.dlt_ports.clone(),
            someip_ports: self.prefs.someip_ports.clone(),
            syslog_ports: self.prefs.syslog_ports.clone(),
            unpack_limit_gib: self.prefs.unpack_limit_gib,
            local_time: self.prefs.local_time,
            mf4_local_time: self.prefs.mf4_local_time,
            bookmark_notes: self.prefs.bookmark_notes,
            ..prefs
        };
        if prefs == self.prefs {
            return;
        }
        self.prefs = prefs;
        if let Some(path) = prefs_path()
            && let Err(e) = self.prefs.save(&path)
            && let Some(t) = self.tabs.get_mut(self.active)
        {
            t.status = format!("Could not save preferences: {e}");
        }
    }

    /// Record a filter in the filter history.
    fn record_search(&mut self, tag: &str, pattern: &str) {
        if self.history.push(tag, pattern)
            && let Some(path) = history_path()
            && let Err(e) = self.history.save(&path)
            && let Some(t) = self.tabs.get_mut(self.active)
        {
            t.status = format!("Could not save filter history: {e}");
        }
    }

    /// Record a Search box search in the search history.
    fn record_search_box(&mut self, tag: &str, pattern: &str) {
        if self.search_history.push(tag, pattern)
            && let Some(path) = search_history_path()
            && let Err(e) = self.search_history.save(&path)
            && let Some(t) = self.tabs.get_mut(self.active)
        {
            t.status = format!("Could not save search history: {e}");
        }
    }

    fn tab(&mut self) -> Option<&mut Tab> {
        self.tabs.get_mut(self.active)
    }

    /// Open `path` in a new tab, or switch to it if it is already open.
    fn open(&mut self, ctx: &egui::Context, path: PathBuf) {
        self.open_files(ctx, vec![path]);
    }

    /// Files opened together become one view, one after another (owner,
    /// 2026-10-20), if they are of one kind; otherwise separate tabs. A view
    /// already open with the same files is switched to.
    fn open_files(&mut self, ctx: &egui::Context, paths: Vec<PathBuf>) {
        if paths.is_empty() {
            return;
        }
        let same = |t: &Tab| t.paths.len() == paths.len() && t.paths.iter().zip(&paths).all(|(a, b)| same_file(a, b));
        if let Some(i) = self.tabs.iter().position(same) {
            self.active = i;
            return;
        }
        self.tabs.push(Tab::open_many(ctx, paths, &self.prefs, self.fibex.clone()));
        self.active = self.tabs.len() - 1;
    }

    /// Tab `i`'s files as separate tabs (its own files stay in their order).
    fn split_tab(&mut self, ctx: &egui::Context, i: usize) {
        let Some(t) = self.tabs.get(i) else { return };
        let paths = t.paths.clone();
        self.close(i);
        for p in paths {
            self.open(ctx, p);
        }
    }

    fn close(&mut self, i: usize) {
        if i >= self.tabs.len() {
            return;
        }
        self.tabs.remove(i).cancel_jobs();
        if self.active > i || self.active >= self.tabs.len() {
            self.active = self.active.saturating_sub(1);
        }
    }

    fn close_others(&mut self, keep: usize) {
        for (i, mut t) in std::mem::take(&mut self.tabs).into_iter().enumerate() {
            if i == keep {
                self.tabs.push(t);
            } else {
                t.cancel_jobs();
            }
        }
        self.active = 0;
    }

    fn cycle_tab(&mut self, forward: bool) {
        let n = self.tabs.len();
        if n > 1 {
            self.active = if forward { (self.active + 1) % n } else { (self.active + n - 1) % n };
        }
    }

    fn poll_jobs(&mut self, ctx: &egui::Context) {
        let mut i = 0;
        while i < self.tabs.len() {
            if self.tabs[i].poll_jobs(ctx) {
                i += 1;
                continue;
            }
            if let Some(why) = self.tabs[i].mixed.take() {
                let paths = self.tabs[i].paths.clone();
                self.close(i);
                for p in paths {
                    self.open(ctx, p);
                }
                if let Some(t) = self.tab() {
                    t.status = format!("Opened as separate tabs: {why}");
                }
                continue;
            }
            let msg = std::mem::take(&mut self.tabs[i].status);
            self.close(i);
            match self.tab() {
                Some(t) => t.status = msg,
                None => self.status = msg,
            }
        }
    }

    fn handle_input(&mut self, ctx: &egui::Context) {
        let dropped: Vec<PathBuf> = ctx.input(|i| {
            i.raw.dropped_files.iter().map(|f| f.path().to_path_buf()).filter(|p| !p.as_os_str().is_empty()).collect()
        });
        self.open_files(ctx, dropped);
        let shortcut = |m, k| ctx.input_mut(|i| i.consume_shortcut(&KeyboardShortcut::new(m, k)));
        if shortcut(Modifiers::COMMAND, Key::O) {
            self.pick_files(ctx);
        }
        if shortcut(Modifiers::COMMAND, Key::F) {
            self.focus_search = true;
        }
        // Before Ctrl+L: matching ignores an unrequested Shift.
        if shortcut(Modifiers::COMMAND | Modifiers::SHIFT, Key::L) {
            self.show_library = !self.show_library;
        }
        if (shortcut(Modifiers::COMMAND, Key::G) || shortcut(Modifiers::COMMAND, Key::L)) && !self.tabs.is_empty() {
            self.goto = Some(String::new());
        }
        if shortcut(Modifiers::COMMAND, Key::W) {
            self.close(self.active);
        }
        // Before Ctrl+S, if one comes: matching ignores an unrequested Shift.
        if shortcut(Modifiers::COMMAND | Modifiers::SHIFT, Key::S) {
            shot::request(ctx, None);
        }
        if let Some(image) = shot::take(ctx) {
            let [w, h] = image.size;
            ctx.copy_image((*image).clone());
            if let Some(t) = self.tab() {
                t.status = format!("Screenshot copied to the clipboard ({w} × {h})");
            }
        }
        if shortcut(Modifiers::NONE, Key::F1) {
            self.about = true;
        }
        if shortcut(Modifiers::COMMAND, Key::Comma) {
            self.settings_open = !self.settings_open;
        }
        // Shift variants first: matching ignores an unrequested Shift.
        if shortcut(Modifiers::COMMAND | Modifiers::SHIFT, Key::Tab) || shortcut(Modifiers::COMMAND, Key::PageUp) {
            self.cycle_tab(false);
        }
        if shortcut(Modifiers::COMMAND, Key::Tab) || shortcut(Modifiers::COMMAND, Key::PageDown) {
            self.cycle_tab(true);
        }
        // Single-key commands (klogg style) only when not typing in a field.
        if !ctx.text_edit_focused()
            && self.goto.is_none()
            && let Some(tab) = self.tab()
        {
            let (shift, n, m, b) =
                ctx.input(|i| (i.modifiers.shift, i.key_pressed(Key::N), i.key_pressed(Key::M), i.key_pressed(Key::B)));
            if n {
                tab.jump_match(!shift);
            }
            if m {
                tab.toggle_bookmark();
            }
            if b {
                tab.jump_bookmark(!shift);
            }
        }
    }

    /// Use these FIBEX files (none: decode nothing); DLT tabs read their
    /// files again with them.
    /// The FIBEX files' arguments, worked out once per catalog.
    fn fibex_arguments(&mut self) -> Arc<Vec<nerka_core::Argument>> {
        let key = self.fibex.as_ref().map_or(0, |f| Arc::as_ptr(f) as usize);
        if self.fibex_args.as_ref().is_none_or(|(k, _)| *k != key) {
            let args = self.fibex.as_ref().map(|f| f.arguments()).unwrap_or_default();
            self.fibex_args = Some((key, Arc::new(args)));
        }
        self.fibex_args.as_ref().map(|(_, a)| a.clone()).unwrap_or_default()
    }

    fn set_fibex(&mut self, ctx: &egui::Context, paths: Vec<PathBuf>) {
        if paths.is_empty() {
            self.fibex = None;
        } else {
            match nerka_core::Fibex::load(&paths) {
                Ok(f) => {
                    self.status = format!("FIBEX: {} messages from {} files", f.len(), f.files.len());
                    self.fibex = Some(Arc::new(f));
                }
                Err(e) => {
                    self.status = format!("FIBEX not loaded: {e}");
                    if let Some(t) = self.tabs.get_mut(self.active) {
                        t.status = self.status.clone();
                    }
                    return;
                }
            }
        }
        self.fibex_paths = paths;
        for t in &mut self.tabs {
            if t.file.as_ref().is_some_and(|f| !f.columns().is_empty()) {
                t.reopen(ctx, &self.prefs, self.fibex.clone());
            }
        }
    }

    fn pick_files(&mut self, ctx: &egui::Context) {
        if let Some(paths) = rfd::FileDialog::new()
            .add_filter(
                "Logs and traces",
                &["log", "txt", "out", "json", "jsonl", "ndjson", "dlt", "pcap", "pcapng", "cap", "mf4"],
            )
            .add_filter("JSON logs", &["json", "jsonl", "ndjson"])
            .add_filter("DLT traces", &["dlt"])
            .add_filter("Network captures (DLT, SOME/IP, syslog)", &["pcap", "pcapng", "cap"])
            .add_filter("MF4 bus logs (DLT)", &["mf4"])
            .add_filter("All files", &["*"])
            .pick_files()
        {
            self.open_files(ctx, paths);
        }
    }

    /// Files another `nerka-gui` handed to this window; whether it has focus.
    fn poll_instance(&mut self, ctx: &egui::Context) {
        let Some(server) = &mut self.server else { return };
        server.set_focused(ctx.input(|i| i.viewport().focused).unwrap_or(false));
        let requests = server.poll();
        for r in requests {
            let instance::Request::Open(files) = r;
            self.open_files(ctx, files);
            ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
            ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
        }
    }

    /// Open tab `i`'s file in a new window, with its filter, search and
    /// selection (as a preset file), and close it here.
    fn move_tab(&mut self, i: usize) {
        let Some(t) = self.tabs.get(i) else { return };
        let paths = t.paths.clone();
        let file = std::env::temp_dir().join(format!("nerka-move-{}-{}.nerka-preset", std::process::id(), self.moves));
        self.moves += 1;
        let res = std::env::current_exe().and_then(|exe| {
            std::fs::write(&file, t.to_preset("moved tab").serialize())?;
            let mut cmd = std::process::Command::new(exe);
            cmd.arg("--new-window").arg("--preset-file").arg(&file);
            for p in &self.pack_paths {
                cmd.arg("--pack").arg(p);
            }
            cmd.args(&paths).spawn().map(|_| ())
        });
        match res {
            Ok(()) => self.close(i),
            Err(e) => {
                let _ = std::fs::remove_file(&file);
                if let Some(t) = self.tabs.get_mut(i) {
                    t.status = format!("Could not open a new window: {e}");
                }
            }
        }
    }

    fn tab_bar(&mut self, ui: &mut egui::Ui) {
        let (mut select, mut close, mut close_others, mut moved, mut split) = (None, None, None, None, None);
        let (mut files, mut records_edit, mut unfold) = (None, None, None);
        egui::ScrollArea::horizontal().show(ui, |ui| {
            ui.horizontal(|ui| {
                for (i, t) in self.tabs.iter().enumerate() {
                    let label = if t.is_busy() { format!("⏳ {}", t.title()) } else { t.title() };
                    let r = ui.selectable_label(i == self.active, label).on_hover_text(t.files_text());
                    if r.clicked() {
                        select = Some(i);
                    }
                    if r.middle_clicked() {
                        close = Some(i);
                    }
                    r.context_menu(|ui| {
                        if ui.button("Close").clicked() {
                            close = Some(i);
                        }
                        if ui.button("Close other tabs").clicked() {
                            close_others = Some(i);
                        }
                        if ui
                            .button("Move to new window")
                            .on_hover_text("With its filter, search and selection")
                            .clicked()
                        {
                            moved = Some(i);
                        }
                        if ui.button(if t.paths.len() > 1 { "Copy full paths" } else { "Copy full path" }).clicked() {
                            ui.ctx().copy_text(t.files_text());
                        }
                        if t.paths.len() > 1 && ui.button("Open the files as separate tabs").clicked() {
                            split = Some(i);
                        }
                        if ui.button("Files…").on_hover_text("Reorder, add or remove the files of this tab").clicked()
                        {
                            files = Some(i);
                        }
                        let text = t.file.as_ref().is_some_and(|f| f.columns().is_empty());
                        if ui
                            .add_enabled(text, egui::Button::new("Records…"))
                            .on_hover_text("One row per line, or multi-line records (text files)")
                            .clicked()
                        {
                            records_edit = Some(i);
                        }
                        let (label, tip) = if t.main_view.unfold_all {
                            ("Fold all records", "Each record of several lines on one row again")
                        } else {
                            ("Unfold all records", "Every record of several lines shown whole (→ / ← one record)")
                        };
                        if ui.add_enabled(text, egui::Button::new(label)).on_hover_text(tip).clicked() {
                            unfold = Some(i);
                        }
                    });
                    if close_button(ui).on_hover_text("Close (Ctrl+W)").clicked() {
                        close = Some(i);
                    }
                    ui.separator();
                }
            });
        });
        if let Some(i) = select {
            self.active = i;
        }
        if let Some(i) = files {
            self.active = i;
            let t = &mut self.tabs[i];
            t.files_edit = Some(t.paths.clone());
        }
        if let Some(i) = unfold {
            let t = &mut self.tabs[i];
            let all = !t.main_view.unfold_all;
            for v in [&mut t.main_view, &mut t.hits_view] {
                v.unfold_all = all;
                v.unfolded.clear();
            }
        }
        if let Some(i) = records_edit {
            self.active = i;
            let t = &mut self.tabs[i];
            t.records_edit = Some(records::Edit::of(t.records.as_ref()));
        }
        if let Some(i) = close {
            self.close(i);
        } else if let Some(i) = close_others {
            self.close_others(i);
        } else if let Some(i) = moved {
            self.move_tab(i);
        } else if let Some(i) = split {
            let ctx = ui.ctx().clone();
            self.split_tab(&ctx, i);
        }
    }

    /// The active tab's Files window (its menu → Files…): the files of the
    /// view in order, to move up and down, remove and add to. Apply reads
    /// them again as one view (`Tab::set_files`). Sorting the files by
    /// their time is not done yet.
    fn files_window(&mut self, ctx: &egui::Context) {
        let Some(t) = self.tabs.get_mut(self.active) else { return };
        if t.files_edit.is_none() {
            return;
        }
        let title = t.title();
        let Some(list) = &mut t.files_edit else { return };
        let (mut open, mut apply, mut add, mut cancel) = (true, false, false, false);
        egui::Window::new(format!("Files of {title}"))
            .id(egui::Id::unique("files-window"))
            .open(&mut open)
            .collapsible(false)
            .default_width(560.0)
            .show(ctx, |ui| {
                ui.weak(
                    "The tab shows these files as one view, one after another in this order. They must be of one \
                     kind: plain text with plain text, DLT with DLT and captures.",
                );
                ui.separator();
                let (n, mut moved, mut removed) = (list.len(), None, None);
                egui::ScrollArea::vertical().max_height(320.0).show(ui, |ui| {
                    egui::Grid::new("files-grid").num_columns(5).striped(true).show(ui, |ui| {
                        for (i, p) in list.iter().enumerate() {
                            ui.weak(format!("{}", i + 1));
                            let name =
                                p.file_name().map_or_else(|| p.display().to_string(), |f| f.to_string_lossy().into());
                            ui.label(name).on_hover_text(p.display().to_string());
                            if ui.add_enabled(i > 0, egui::Button::new("↑").small()).on_hover_text("Move up").clicked()
                            {
                                moved = Some((i, i - 1));
                            }
                            if ui
                                .add_enabled(i + 1 < n, egui::Button::new("↓").small())
                                .on_hover_text("Move down")
                                .clicked()
                            {
                                moved = Some((i, i + 1));
                            }
                            let rm = ui.add_enabled(n > 1, egui::Button::new("−").small());
                            if rm
                                .on_hover_text("Remove from this tab")
                                .on_disabled_hover_text("A tab keeps one file")
                                .clicked()
                            {
                                removed = Some(i);
                            }
                            ui.end_row();
                        }
                    });
                });
                if let Some((a, b)) = moved {
                    list.swap(a, b);
                }
                if let Some(i) = removed {
                    list.remove(i);
                }
                ui.separator();
                ui.horizontal(|ui| {
                    add = ui.button("+ Add files…").clicked();
                    ui.separator();
                    let changed = *list != t.paths;
                    apply = ui
                        .add_enabled(changed, egui::Button::new("Apply"))
                        .on_hover_text("Read the files again in this order; filter, search and selection stay")
                        .clicked();
                    cancel = ui.button("Cancel").clicked();
                });
            });
        if add && let Some(paths) = rfd::FileDialog::new().pick_files() {
            for p in paths {
                if !list.iter().any(|q| same_file(q, &p)) {
                    list.push(p);
                }
            }
        }
        if apply {
            let paths = list.clone();
            t.set_files(ctx, paths, &self.prefs, self.fibex.clone());
        }
        if apply || cancel || !open {
            t.files_edit = None;
        }
    }

    /// The active tab's Records window: Apply reads its files again, split
    /// as chosen, and remembers it for each file.
    fn records_window(&mut self, ctx: &egui::Context) {
        let (prefs, fibex) = (&self.prefs, self.fibex.clone());
        let Some(t) = self.tabs.get_mut(self.active) else { return };
        let title = t.title();
        let Some(edit) = &mut t.records_edit else { return };
        match records::window(ctx, &title, edit) {
            records::Outcome::Open => {}
            records::Outcome::Close => t.records_edit = None,
            records::Outcome::Apply(split) => {
                t.records_edit = None;
                for p in &t.paths {
                    if let Err(e) = records::remember(p, split.as_ref()) {
                        t.status = format!("Could not remember how {} is read: {e}", p.display());
                    }
                }
                t.records = split;
                t.reopen(ctx, prefs, fibex);
            }
        }
    }

    /// Ctrl+G: modal "go to line" box for the active tab.
    fn goto_dialog(&mut self, ctx: &egui::Context) {
        let Some(text) = &mut self.goto else { return };
        let lines = self.tabs.get(self.active).map_or(0, |t| t.total_lines());
        let mut jump = None;
        let modal = egui::Modal::new(egui::Id::unique("goto")).show(ctx, |ui| {
            ui.label(format!("Go to line (1 – {lines}; filtered: the nearest kept line):"));
            ui.add(egui::TextEdit::singleline(text).desired_width(160.0)).request_focus();
            if ui.input(|i| i.key_pressed(Key::Enter)) {
                jump = Some(text.trim().parse::<u64>().ok());
            }
        });
        match jump {
            Some(Some(n)) => {
                if let Some(t) = self.tabs.get_mut(self.active) {
                    t.goto_line(n.saturating_sub(1));
                }
                self.goto = None;
            }
            Some(None) => self.goto = None,
            None if modal.should_close() => self.goto = None,
            None => {}
        }
    }

    /// F1 / the About button: name, version, licence and where to find it.
    fn about_dialog(&mut self, ctx: &egui::Context) {
        if !self.about {
            return;
        }
        let icon = self
            .about_icon
            .get_or_insert_with(|| {
                let i = icon::icon();
                let image = egui::ColorImage::from_rgba_unmultiplied([i.width as usize, i.height as usize], &i.rgba);
                ctx.load_texture("about-icon", image, egui::TextureOptions::LINEAR)
            })
            .clone();
        let mut close = false;
        let modal = egui::Modal::new(egui::Id::unique("about")).show(ctx, |ui| {
            ui.set_width(340.0);
            ui.horizontal(|ui| {
                ui.add(egui::Image::new(&icon).fit_to_exact_size(egui::vec2(72.0, 72.0)));
                ui.vertical(|ui| {
                    ui.heading("nerka");
                    ui.label(format!("Version {}", env!("CARGO_PKG_VERSION")));
                    ui.weak("Fast log viewer for plain text and DLT");
                });
            });
            ui.add_space(8.0);
            // `authors` in Cargo.toml: "Name <email>".
            let (name, email) = env!("CARGO_PKG_AUTHORS")
                .split(':')
                .next()
                .and_then(|a| a.split_once(" <"))
                .map_or((env!("CARGO_PKG_AUTHORS"), ""), |(n, e)| (n, e.trim_end_matches('>')));
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                ui.label(format!("© 2026 {name}"));
                if !email.is_empty() {
                    ui.hyperlink_to(email, format!("mailto:{email}"));
                }
            });
            ui.label(format!("Licensed under {}.", env!("CARGO_PKG_LICENSE").replace(" OR ", " or ")));
            ui.hyperlink(env!("CARGO_PKG_REPOSITORY"));
            ui.add_space(4.0);
            ui.weak("“Nerka” is Polish for kidney: it filters what flows through it.");
            ui.add_space(8.0);
            ui.vertical_centered(|ui| close = ui.button("Close").clicked());
        });
        if close || modal.should_close() {
            self.about = false;
        }
    }

    fn update_title(&mut self, ctx: &egui::Context) {
        // The window's name first, so Alt+Tab shows it.
        let file = self.tabs.get(self.active).map(|t| match t.paths.len() {
            1 => t.path.display().to_string(),
            _ => t.title(),
        });
        let title = match (self.window_name.as_deref().filter(|n| !n.trim().is_empty()), file) {
            (Some(n), Some(f)) => format!("{} · {f} — nerka", n.trim()),
            (Some(n), None) => format!("{} — nerka", n.trim()),
            (None, Some(f)) => format!("nerka — {f}"),
            (None, None) => "nerka".to_owned(),
        };
        if title != self.window_title {
            ctx.send_viewport_cmd(egui::ViewportCommand::Title(title.clone()));
            self.window_title = title;
        }
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.frame(ui);
    }
}

impl App {
    /// One frame of the whole window (also driven by tests).
    fn frame(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        self.poll_instance(&ctx);
        self.poll_jobs(&ctx);
        for t in &mut self.tabs {
            t.notes = self.prefs.bookmark_notes;
            t.columns.note_column(t.notes == NoteMode::Column);
        }
        self.handle_input(&ctx);
        self.update_title(&ctx);
        self.goto_dialog(&ctx);
        self.files_window(&ctx);
        self.records_window(&ctx);
        self.about_dialog(&ctx);
        self.settings_window(&ctx);
        if let Some(t) = self.tabs.get_mut(self.active)
            && let Some(d) = &mut t.detail
        {
            let line = d.line;
            let args = self.fibex_arguments();
            let t = self.tabs.get_mut(self.active).expect("checked");
            let signals = t.line_signals(line, &args);
            let d = t.detail.as_mut().expect("still there");
            d.signals = signals;
            let (open, copied) = d.show(&ctx);
            if let Some(text) = copied {
                ctx.copy_text(text);
                t.status = "Copied".into();
            }
            if let Some(l) = t.detail.as_mut().and_then(|d| d.goto.take()) {
                t.goto_line(l);
                // The window goes along to that line.
                if let Some(file) = &t.file {
                    t.detail = t.detail.as_ref().and_then(|d| d.moved(&**file, l));
                }
            }
            if let Some(down) = t.detail.as_mut().and_then(|d| d.step.take()) {
                t.step_detail(down);
            }
            if !open {
                t.detail = None;
            }
        }

        egui::Panel::top("toolbar").show(ui, |ui| {
            ui.horizontal(|ui| {
                if ui.button("📂 Open…").on_hover_text("Ctrl+O").clicked() {
                    self.pick_files(&ctx);
                }
                ui.separator();
                let marks = self.tabs.get(self.active).map_or(0, |t| t.bookmarks.len());
                ui.toggle_value(&mut self.show_bookmarks, format!("🔖 Bookmarks ({marks})")).on_hover_text(
                    "Mark lines to come back to: m (or right-click) toggles a bookmark on the current line, \
                     b / Shift+B jump to the next / previous one. Saved next to the log as <file>.nerka-marks.",
                );
                let queries: usize = self.library.packs().iter().map(|p| p.queries.len()).sum();
                let signals = self.tabs.get(self.active).map_or(0, |t| t.signals.len());
                ui.toggle_value(&mut self.show_signals, format!("📈 Signals ({signals})")).on_hover_text(
                    "Values the lines of this tab carry, captured by a regex group: filter with @name \
                     (@speed > 100, @gear == D)",
                );
                ui.toggle_value(&mut self.show_library, format!("📚 Library ({queries})")).on_hover_text(
                    "Saved queries from packs (*.nerka-pack): pick them for the filter or the search \
                     (Ctrl+Shift+L)",
                );
                if let Some(f) = self.tabs.get(self.active).and_then(|t| t.file.as_ref()) {
                    ui.separator();
                    let indexes: usize =
                        self.tabs.iter().filter_map(|t| t.file.as_ref()).map(|f| f.index_memory_bytes()).sum();
                    let text = format!("{} lines · {} · {}", f.line_count(), size_text(f.size_bytes()), f.format());
                    let tip = format!(
                        "Read as: {}\nIndex of this tab: {}; of all tabs: {}",
                        f.format(),
                        size_text(f.index_memory_bytes() as u64),
                        size_text(indexes as u64)
                    );
                    ui.label(text).on_hover_text(tip);
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("ℹ About").on_hover_text("F1").clicked() {
                        self.about = true;
                    }
                    if ui.selectable_label(self.settings_open, "⚙").on_hover_text("Settings (Ctrl+,)").clicked() {
                        self.settings_open = !self.settings_open;
                    }
                    let r = ui.button("📷").on_hover_text("Screenshot of the window to the clipboard (Ctrl+Shift+S)");
                    if r.clicked() {
                        shot::request(ui.ctx(), None);
                    }
                    // The plot at hand, not only in the Signals panel.
                    if let Some(tab) = self.tabs.get(self.active) {
                        let mut plot = plot::lock(&tab.plot);
                        let tip = if plot.shown { "Close the plot (its lanes are kept)" } else { "Show the plot" };
                        if ui.selectable_label(plot.shown, "📈 Plot").on_hover_text(tip).clicked() {
                            plot.shown = !plot.shown;
                        }
                    }
                });
            });
        });

        if !self.tabs.is_empty() {
            egui::Panel::top("tabs").show(ui, |ui| self.tab_bar(ui));
        }

        egui::Panel::bottom("status").show(ui, |ui| {
            ui.horizontal(|ui| match self.tabs.get(self.active) {
                Some(t) => {
                    if let Some(j) = &t.loading {
                        ui.add(egui::ProgressBar::new(j.progress.fraction()).desired_width(200.0).show_percentage());
                        ui.label(format!("Indexing {}", t.path.display()));
                    } else if let Some((_, j)) = &t.searching {
                        ui.add(egui::ProgressBar::new(j.progress.fraction()).desired_width(200.0).show_percentage());
                        if ui.button("Cancel").clicked() {
                            j.cancel();
                        }
                    } else if let Some((_, j)) = &t.search.job {
                        ui.add(egui::ProgressBar::new(j.progress.fraction()).desired_width(200.0).show_percentage());
                        ui.label("Searching");
                        if ui.button("Cancel").clicked() {
                            j.cancel();
                        }
                    } else {
                        ui.label(&t.status);
                    }
                }
                None => {
                    ui.label(&self.status);
                }
            });
        });

        if !self.tabs.is_empty() {
            // A kept plot's lanes that the loaded FIBEX files describe.
            let args = if self.fibex.is_some() { self.fibex_arguments() } else { Default::default() };
            let tab = &mut self.tabs[self.active];
            tab.pick_plotted_arguments(&args);
            tab.refresh_signals(&ctx, &self.library, &self.saved_signals, self.saved_gen);
        }
        // Saved after the tab is done with (the panel changed them), and
        // FIBEX files chosen there (every DLT tab reads its file again).
        let mut save_signals = false;
        let mut fibex_action = None;
        let show_bookmarks = self.show_bookmarks;
        let mut focus_search = std::mem::take(&mut self.focus_search);
        let Some(tab) = self.tabs.get_mut(self.active) else {
            egui::CentralPanel::default().show(ui, |ui| {
                ui.centered_and_justified(|ui| ui.label("Open log files (Ctrl+O) or drop them here"));
            });
            return;
        };
        let Some(file) = tab.file.clone() else {
            egui::CentralPanel::default().show(ui, |ui| {
                ui.centered_and_justified(|ui| ui.label("Indexing…"));
            });
            // Keep a pending Ctrl+F for when the file is ready.
            self.focus_search = focus_search;
            return;
        };

        // Both boxes' settings, compared after the frame (the Search box is
        // drawn last) to remember changes.
        let before = tab.search_prefs();
        let searched =
            egui::Panel::top("filter").show(ui, |ui| tab.filter_bar(ui, &mut focus_search, &self.history)).inner;
        if self.show_library {
            let lit = |t: &Tab| t.picks.nodes.iter().map(|n| (n.key, n.lit)).collect::<Vec<_>>();
            let lit_before = lit(tab);
            // The library on the far right, the selection next to it.
            let a1 = egui::Panel::right("library")
                .resizable(true)
                .default_size(260.0)
                .show(ui, |ui| {
                    library::library_panel(
                        ui,
                        &mut self.library,
                        &self.library_errors,
                        &mut tab.picks,
                        &mut self.library_view,
                        Some(file.schema()),
                    )
                })
                .inner;
            let current = tab.preset.clone();
            let (a2, pa) = egui::Panel::right("selection")
                .resizable(true)
                .default_size(320.0)
                .show(ui, |ui| {
                    let pa = presets::presets_bar(ui, &mut self.presets, current.as_deref());
                    ui.separator();
                    (library::selection_panel(ui, &mut self.library, &mut tab.picks), pa)
                })
                .inner;
            if pa != presets::Action::None {
                self.preset_action(&ctx, pa);
            }
            let Some(tab) = self.tabs.get_mut(self.active) else { return };
            if lit(tab) != lit_before {
                tab.recolor_from(&ctx, &self.library);
            }
            for a in [a1, a2] {
                if a != library::Action::None {
                    self.library_action(&ctx, a);
                }
            }
        }
        let Some(tab) = self.tabs.get_mut(self.active) else { return };
        if show_bookmarks {
            let mut notes = self.prefs.bookmark_notes;
            let close = egui::Panel::right("bookmarks")
                .resizable(true)
                .default_size(300.0)
                .show(ui, |ui| tab.bookmarks_panel(ui, &*file, &mut notes))
                .inner;
            if close {
                self.show_bookmarks = false;
            }
            if notes != self.prefs.bookmark_notes {
                self.prefs.bookmark_notes = notes;
                if let Some(path) = prefs_path()
                    && let Err(e) = self.prefs.save(&path)
                {
                    tab.status = format!("Could not save preferences: {e}");
                }
            }
        }
        if self.show_signals {
            let line = tab.current_line();
            let saved = &mut self.saved_signals;
            let fibex = self.fibex.clone();
            let out = egui::Panel::right("signals")
                .resizable(true)
                // Wide enough for a row's kind, scope, value and source.
                .default_size(500.0)
                .show(ui, |ui| {
                    let input = signals::Input {
                        signals: &tab.signals,
                        generation: tab.signals_gen,
                        saved,
                        file: &*file,
                        line,
                        plot: &mut plot::lock(&tab.plot),
                        views: &mut tab.views,
                        kept: tab.result.as_ref(),
                        fibex: fibex.as_deref(),
                        arg_picks: &mut tab.arg_picks,
                    };
                    signals::signals_panel(ui, &mut tab.signals_ui, input)
                })
                .inner;
            if out.close {
                self.show_signals = false;
            }
            if let Some((name, kind)) = out.kind {
                tab.query_kind.insert(name, kind);
                tab.choices_gen += 1;
            }
            if let Some((name, scope)) = out.scope {
                match self.saved_signals.get(&name) {
                    Some(d) => {
                        let d = (**d).clone().with_scope(scope);
                        self.saved_signals.set(d);
                        save_signals = true;
                    }
                    None => {
                        tab.query_scope.insert(name, scope);
                        tab.choices_gen += 1;
                    }
                }
            }
            if out.saved_changed {
                save_signals = true;
            }
            fibex_action = out.fibex;
            if out.args_changed {
                tab.choices_gen += 1;
            }
            if let Some(line) = out.goto {
                tab.goto_line(line);
            }
        }
        let search_history = &self.search_history;
        let found = egui::Panel::bottom("search")
            .resizable(true)
            .default_size(260.0)
            .min_size(120.0)
            .show(ui, |ui| tab.search_panel(ui, &*file, search_history))
            .inner;
        let (plotted, saved_plots) = {
            let p = plot::lock(&tab.plot);
            (p.shown, self.plots.listed(&p))
        };
        let mut plot_out = plot::Output::default();
        if !(plotted && self.plot_window) {
            self.plot_win_key = None;
        }
        if plotted && self.plot_window {
            // The plot's own window: what it draws now, and what it asked.
            let line = tab.current_line();
            let (goto, toggle, closed) = {
                let mut d = self.plot_win.lock().unwrap_or_else(|e| e.into_inner());
                let input = plot::WindowInput {
                    plot: tab.plot.clone(),
                    signals: tab.signals.clone(),
                    generation: tab.signals_gen,
                    file: file.clone(),
                    cursor: line,
                    kept: tab.result.clone(),
                    bookmarks: plot::Bookmark::all(&tab.bookmarks),
                    saved: saved_plots,
                };
                let key = input.key();
                if self.plot_win_key.as_ref() != Some(&key) {
                    self.plot_win_key = Some(key);
                    ctx.request_repaint_of(egui::ViewportId::from_hash_of("nerka-plot"));
                }
                d.input = Some(input);
                let mut out = std::mem::take(&mut d.out);
                if let Some((l, note)) = out.bookmark.take() {
                    tab.set_bookmark(l, note);
                }
                let asked = (out.goto, out.toggle_window, std::mem::take(&mut d.closed));
                plot_out = out;
                asked
            };
            let shared = self.plot_win.clone();
            let builder = egui::ViewportBuilder::default()
                .with_title(format!("Plot — {} — nerka", tab.title()))
                .with_inner_size([1100.0, 460.0]);
            let id = egui::ViewportId::from_hash_of("nerka-plot");
            ctx.show_viewport_deferred(id, builder, move |ui, _| plot::window_ui(ui, &shared));
            if toggle || closed {
                self.plot_window = false;
                self.plot_win_key = None;
            }
            if let Some(line) = goto {
                tab.goto_line(line);
            }
        } else if plotted {
            let line = tab.current_line();
            let in_window = false;
            let kept = tab.result.clone();
            let (views, generation) = (&mut tab.views, tab.signals_gen);
            let mut series_of = |d: &nerka_core::SignalDef| views.series(d, &*file, kept.as_ref(), generation);
            let marks = plot::Bookmark::all(&tab.bookmarks);
            let src = plot::Source {
                signals: &tab.signals,
                generation,
                file: &*file,
                cursor: line,
                kept: kept.as_ref().map_or(0, |r| Arc::as_ptr(r) as usize),
                series_of: &mut series_of,
                in_window,
                bookmarks: &marks,
                saved: &saved_plots,
            };
            let shared = tab.plot.clone();
            let mut out = egui::Panel::bottom("plot")
                .resizable(true)
                .default_size(220.0)
                .min_size(80.0)
                .show(ui, |ui| plot::plot_panel(ui, &mut plot::lock(&shared), src))
                .inner;
            if out.toggle_window {
                self.plot_window = !self.plot_window;
            }
            if let Some((l, note)) = out.bookmark.take() {
                tab.set_bookmark(l, note);
            }
            if let Some(line) = out.goto.take() {
                tab.goto_line(line);
            }
            plot_out = out;
        }
        if plot_out.close {
            plot::lock(&tab.plot).shown = false;
        }
        if let Some(a) = plot_out.plots {
            let res = self.plots.act(&mut plot::lock(&tab.plot), a);
            match res {
                Ok(deleted) => {
                    self.plots.errors.clear();
                    if let Some(name) = deleted {
                        for t in &self.tabs {
                            let mut p = plot::lock(&t.plot);
                            if p.name.as_deref() == Some(name.as_str()) {
                                p.name = None;
                            }
                        }
                    }
                }
                Err(e) => tab.status = format!("Plot: {e}"),
            }
        }
        let tab = &mut self.tabs[self.active];
        for e in self.plots.errors.drain(..) {
            tab.status = format!("Plots: {e}");
        }
        egui::CentralPanel::default().show(ui, |ui| tab.main_panel(ui, &*file));
        let after = tab.search_prefs();
        if after != before {
            self.save_prefs_from(after);
        }
        if let Some((tag, pattern)) = searched {
            self.record_search(tag, &pattern);
        }
        if let Some((tag, pattern)) = found {
            self.record_search_box(tag, &pattern);
        }
        if save_signals {
            self.save_saved_signals();
        }
        match fibex_action {
            Some(signals::FibexAction::Add) => {
                if let Some(paths) = rfd::FileDialog::new()
                    .add_filter("FIBEX", &["xml", "fibex"])
                    .add_filter("All files", &["*"])
                    .pick_files()
                {
                    let mut all = self.fibex_paths.clone();
                    all.extend(paths);
                    self.set_fibex(&ctx, all);
                }
            }
            Some(signals::FibexAction::Forget) => {
                self.set_fibex(&ctx, Vec::new());
                // Without FIBEX the messages have no arguments.
                for t in &mut self.tabs {
                    t.arg_picks.clear();
                    t.choices_gen += 1;
                }
            }
            None => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LOG: &str = "\
[INFO] cam1 frame drop
[ERROR] cam1 frame drop
[ERROR] cam2 frame drop
[WARN] cam1 timeout
[ERROR] disk full
[ERROR] cam1 timeout
";

    const PACK: &str = "\
pack vision
query errors  labels: triage
    level >= ERROR
query cam(cam)  parent: errors  labels: vision
    raw ~ /$cam /
query drops  labels: vision  color: violet
    /frame drop/
query timeouts  labels: vision
    /timeout/
";

    /// Issue #2: a parent and its child picked together. The box holds the
    /// parent once; the child keeps its chip, which says why.
    #[test]
    fn a_child_picked_with_its_parent_keeps_its_chip() {
        let mut lib = Library::new();
        lib.add(pack::parse_pack("vision.nerka-pack", PACK).unwrap()).unwrap();
        let mut p = Picks::default();
        p.add(&lib, "vision.errors", Use::Include, None);
        p.add(&lib, "vision.cam", Use::Include, None);
        p.params.insert("cam".into(), ("cam1".into(), false));
        let a = p.expand(&lib).unwrap();
        assert_eq!(a.exp.nfd, "(level >= ERROR)");
        let lines = expansion_lines(&a, &|_| false);
        let names: Vec<&str> = lines.iter().map(|l| l.name).collect();
        assert_eq!(names, ["errors", "cam"]);
        assert_eq!(lines[1].nfd, "(level >= ERROR) AND (raw ~ /cam1 /)");
        assert!(lines[1].origin.contains("In 'errors', which is picked too"), "{}", lines[1].origin);
        assert!(lines[1].lit.is_some(), "its lines can still be coloured");
    }

    /// Poll the tab's jobs until none is running.
    fn settle(tab: &mut Tab, ctx: &egui::Context) {
        for _ in 0..500 {
            assert!(tab.poll_jobs(ctx), "{}", tab.status);
            if !tab.is_busy() {
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("jobs still running");
    }

    fn hits(tab: &Tab) -> Vec<(LineNo, Vec<&str>)> {
        let (a, h) = tab.search.hits.as_ref().expect("search hits");
        (0..h.len()).map(|i| (h.lines[i], h.matched(i).map(|q| short(&a.names[q])).collect())).collect()
    }

    #[test]
    fn a_dlt_tab_reads_its_file_again_with_fibex() {
        use nerka_core::input::dlt::writer::{Msg, Payload};
        let dir = std::env::temp_dir().join(format!("nerka-gui-fibex-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (dlt, xml) = (dir.join("t.dlt"), dir.join("t.xml"));
        let mut buf = Vec::new();
        for i in 0..5u16 {
            let data = (40 + i).to_le_bytes().to_vec();
            let m = Msg {
                ext: None,
                payload: Payload::NonVerbose { id: 7, data },
                ..Msg::log(nerka_core::Level::Info, "E1", "", "", vec![])
            };
            m.encode(&mut buf);
        }
        std::fs::write(&dlt, &buf).unwrap();
        std::fs::write(
            &xml,
            r#"<fx:FIBEX xmlns:fx="f" xmlns:ho="h"><fx:ELEMENTS><fx:PDUS>
              <fx:PDU ID="p"><ho:DESC>speed</ho:DESC><fx:SIGNAL-INSTANCES><fx:SIGNAL-INSTANCE ID="s">
                <fx:SEQUENCE-NUMBER>0</fx:SEQUENCE-NUMBER><fx:SIGNAL-REF ID-REF="S_UINT16"/></fx:SIGNAL-INSTANCE></fx:SIGNAL-INSTANCES></fx:PDU>
            </fx:PDUS><fx:FRAMES><fx:FRAME ID="ID_7"><fx:PDU-INSTANCES><fx:PDU-INSTANCE ID="i"><fx:PDU-REF ID-REF="p"/>
              <fx:SEQUENCE-NUMBER>0</fx:SEQUENCE-NUMBER></fx:PDU-INSTANCE></fx:PDU-INSTANCES>
              <fx:MANUFACTURER-EXTENSION><APPLICATION_ID>NAV</APPLICATION_ID></fx:MANUFACTURER-EXTENSION></fx:FRAME></fx:FRAMES>
            </fx:ELEMENTS></fx:FIBEX>"#,
        )
        .unwrap();
        let ctx = egui::Context::default();
        let mut tab = Tab::open(&ctx, dlt, &SearchPrefs::default());
        settle(&mut tab, &ctx);
        let text = |tab: &Tab, l| tab.file.as_ref().unwrap().line_text(l).unwrap().into_owned();
        assert!(text(&tab, 2).ends_with("[7] 2a 00"), "{}", text(&tab, 2));
        tab.goto_line(3);
        // A filter in the box (not run yet) runs when the file is read again.
        tab.mode = Mode::Query;
        tab.pattern = "payload ~ /speed 4[2-4]/".into();

        let fibex = Arc::new(nerka_core::Fibex::load(&[&xml]).unwrap());
        tab.reopen(&ctx, &SearchPrefs::default(), Some(fibex));
        settle(&mut tab, &ctx);
        assert!(text(&tab, 2).ends_with("	E1	NAV			speed 42"), "{}", text(&tab, 2));
        // The filter ran again on the decoded text; the current line stayed.
        assert_eq!(tab.result.as_ref().unwrap().lines, [2, 3, 4]);
        assert_eq!(tab.current_line(), 3);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A FIBEX describing non-verbose message 11 of ECU1: "Queue almost
    /// full. Free slots:" and a u16 (the argument `free_slots`).
    pub(crate) const QUEUE_FIBEX: &str = r#"<fx:FIBEX xmlns:fx="f" xmlns:ho="h"><fx:ELEMENTS><fx:ECUS><fx:ECU ID="ECU1"/></fx:ECUS><fx:PDUS>
              <fx:PDU ID="t"><ho:DESC>Queue almost full. Free slots:</ho:DESC></fx:PDU>
              <fx:PDU ID="v"><fx:SIGNAL-INSTANCES><fx:SIGNAL-INSTANCE ID="s"><fx:SEQUENCE-NUMBER>0</fx:SEQUENCE-NUMBER>
                <fx:SIGNAL-REF ID-REF="S_UINT16"/></fx:SIGNAL-INSTANCE></fx:SIGNAL-INSTANCES></fx:PDU>
            </fx:PDUS><fx:FRAMES><fx:FRAME ID="ID_11"><fx:PDU-INSTANCES>
              <fx:PDU-INSTANCE ID="1"><fx:PDU-REF ID-REF="t"/><fx:SEQUENCE-NUMBER>0</fx:SEQUENCE-NUMBER></fx:PDU-INSTANCE>
              <fx:PDU-INSTANCE ID="2"><fx:PDU-REF ID-REF="v"/><fx:SEQUENCE-NUMBER>1</fx:SEQUENCE-NUMBER></fx:PDU-INSTANCE>
            </fx:PDU-INSTANCES><fx:MANUFACTURER-EXTENSION><APPLICATION_ID>QUE</APPLICATION_ID></fx:MANUFACTURER-EXTENSION>
            </fx:FRAME></fx:FRAMES></fx:ELEMENTS></fx:FIBEX>"#;

    /// A DLT of message 11 (see [`QUEUE_FIBEX`]), one per free-slots value.
    fn queue_dlt(sizes: &[u16]) -> Vec<u8> {
        use nerka_core::input::dlt::writer::{Msg, Payload};
        let mut buf = Vec::new();
        for size in sizes {
            let m = Msg {
                ext: None,
                payload: Payload::NonVerbose { id: 11, data: size.to_le_bytes().to_vec() },
                ..Msg::log(nerka_core::Level::Info, "ECU1", "", "", vec![])
            };
            m.encode(&mut buf);
        }
        buf
    }

    #[test]
    fn fibex_arguments_become_signals_of_a_tab() {
        let dir = std::env::temp_dir().join(format!("nerka-gui-fibex-args-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (dlt, xml) = (dir.join("t.dlt"), dir.join("t.xml"));
        std::fs::write(&dlt, queue_dlt(&[512, 64, 900, 32])).unwrap();
        std::fs::write(&xml, QUEUE_FIBEX).unwrap();
        let fibex = Arc::new(nerka_core::Fibex::load(&[&xml]).unwrap());
        let args = fibex.arguments();
        assert_eq!(args.iter().map(|a| a.name.as_str()).collect::<Vec<_>>(), ["free_slots"]);
        let ctx = egui::Context::default();
        let mut tab = Tab::open_many(&ctx, vec![dlt], &SearchPrefs::default(), Some(fibex.clone()));
        settle(&mut tab, &ctx);
        // Not picked yet: the line window still lists the message's argument.
        let own = tab.line_signals(0, &args);
        let free: Vec<(&str, Option<&str>, &str)> =
            own.iter().map(|s| (s.name.as_str(), s.value.as_deref(), s.origin)).collect();
        assert_eq!(free, [("free_slots", Some("512"), "FIBEX, not used")]);
        tab.arg_picks = args.clone();
        tab.choices_gen += 1;
        tab.refresh_signals(&ctx, &Library::new(), &Signals::default(), 0);
        settle(&mut tab, &ctx);
        let d = tab.signals.get("free_slots").expect("a signal");
        assert_eq!((d.kind(), d.argument_index()), (SignalKind::Num, Some(0)));
        tab.mode = Mode::Query;
        tab.pattern = "@free_slots < 100".into();
        tab.start_search(&ctx);
        settle(&mut tab, &ctx);
        assert_eq!(tab.result.as_ref().unwrap().lines, [1, 3]);
        // Picked, it is listed once, as the tab's signal.
        let own = tab.line_signals(1, &args);
        // (with the file's built-in @time, an event on every DLT line).
        assert_eq!(
            own.iter().map(|s| (s.name.as_str(), s.origin)).collect::<Vec<_>>(),
            [("time", "built in"), ("free_slots", "FIBEX")]
        );

        // Its − in the Signals panel unpicks it (owner, 2026-10-28: it only
        // took it out of the plot), so the tab's signals lose it.
        plot::lock(&tab.plot).toggle("free_slots");
        let file = tab.file.clone().unwrap();
        let mut minus = None;
        for t in 0..3 {
            let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(700.0, 900.0));
            let events = match (t, minus) {
                (2, Some(p)) => vec![
                    egui::Event::PointerMoved(p),
                    egui::Event::PointerButton {
                        pos: p,
                        button: egui::PointerButton::Primary,
                        pressed: true,
                        modifiers: Default::default(),
                    },
                    egui::Event::PointerButton {
                        pos: p,
                        button: egui::PointerButton::Primary,
                        pressed: false,
                        modifiers: Default::default(),
                    },
                ],
                _ => vec![],
            };
            let input =
                egui::RawInput { screen_rect: Some(screen), time: Some(t as f64), events, ..Default::default() };
            let mut changed = false;
            let mut out = ctx.run_ui(input, |ui| {
                egui::CentralPanel::default().show(ui, |ui| {
                    let input = signals::Input {
                        signals: &tab.signals,
                        generation: tab.signals_gen,
                        saved: &mut Signals::default(),
                        file: &*file,
                        line: 0,
                        plot: &mut plot::lock(&tab.plot),
                        views: &mut tab.views,
                        kept: None,
                        fibex: Some(&*fibex),
                        arg_picks: &mut tab.arg_picks,
                    };
                    changed = signals::signals_panel(ui, &mut tab.signals_ui, input).args_changed;
                });
            });
            out.textures_delta.clear();
            minus = out.shapes.iter().find_map(|s| match &s.shape {
                egui::Shape::Text(t) if t.galley.text() == "−" => Some(t.pos + t.galley.size() / 2.0),
                _ => None,
            });
            if t == 2 {
                assert!(changed, "the panel says the picks changed");
            }
        }
        assert!(tab.arg_picks.is_empty());
        assert!(plot::lock(&tab.plot).plotted.is_empty());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn the_line_window_lists_signals_and_follows_the_selection() {
        let dir = std::env::temp_dir().join(format!("nerka-gui-detail-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let log = dir.join("app.log");
        std::fs::write(
            &log,
            "v=10 gear=P
boot
v=60
gear=D
",
        )
        .unwrap();
        let ctx = egui::Context::default();
        let mut tab = Tab::open(&ctx, log, &SearchPrefs::default());
        settle(&mut tab, &ctx);
        tab.signals.set(nerka_core::SignalDef::parse_spec(r"v=/v=(?<v>\d+)/").unwrap());
        tab.signals.set(nerka_core::SignalDef::parse_spec(r"gear:enum=/gear=(?<gear>\w)/").unwrap());
        tab.signals_changed(&ctx);
        settle(&mut tab, &ctx);
        // Line 3 carries v; gear holds P from line 1.
        let s = tab.line_signals(2, &[]);
        let rows: Vec<(&str, Option<&str>, Option<LineNo>)> =
            s.iter().map(|s| (s.name.as_str(), s.value.as_deref(), s.from)).collect();
        assert_eq!(rows, [("v", Some("60"), Some(2)), ("gear", Some("P"), Some(0))], "events first");
        assert_eq!(tab.line_signals(1, &[])[0].from, Some(0), "a state");
        // The window follows the line selected in a view.
        let file = tab.file.clone().unwrap();
        tab.detail = LineDetail::of(&*file, 0);
        tab.detail.as_mut().unwrap().wrap = false;
        tab.view_output(&view::ViewOutput { moved_to: Some(3), ..Default::default() }, false, |r| r);
        let d = tab.detail.as_ref().unwrap();
        assert_eq!((d.line, d.wrap), (3, false), "keeps Wrap");
        assert_eq!(d.cells.last().map(String::as_str), Some("gear=D"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// ↑ / ↓ over the line window step through the log (the filter result
    /// here), not through the window's buttons.
    #[test]
    fn arrows_in_the_line_window_step_through_the_view() {
        let dir = std::env::temp_dir().join(format!("nerka-gui-detail-keys-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let log = dir.join("app.log");
        std::fs::write(&log, "a ERROR\nb\nc ERROR\nd\ne ERROR\n").unwrap();
        let ctx = egui::Context::default();
        let mut tab = Tab::open(&ctx, log, &SearchPrefs::default());
        settle(&mut tab, &ctx);
        tab.pattern = "ERROR".into();
        tab.start_search(&ctx);
        settle(&mut tab, &ctx);
        assert_eq!(tab.result.as_ref().unwrap().lines, [0, 2, 4]);
        let file = tab.file.clone().unwrap();
        tab.detail = LineDetail::of(&*file, 0);

        // A key pressed with the pointer over the window asks for a step.
        let key = |key| egui::Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        };
        let mut t = 0.0;
        let mut frame = |events: Vec<egui::Event>, d: &mut LineDetail| {
            t += 1.0 / 60.0;
            let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1000.0, 800.0));
            let input = egui::RawInput { screen_rect: Some(screen), time: Some(t), events, ..Default::default() };
            let mut out = ctx.run_ui(input, |ui| {
                d.show(ui.ctx());
            });
            out.textures_delta.clear();
        };
        let d = tab.detail.as_mut().unwrap();
        frame(vec![], d);
        frame(vec![], d);
        let rect = ctx.memory(|m| m.area_rect(egui::Id::unique("line-detail"))).expect("the window");
        frame(vec![egui::Event::PointerMoved(rect.center())], d);
        frame(vec![key(egui::Key::ArrowDown)], d);
        assert_eq!(d.step.take(), Some(true));
        frame(vec![key(egui::Key::ArrowUp)], d);
        assert_eq!(d.step.take(), Some(false));

        // A step goes to the next kept line, and the main view goes along.
        tab.step_detail(true);
        assert_eq!(tab.detail.as_ref().unwrap().line, 2);
        assert_eq!(tab.current_line(), 2);
        tab.step_detail(true);
        tab.step_detail(true);
        assert_eq!(tab.detail.as_ref().unwrap().line, 4, "stops at the last line");
        tab.step_detail(false);
        assert_eq!((tab.detail.as_ref().unwrap().line, tab.current_line()), (2, 2));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn the_line_window_shows_a_dlt_payload_with_its_line_breaks() {
        use nerka_core::input::dlt::writer::{Msg, Value};
        let mut buf = Vec::new();
        let args = vec![Value::Str("first line\n\tsecond line".into()).into()];
        Msg::log(nerka_core::Level::Error, "ECU1", "NAV", "POS", args).encode(&mut buf);
        let file = nerka_core::DltFile::from_bytes("t.dlt", buf);
        let mut d = LineDetail::of(&file, 0).unwrap();
        assert_eq!(d.names.len(), d.cells.len());
        assert_eq!(d.cells.last().unwrap(), "first line\n\tsecond line");

        let ctx = egui::Context::default();
        let mut texts = Vec::new();
        for t in 0..3 {
            let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(900.0, 600.0));
            let input = egui::RawInput { screen_rect: Some(screen), time: Some(t as f64 / 60.0), ..Default::default() };
            let mut out = ctx.run_ui(input, |ui| {
                let (open, copied) = d.show(ui.ctx());
                assert!(open);
                assert_eq!(copied, None);
            });
            texts = out
                .shapes
                .iter()
                .filter_map(|s| match &s.shape {
                    egui::Shape::Text(t) => Some(t.galley.text().to_owned()),
                    _ => None,
                })
                .collect();
            out.textures_delta.clear();
        }
        for want in ["Line 1", "Copy message", "Copy line", "apid", "NAV", "first line\n\tsecond line"] {
            assert!(texts.iter().any(|t| t == want), "{want:?} not in {texts:?}");
        }
    }

    #[test]
    fn chips_fold_into_one_row_and_unfold_into_lines() {
        let lines = [
            ChipLine {
                joiner: "",
                name: "ssl",
                color: Color32::RED,
                nfd: "(/SSL/)",
                origin: "demo.ssl".into(),
                lit: Some((1, true)),
                removable: false,
            },
            ChipLine {
                joiner: "OR",
                name: "disk",
                color: Color32::BLUE,
                nfd: "(/disk/)",
                origin: "demo.disk".into(),
                lit: Some((2, false)),
                removable: false,
            },
            ChipLine {
                joiner: "AND NOT",
                name: "noise",
                color: Color32::GRAY,
                nfd: "(/nav/)",
                origin: "demo.noise".into(),
                lit: None,
                removable: false,
            },
        ];
        let ctx = egui::Context::default();
        let texts = |open: &mut bool| {
            let mut out = Vec::new();
            for t in 0..2 {
                let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(900.0, 300.0));
                let input = egui::RawInput { screen_rect: Some(screen), time: Some(t as f64), ..Default::default() };
                let mut o = ctx.run_ui(input, |ui| {
                    egui::CentralPanel::default().show(ui, |ui| {
                        chips_view(ui, &lines, open);
                    });
                });
                o.textures_delta.clear();
                out = o
                    .shapes
                    .iter()
                    .filter_map(|s| match &s.shape {
                        egui::Shape::Text(t) => Some((t.galley.text().to_owned(), t.pos.y.round())),
                        _ => None,
                    })
                    .collect::<Vec<_>>();
            }
            out
        };
        let folded = texts(&mut false);
        let y = |s: &str, v: &[(String, f32)]| v.iter().find(|(t, _)| t == s).map(|(_, y)| *y);
        assert!(folded.iter().all(|(t, _)| !t.starts_with('(')), "no NFD while folded: {folded:?}");
        assert_eq!(y("ssl", &folded), y("noise", &folded), "one row");
        assert!(y("AND NOT", &folded).is_some());
        let open = texts(&mut true);
        assert!(y("(/disk/)", &open).is_some(), "{open:?}");
        assert!(y("ssl", &open) < y("noise", &open), "one line per query");
    }

    fn p_why(p: &Preview) -> Option<String> {
        match p {
            Preview::AutoText { why, .. } => why.clone(),
            _ => None,
        }
    }

    /// Bug (owner, 2026-11-02): after a click on a search hit, a click on a
    /// line of the main view jumped back to the hit. The hits pane (drawn
    /// first, still focused on the press) moved the main view to its hit
    /// whenever they differed, not only when its selection moved.
    #[test]
    fn a_click_in_the_main_view_after_a_hit_stays() {
        let dir = std::env::temp_dir().join(format!("nerka-gui-hitclick-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let log = dir.join("app.log");
        let text: String =
            (0..300).map(|i| if i % 50 == 0 { format!("needle {i}\n") } else { format!("line {i}\n") }).collect();
        std::fs::write(&log, text).unwrap();
        let ctx = egui::Context::default();
        let mut app = App::default();
        app.open(&ctx, log);
        settle(&mut app.tabs[0], &ctx);
        app.tabs[0].search.pattern = "needle".into();
        app.tabs[0].run_search_box(&ctx);
        settle_app(&mut app, &ctx);
        let mut t = 0.0;
        let mut frame = |app: &mut App, events: Vec<egui::Event>| {
            t += 1.0 / 60.0;
            let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1200.0, 800.0));
            let input = egui::RawInput { screen_rect: Some(screen), time: Some(t), events, ..Default::default() };
            let mut out = ctx.run_ui(input, |ui| app.frame(ui));
            out.textures_delta.clear();
            out.shapes
        };
        let find = |shapes: &[egui::epaint::ClippedShape], label: &str, lowest: bool| {
            let at = shapes.iter().filter_map(|s| match &s.shape {
                egui::Shape::Text(t) if t.galley.text() == label => Some(t.pos + egui::vec2(20.0, 5.0)),
                _ => None,
            });
            if lowest { at.max_by(|a, b| a.y.total_cmp(&b.y)) } else { at.min_by(|a, b| a.y.total_cmp(&b.y)) }
        };
        let button = |p: egui::Pos2, pressed| egui::Event::PointerButton {
            pos: p,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };
        let mut shapes = Vec::new();
        for _ in 0..3 {
            shapes = frame(&mut app, vec![]);
        }
        // A click on the hit `needle 150` (the lowest: the hits pane).
        let hit = find(&shapes, "needle 150", true).expect("the hit");
        frame(&mut app, vec![egui::Event::PointerMoved(hit), button(hit, true)]);
        frame(&mut app, vec![button(hit, false)]);
        for _ in 0..2 {
            shapes = frame(&mut app, vec![]);
        }
        assert_eq!(app.tabs[0].current_line(), 150);
        // Then scroll the main view away and click a line there: press, a
        // frame, release.
        let main = find(&shapes, "line 145", false).expect("line 145 in the main view");
        let wheel = egui::Event::MouseWheel {
            unit: egui::MouseWheelUnit::Line,
            delta: egui::vec2(0.0, -10.0),
            phase: egui::TouchPhase::Move,
            source: egui::MouseWheelSource::Wheel,
            modifiers: egui::Modifiers::NONE,
        };
        frame(&mut app, vec![egui::Event::PointerMoved(main), wheel]);
        for _ in 0..2 {
            shapes = frame(&mut app, vec![]);
        }
        let target = app.tabs[0].main_view.top + 5;
        assert!(target > 160, "scrolled: {target}");
        let other = find(&shapes, &format!("line {target}"), false).expect("a line of the main view, scrolled");
        frame(&mut app, vec![egui::Event::PointerMoved(other), button(other, true)]);
        frame(&mut app, vec![]);
        frame(&mut app, vec![button(other, false)]);
        for _ in 0..3 {
            frame(&mut app, vec![]);
        }
        assert_eq!(app.tabs[0].current_line(), target, "the cursor stays on the line clicked");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Bug: ticking the Search box's Regex / Ignore case wasn't saved, as
    /// the settings were compared before the Search box was drawn.
    #[test]
    fn ticking_the_search_box_checkboxes_is_remembered() {
        let dir = std::env::temp_dir().join(format!("nerka-gui-searchprefs-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let log = dir.join("app.log");
        std::fs::write(&log, LOG).unwrap();
        let ctx = egui::Context::default();
        let mut app = App::default();
        app.open(&ctx, log);
        settle(&mut app.tabs[0], &ctx);
        let mut t = 0.0;
        let mut frame = |app: &mut App, events: Vec<egui::Event>| {
            t += 1.0 / 60.0;
            let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1200.0, 800.0));
            let input = egui::RawInput { screen_rect: Some(screen), time: Some(t), events, ..Default::default() };
            let mut out = ctx.run_ui(input, |ui| app.frame(ui));
            out.textures_delta.clear();
            out.shapes
        };
        // The lowest "Regex" / "Ignore case" labels are the Search box's.
        let lowest = |shapes: &[egui::epaint::ClippedShape], label: &str| {
            shapes
                .iter()
                .filter_map(|s| match &s.shape {
                    egui::Shape::Text(t) if t.galley.text() == label => Some(t.pos + t.galley.size() / 2.0),
                    _ => None,
                })
                .max_by(|a, b| a.y.total_cmp(&b.y))
        };
        let click = |p: egui::Pos2| {
            vec![
                egui::Event::PointerMoved(p),
                egui::Event::PointerButton {
                    pos: p,
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: egui::Modifiers::NONE,
                },
                egui::Event::PointerButton {
                    pos: p,
                    button: egui::PointerButton::Primary,
                    pressed: false,
                    modifiers: egui::Modifiers::NONE,
                },
            ]
        };
        let mut shapes = Vec::new();
        for _ in 0..3 {
            shapes = frame(&mut app, vec![]);
        }
        assert!(!app.prefs.search_regex && !app.prefs.search_ignore_case);
        let regex = lowest(&shapes, "Regex").expect("the Search box's Regex");
        frame(&mut app, click(regex));
        let shapes = frame(&mut app, vec![]);
        assert!(app.tabs[0].search.regex, "the click ticked it");
        assert!(app.prefs.search_regex, "and it is remembered");
        let ci = lowest(&shapes, "Ignore case").expect("the Search box's Ignore case");
        frame(&mut app, click(ci));
        frame(&mut app, vec![]);
        assert!(app.prefs.search_ignore_case);
        // A new tab starts with them.
        let tab = Tab::open(&ctx, dir.join("app.log"), &app.prefs);
        assert!(tab.search.regex && tab.search.ignore_case);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Settle every tab's jobs, as frames would.
    fn settle_app(app: &mut App, ctx: &egui::Context) {
        for _ in 0..500 {
            app.poll_jobs(ctx);
            if app.tabs.iter().all(|t| !t.is_busy()) {
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("jobs still running");
    }

    /// Owner, 2026-10-28: a plot kept in one tab opens in another with all
    /// its lanes. One whose signal that tab lacks stays (it was dropped when
    /// the tab's signals were built again); one the loaded FIBEX files
    /// describe becomes the tab's signal.
    #[test]
    fn a_kept_plot_opens_in_another_tab_with_all_its_lanes() {
        let dir = std::env::temp_dir().join(format!("nerka-gui-tabplots-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let fibex = dir.join("ecu.xml");
        std::fs::write(&fibex, QUEUE_FIBEX).unwrap();
        let dlt = |path: &Path| std::fs::write(path, queue_dlt(&[512, 64])).unwrap();
        let (a, b) = (dir.join("a.dlt"), dir.join("b.dlt"));
        dlt(&a);
        dlt(&b);
        let ctx = egui::Context::default();
        let mut app = App::default();
        app.set_fibex(&ctx, vec![fibex]);
        app.show_signals = true;
        let run = |app: &mut App, n: usize| {
            for _ in 0..n {
                let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1200.0, 800.0));
                let input = egui::RawInput { screen_rect: Some(screen), ..Default::default() };
                let mut out = ctx.run_ui(input, |ui| app.frame(ui));
                out.textures_delta.clear();
                settle_app(app, &ctx);
            }
        };
        app.open(&ctx, a);
        app.open(&ctx, b);
        settle_app(&mut app, &ctx);
        app.active = 0;
        run(&mut app, 2);
        let arg = app.fibex_arguments()[0].clone();
        app.tabs[0].arg_picks.push(arg.clone());
        app.tabs[0].choices_gen += 1;
        {
            let mut p = plot::lock(&app.tabs[0].plot);
            p.toggle(&arg.name);
            p.toggle("w"); // as if from tab A's own selection
        }
        run(&mut app, 2);
        let keep = plot::PlotAction::Keep { name: "drive".into(), store: false };
        app.plots.act(&mut plot::lock(&app.tabs[0].plot), keep).unwrap();

        app.active = 1;
        run(&mut app, 2);
        assert!(app.tabs[1].arg_picks.is_empty());
        app.plots.act(&mut plot::lock(&app.tabs[1].plot), plot::PlotAction::Use("drive".into())).unwrap();
        run(&mut app, 3);
        assert_eq!(app.tabs[1].arg_picks.iter().map(|a| a.name.as_str()).collect::<Vec<_>>(), [arg.name.as_str()]);
        assert!(app.tabs[1].signals.get(&arg.name).is_some(), "the FIBEX argument is the tab's signal");
        // The tab's signals are built again: the lanes stay.
        app.tabs[1].choices_gen += 1;
        run(&mut app, 3);
        let p = plot::lock(&app.tabs[1].plot);
        assert_eq!(p.plotted, [arg.name.as_str(), "w"]);
        assert_eq!(p.name.as_deref(), Some("drive"));
        drop(p);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn files_opened_together_are_one_view_with_bookmarks_per_file() {
        let dir = std::env::temp_dir().join(format!("nerka-gui-concat-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (a, b) = (dir.join("a.log"), dir.join("b.log"));
        std::fs::write(
            &a,
            "[INFO] a one
[ERROR] a two
",
        )
        .unwrap();
        std::fs::write(
            &b,
            "[ERROR] b one
[INFO] b two
[ERROR] b three
",
        )
        .unwrap();
        let ctx = egui::Context::default();
        let mut app = App::default();
        app.open_files(&ctx, vec![a.clone(), b.clone()]);
        settle_app(&mut app, &ctx);
        assert_eq!(app.tabs.len(), 1);
        let t = &mut app.tabs[0];
        assert_eq!(t.title(), "a.log + 1");
        assert_eq!(t.part_files, [(0, 2), (2, 3)]);
        assert_eq!(t.file.as_ref().unwrap().line_count(), 5);
        // Filter by level and by file across both.
        t.mode = Mode::Query;
        t.pattern = "level >= ERROR AND NOT file == a.log".into();
        t.start_search(&ctx);
        settle(t, &ctx);
        assert_eq!(t.result.as_ref().unwrap().lines, [2, 4]);
        // A bookmark in b goes to b's own sidecar, as b numbers its lines.
        t.toggle_bookmark_at(3);
        let sidecar = std::fs::read_to_string(Bookmarks::sidecar_path(&b)).unwrap();
        assert!(sidecar.starts_with("2\t\tB1\t@"), "with its anchor: {sidecar:?}");
        assert!(!Bookmarks::sidecar_path(&a).exists());
        // Opening the same files again switches to the view.
        app.open_files(&ctx, vec![a.clone(), b.clone()]);
        assert_eq!(app.tabs.len(), 1);
        // Split into tabs: one per file, b's bookmark with it.
        app.split_tab(&ctx, 0);
        settle_app(&mut app, &ctx);
        assert_eq!(app.tabs.iter().map(|t| t.title()).collect::<Vec<_>>(), ["a.log", "b.log"]);
        assert!(app.tabs[1].bookmarks.contains(1));
        std::fs::remove_dir_all(&dir).ok();
    }

    /// The Files window: the tab's files reordered, one removed and added
    /// again; the view stays on the same line of the same file. Files of
    /// another kind leave the tab as it was.
    #[test]
    fn a_tabs_files_are_reordered_removed_and_added() {
        let dir = std::env::temp_dir().join(format!("nerka-gui-files-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (a, b, d) = (dir.join("a.log"), dir.join("b.log"), dir.join("d.dlt"));
        std::fs::write(&a, "[INFO] a one\n[ERROR] a two\n").unwrap();
        std::fs::write(&b, "[ERROR] b one\n[INFO] b two\n[ERROR] b three\n").unwrap();
        let mut buf = Vec::new();
        nerka_core::input::dlt::writer::Msg::log(nerka_core::Level::Info, "E", "A", "C", vec![]).encode(&mut buf);
        std::fs::write(&d, buf).unwrap();
        let ctx = egui::Context::default();
        let prefs = SearchPrefs::default();
        let mut t = Tab::open_many(&ctx, vec![a.clone(), b.clone()], &prefs, None);
        settle(&mut t, &ctx);
        t.pattern = "ERROR".into();
        t.start_search(&ctx);
        settle(&mut t, &ctx);
        assert_eq!(t.result.as_ref().unwrap().lines, [1, 2, 4]);
        t.goto_line(4); // b three

        t.set_files(&ctx, vec![b.clone(), a.clone()], &prefs, None);
        settle(&mut t, &ctx);
        assert_eq!(t.part_files, [(0, 3), (3, 2)]);
        assert_eq!(t.title(), "b.log + 1");
        assert_eq!(t.current_line(), 2, "still b three");
        assert_eq!(t.result.as_ref().unwrap().lines, [0, 2, 4], "the filter ran again");

        t.set_files(&ctx, vec![b.clone()], &prefs, None);
        settle(&mut t, &ctx);
        assert_eq!(t.file.as_ref().unwrap().line_count(), 3);
        assert_eq!(t.current_line(), 2);

        // A DLT file with a text file: not one view, nothing changes.
        t.set_files(&ctx, vec![b.clone(), d], &prefs, None);
        settle(&mut t, &ctx);
        assert_eq!(t.paths, std::slice::from_ref(&b));
        assert!(t.status.starts_with("Files not changed"), "{}", t.status);
        assert_eq!(t.file.as_ref().unwrap().line_count(), 3);

        t.set_files(&ctx, vec![b, a], &prefs, None);
        settle(&mut t, &ctx);
        assert_eq!(t.part_files, [(0, 3), (3, 2)]);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn files_of_different_kinds_open_as_separate_tabs() {
        let dir = std::env::temp_dir().join(format!("nerka-gui-mixed-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (a, d) = (dir.join("a.log"), dir.join("d.dlt"));
        std::fs::write(
            &a, "text
",
        )
        .unwrap();
        let mut buf = Vec::new();
        nerka_core::input::dlt::writer::Msg::log(nerka_core::Level::Info, "E", "A", "C", vec![]).encode(&mut buf);
        std::fs::write(&d, buf).unwrap();
        let ctx = egui::Context::default();
        let mut app = App::default();
        app.open_files(&ctx, vec![a, d]);
        settle_app(&mut app, &ctx);
        settle_app(&mut app, &ctx);
        assert_eq!(app.tabs.iter().map(|t| t.title()).collect::<Vec<_>>(), ["a.log", "d.dlt"]);
        let status = &app.tabs[app.active].status;
        assert!(status.contains("separate tabs") || status.starts_with("Indexed"), "{status}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_new_tab_takes_both_boxes_settings_from_the_prefs() {
        let dir = std::env::temp_dir().join(format!("nerka-gui-prefs-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let log = dir.join("app.log");
        std::fs::write(&log, LOG).unwrap();
        let prefs = SearchPrefs {
            mode: Mode::Text,
            options: MatcherOptions { fixed_string: false, case_insensitive: true, invert: false },
            search_mode: Mode::Auto,
            search_regex: true,
            search_ignore_case: true,
            ..SearchPrefs::default()
        };
        let ctx = egui::Context::default();
        let tab = Tab::open(&ctx, log, &prefs);
        let got = tab.search_prefs();
        assert_eq!(got, prefs);
        // Through the prefs file, as the next start reads it.
        assert_eq!(SearchPrefs::parse(&prefs.serialize()), prefs);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A mode set for a box (the selection's NFD, a history entry) isn't
    /// remembered for new tabs; only one picked in the box's list is.
    #[test]
    fn only_a_picked_mode_goes_into_the_prefs() {
        let dir = std::env::temp_dir().join(format!("nerka-gui-pickedmode-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let log = dir.join("app.log");
        std::fs::write(&log, LOG).unwrap();
        let mut lib = Library::new();
        lib.add(pack::parse_pack("vision.nerka-pack", PACK).unwrap()).unwrap();
        let prefs = SearchPrefs { mode: Mode::Auto, search_mode: Mode::Text, ..SearchPrefs::default() };
        let ctx = egui::Context::default();
        let mut tab = Tab::open(&ctx, log, &prefs);
        settle(&mut tab, &ctx);
        tab.picks.add(&lib, "vision.drops", Use::Include, None);
        tab.apply_filter_picks(&ctx, &lib).unwrap();
        tab.search.set("query", "level >= ERROR");
        assert_eq!((tab.mode, tab.search.mode), (Mode::Query, Mode::Query));
        let got = tab.search_prefs();
        assert_eq!((got.mode, got.search_mode), (Mode::Auto, Mode::Text));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn the_settings_window_takes_typing() {
        let ctx = egui::Context::default();
        let mut name = String::new();
        let mut ports = ["3490", "30490", "514"].map(str::to_owned);
        let mut saved = None;
        let mut rects = Vec::new();
        let mut frame =
            |events: Vec<egui::Event>, name: &mut String, ports: &mut [String; 3], rects: &mut Vec<egui::Rect>| {
                let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 600.0));
                let input = egui::RawInput { screen_rect: Some(screen), events, ..Default::default() };
                let mut out = ctx.run_ui(input, |ui| {
                    egui::Window::new("Settings").show(ui.ctx(), |ui| {
                        let fields = SettingsFields {
                            name,
                            ports,
                            limit: &mut "8".to_owned(),
                            local_time: &mut true,
                            mf4_local_time: &mut true,
                        };
                        let [dlt, someip, syslog] = settings_ui(ui, fields).ports;
                        assert!(someip.is_none() && syslog.is_none(), "only the DLT ports were typed in");
                        if let Some(p) = dlt {
                            saved = Some(p);
                        }
                    });
                });
                out.textures_delta.clear();
                // Where the fields' texts are: the name's hint, the ports.
                rects.clear();
                for want in ["e.g. OTA crash", "3490"] {
                    for s in &out.shapes {
                        if let egui::Shape::Text(t) = &s.shape
                            && t.galley.text() == want
                        {
                            rects.push(egui::Rect::from_min_size(t.pos, t.galley.size()));
                        }
                    }
                }
            };
        let click = |p: egui::Pos2| {
            vec![
                egui::Event::PointerMoved(p),
                egui::Event::PointerButton {
                    pos: p,
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: egui::Modifiers::NONE,
                },
                egui::Event::PointerButton {
                    pos: p,
                    button: egui::PointerButton::Primary,
                    pressed: false,
                    modifiers: egui::Modifiers::NONE,
                },
            ]
        };
        for _ in 0..3 {
            frame(vec![], &mut name, &mut ports, &mut rects);
        }
        assert!(rects.len() >= 2, "two text fields: {rects:?}");
        let (name_at, ports_at) = (rects[0].center(), rects[1].right_center() + egui::vec2(2.0, 0.0));
        frame(click(name_at), &mut name, &mut ports, &mut rects);
        frame(vec![egui::Event::Text("OTA crash".into())], &mut name, &mut ports, &mut rects);
        assert_eq!(name, "OTA crash");
        frame(click(ports_at), &mut name, &mut ports, &mut rects);
        frame(vec![egui::Event::Text(" 3489".into())], &mut name, &mut ports, &mut rects);
        assert_eq!(ports[0], "3490 3489");
        assert_eq!(saved, Some(vec![3490, 3489]));
    }

    #[test]
    fn boxes_read_their_text_by_mode() {
        let lit = MatcherOptions { fixed_string: true, ..Default::default() };
        let kind = |r: &Result<Filter, String>| match r {
            Ok(Filter::Regex(_)) => "text",
            Ok(Filter::Query(_)) => "query",
            Err(_) => "error",
        };
        let read = |mode, s: &str| {
            let (f, p) = read_box(mode, s, &lit, &[], &Signals::default());
            (kind(&f), p)
        };
        assert_eq!(
            read(Mode::Auto, "connection reset"),
            ("text", Preview::AutoText { text: "connection reset".into(), why: None, regex: false })
        );
        assert_eq!(read(Mode::Auto, "level >= ERROR"), ("query", Preview::AutoQuery("level >= ERROR".into())));
        let (k, p) = read(Mode::Auto, "level >= LOUD");
        assert!(k == "text" && matches!(p, Preview::AutoText { why: Some(_), .. }), "{p:?}");
        assert_eq!(read(Mode::Auto, ""), ("text", Preview::None));
        // Quoted text or a lone regex is a query that only searches text.
        assert_eq!(
            read(Mode::Auto, "\"ctid = BOOT\""),
            ("query", Preview::AutoText { text: "ctid = BOOT".into(), why: None, regex: false })
        );
        assert_eq!(read(Mode::Auto, "/SSL|CERT/i"), ("query", Preview::AutoRegex("/SSL|CERT/i".into())));
        // Regex ticked: Auto's plain words and its text are regexes.
        let re = MatcherOptions { fixed_string: false, ..Default::default() };
        let (f, p) = read_box(Mode::Auto, "cam[12] frame", &re, &[], &Signals::default());
        assert!(matches!(f, Ok(Filter::Regex(_))), "{p:?}");
        assert_eq!(p, Preview::AutoText { text: "cam[12] frame".into(), why: p_why(&p), regex: true });
        let (_, p) = read_box(Mode::Auto, "drop.* AND level >= ERROR", &re, &[], &Signals::default());
        assert!(matches!(p, Preview::AutoText { regex: true, .. }), "{p:?}");
        let (f, p) = read_box(Mode::Auto, "fr.me AND level >= ERROR", &re, &[], &Signals::default());
        assert_eq!((kind(&f), p), ("query", Preview::AutoQuery("/fr.me/ AND level >= ERROR".into())));
        // Text mode never reads a query; NFD mode never falls back.
        assert_eq!(read(Mode::Text, "level >= ERROR"), ("text", Preview::None));
        assert_eq!(read(Mode::Query, "level >= ERROR"), ("query", Preview::None));
        let (k, p) = read(Mode::Query, "connection reset");
        assert!(k == "error" && matches!(p, Preview::QueryError(_)), "{p:?}");

        // Invert in Auto wraps a query in NOT; ignore case reaches phrases.
        let file = nerka_core::TextFile::from_bytes("t", LOG.as_bytes().to_vec());
        let run = |mode, s: &str, o: &MatcherOptions| {
            let f = read_box(mode, s, o, &[], &Signals::default()).0.unwrap();
            nerka_core::search(&file, &f, &nerka_core::Progress::default()).unwrap().lines
        };
        let inv = MatcherOptions { invert: true, ..lit.clone() };
        assert_eq!(run(Mode::Auto, "timeout AND level >= ERROR", &lit), [5]);
        assert_eq!(run(Mode::Auto, "timeout AND level >= ERROR", &inv), [0, 1, 2, 3, 4]);
        let ci = MatcherOptions { case_insensitive: true, ..lit.clone() };
        assert_eq!(run(Mode::Auto, "CAM1 FRAME AND level >= ERROR", &ci), [1]);
        assert!(run(Mode::Auto, "CAM1 FRAME AND level >= ERROR", &lit).is_empty());
    }

    #[test]
    fn the_line_window_wraps_long_lines_only_with_wrap() {
        let long = format!("short\n{}", "word ".repeat(200));
        let file = nerka_core::TextFile::from_bytes("t", long.replace('\n', " | ").into_bytes());
        let mut d = LineDetail::of(&file, 0).unwrap();
        d.cells = vec![long.clone()];
        let ctx = egui::Context::default();
        // Rows of the message's galley, after a few frames.
        let rows = |d: &mut LineDetail| {
            let mut n = 0;
            for t in 0..3 {
                let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(900.0, 600.0));
                let input =
                    egui::RawInput { screen_rect: Some(screen), time: Some(t as f64 / 60.0), ..Default::default() };
                let mut out = ctx.run_ui(input, |ui| {
                    d.show(ui.ctx());
                });
                if t == 2 {
                    n = out
                        .shapes
                        .iter()
                        .find_map(|s| match &s.shape {
                            egui::Shape::Text(t) if t.galley.text() == long => Some(t.galley.rows.len()),
                            _ => None,
                        })
                        .expect("the message is shown");
                }
                out.textures_delta.clear();
            }
            n
        };
        assert!(rows(&mut d) > 2, "wrapped");
        d.wrap = false;
        assert_eq!(rows(&mut d), 2, "one row per line of the message");
    }

    #[test]
    fn bookmark_text_keeps_the_whole_line() {
        assert_eq!(bookmark_text("10:00:01\tECU1\tconnection lost"), "10:00:01 ECU1 connection lost");
        let long = "x".repeat(BOOKMARK_CHARS + 5);
        let t = bookmark_text(&long);
        assert_eq!(t.chars().count(), BOOKMARK_CHARS + 1);
        assert!(t.ends_with('…'));
    }

    #[test]
    fn a_preset_made_in_one_tab_sets_up_another() {
        let dir = std::env::temp_dir().join(format!("nerka-gui-preset-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (log1, log2) = (dir.join("one.log"), dir.join("two.log"));
        std::fs::write(&log1, LOG).unwrap();
        std::fs::write(&log2, format!("{LOG}[ERROR] cam1 frame drop again\n")).unwrap();
        let mut lib = Library::new();
        lib.add(pack::parse_pack("vision.nerka-pack", PACK).unwrap()).unwrap();
        let ctx = egui::Context::default();

        let mut one = Tab::open(&ctx, log1, &SearchPrefs::default());
        settle(&mut one, &ctx);
        one.picks.add(&lib, "vision.cam", Use::Include, None);
        one.picks.add(&lib, "vision.timeouts", Use::Exclude, None);
        one.picks.params.insert("cam".into(), ("cam1".into(), false));
        one.picks.light_source("vision.cam");
        one.apply_filter_picks(&ctx, &lib).unwrap();
        one.search.mode = Mode::Auto;
        one.search.pattern = "frame drop".into();
        one.run_search_box(&ctx);
        settle(&mut one, &ctx);
        let p = one.to_preset("cam triage");
        assert_eq!(p.filter, Some(BoxText::Selection));
        assert_eq!(p.search, Some(BoxText::Typed { tag: "auto".into(), text: "frame drop".into() }));
        // Through the file format, as it would be saved.
        let p = nerka_core::preset::parse_preset("t", &p.serialize()).unwrap();

        let mut two = Tab::open(&ctx, log2, &SearchPrefs::default());
        settle(&mut two, &ctx);
        two.use_preset(&ctx, &lib, &p);
        settle(&mut two, &ctx);
        assert_eq!(two.preset.as_deref(), Some("cam triage"));
        assert_eq!(two.pattern, one.pattern);
        assert_eq!(two.result.as_ref().unwrap().lines, [1, 6]);
        assert_eq!(two.search.hits.as_ref().unwrap().1.lines, [1, 6]);
        assert_eq!(two.line_colors.as_ref().expect("the colour came along").1.lines, [1, 6]);

        // A typed filter comes back as typed text, in its mode.
        one.mode = Mode::Auto;
        one.pattern = "disk full".into();
        one.start_search(&ctx);
        settle(&mut one, &ctx);
        let p = one.to_preset("typed");
        assert_eq!(p.filter, Some(BoxText::Typed { tag: "auto".into(), text: "disk full".into() }));
        two.use_preset(&ctx, &lib, &p);
        settle(&mut two, &ctx);
        assert_eq!((two.mode, two.result.as_ref().unwrap().lines.as_slice()), (Mode::Auto, &[4][..]));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn search_hits_show_the_line_colours_too() {
        let dir = std::env::temp_dir().join(format!("nerka-gui-hitcolours-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let log = dir.join("app.log");
        std::fs::write(&log, LOG).unwrap();
        let mut lib = Library::new();
        lib.add(pack::parse_pack("vision.nerka-pack", PACK).unwrap()).unwrap();
        let ctx = egui::Context::default();
        let mut tab = Tab::open(&ctx, log, &SearchPrefs::default());
        settle(&mut tab, &ctx);
        tab.picks.add(&lib, "vision.drops", Use::Include, None);
        tab.picks.light_source("vision.drops");
        tab.apply_filter_picks(&ctx, &lib).unwrap();
        tab.search.mode = Mode::Auto;
        tab.search.pattern = "cam1".into();
        tab.run_search_box(&ctx);
        settle(&mut tab, &ctx);
        assert!(tab.line_colors.is_some());

        let violet = library::parse_color("violet").unwrap();
        let tints: Vec<Color32> =
            [70, 90].iter().map(|&a| Color32::from_rgba_unmultiplied(violet.r(), violet.g(), violet.b(), a)).collect();
        let file = tab.file.clone().unwrap();
        let mut tinted = 0;
        for t in 0..2 {
            let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(900.0, 400.0));
            let input = egui::RawInput { screen_rect: Some(screen), time: Some(t as f64), ..Default::default() };
            let mut out = ctx.run_ui(input, |ui| {
                egui::CentralPanel::default().show(ui, |ui| {
                    tab.search_panel(ui, &*file, &SearchHistory::default());
                });
            });
            out.textures_delta.clear();
            tinted = out
                .shapes
                .iter()
                .filter(|s| matches!(&s.shape, egui::Shape::Rect(r) if tints.contains(&r.fill)))
                .count();
        }
        // Hits: lines 0 and 1 (cam1 frame drop); both are kept and coloured.
        assert_eq!(tinted, 2);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn line_colours_follow_the_selection_not_the_box_text() {
        let dir = std::env::temp_dir().join(format!("nerka-gui-colours-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let log = dir.join("app.log");
        std::fs::write(&log, LOG).unwrap();
        let mut lib = Library::new();
        lib.add(pack::parse_pack("vision.nerka-pack", PACK).unwrap()).unwrap();
        let ctx = egui::Context::default();
        let mut tab = Tab::open(&ctx, log, &SearchPrefs::default());
        settle(&mut tab, &ctx);

        tab.picks.add(&lib, "vision.errors", Use::Include, None);
        tab.picks.add(&lib, "vision.timeouts", Use::Include, None);
        tab.apply_filter_picks(&ctx, &lib).expect("runs");
        settle(&mut tab, &ctx);
        assert_eq!(tab.result.as_ref().unwrap().lines, [1, 2, 3, 4, 5]);
        tab.picks.light_source("vision.timeouts");
        tab.recolor_from(&ctx, &lib);
        settle(&mut tab, &ctx);
        assert_eq!(tab.line_colors.clone().expect("line colours").1.lines, [3, 5]);

        // Editing the Filter box by hand keeps them, among the lines the
        // new filter keeps.
        tab.pattern = "(level >= ERROR)".into();
        tab.start_search(&ctx);
        settle(&mut tab, &ctx);
        assert_eq!(tab.result.as_ref().unwrap().lines, [1, 2, 4, 5]);
        assert_eq!(tab.line_colors.clone().expect("after a manual edit").1.lines, [5]);

        // A node switched on after Apply colours too; one removed doesn't.
        tab.picks.light_source("vision.errors");
        tab.recolor_from(&ctx, &lib);
        settle(&mut tab, &ctx);
        assert_eq!(tab.line_colors.clone().unwrap().1.lines, [1, 2, 4, 5]);
        let keys: Vec<u64> = tab.picks.nodes.iter().map(|n| n.key).collect();
        for k in keys {
            tab.picks.remove(k);
        }
        tab.recolor_from(&ctx, &lib);
        settle(&mut tab, &ctx);
        assert!(tab.line_colors.is_none());
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Issue #8: a selection of only signal queries leaves the filter
    /// empty; their colours still go on the lines they match.
    #[test]
    fn signal_picks_colour_lines_without_a_filter() {
        let dir = std::env::temp_dir().join(format!("nerka-gui-sigcolours-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let log = dir.join("app.log");
        std::fs::write(&log, LOG).unwrap();
        let mut lib = Library::new();
        lib.add(pack::parse_pack("vision.nerka-pack", PACK).unwrap()).unwrap();
        let ctx = egui::Context::default();
        let mut tab = Tab::open(&ctx, log.clone(), &SearchPrefs::default());
        settle(&mut tab, &ctx);
        tab.picks.add(&lib, "vision.timeouts", Use::Signal, None);
        tab.picks.light_source("vision.timeouts");
        tab.apply_filter_picks(&ctx, &lib);
        settle(&mut tab, &ctx);
        assert!(tab.result.is_none(), "no filter");
        assert_eq!(tab.line_colors.clone().expect("line colours").1.lines, [3, 5]);

        // A tab that opens with the selection (a preset) colours once loaded.
        let p = tab.to_preset("p");
        let mut two = Tab::open(&ctx, log, &SearchPrefs::default());
        two.use_preset(&ctx, &lib, &p);
        settle(&mut two, &ctx);
        assert_eq!(two.line_colors.clone().expect("after loading").1.lines, [3, 5]);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// `--search-pick` at startup on a DLT file: the query names a DLT field,
    /// unknown until the file is read; it runs once it is (it was lost).
    #[test]
    fn a_library_search_picked_before_the_file_is_read_runs_after() {
        use nerka_core::input::dlt::writer::Msg;
        let dir = std::env::temp_dir().join(format!("nerka-gui-early-search-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let dlt = dir.join("t.dlt");
        let mut buf = Vec::new();
        for apid in ["NAV", "DIAG", "NAV"] {
            Msg::log(nerka_core::Level::Info, "ECU1", apid, "C", vec![]).encode(&mut buf);
        }
        std::fs::write(&dlt, buf).unwrap();
        let mut lib = Library::new();
        lib.add(
            pack::parse_pack(
                "p.nerka-pack",
                "pack p
query nav
    apid == NAV
",
            )
            .unwrap(),
        )
        .unwrap();
        let ctx = egui::Context::default();
        let mut tab = Tab::open(&ctx, dlt, &SearchPrefs::default());
        tab.search_for(&ctx, &lib, &Target::Library("p.nav".into()), true);
        settle(&mut tab, &ctx);
        assert_eq!(hits(&tab).iter().map(|(l, _)| *l).collect::<Vec<_>>(), [0, 2]);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// `bookmark` in the filter and the search: they run again when a
    /// bookmark is added, removed or gets a note.
    #[test]
    fn bookmark_queries_follow_the_bookmarks() {
        let dir = std::env::temp_dir().join(format!("nerka-gui-bookmark-query-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let log = dir.join("app.log");
        std::fs::write(&log, LOG).unwrap();
        let ctx = egui::Context::default();
        let mut tab = Tab::open(&ctx, log, &SearchPrefs::default());
        settle(&mut tab, &ctx);
        tab.set_filter("query", "level == WARN OR bookmark");
        tab.start_search(&ctx);
        tab.search.set("query", "bookmark ~ /look/");
        tab.run_search_box(&ctx);
        settle(&mut tab, &ctx);
        assert_eq!(tab.result.as_ref().unwrap().lines, [3]);

        tab.toggle_bookmark_at(1);
        settle(&mut tab, &ctx);
        assert_eq!(tab.result.as_ref().unwrap().lines, [1, 3]);
        assert!(hits(&tab).is_empty());

        tab.set_bookmark(5, "look here".into());
        settle(&mut tab, &ctx);
        assert_eq!(tab.result.as_ref().unwrap().lines, [1, 3, 5]);
        assert_eq!(hits(&tab).iter().map(|(l, _)| *l).collect::<Vec<_>>(), [5]);

        // The box's text changed since: the filter that ran is kept.
        tab.pattern = "disk".into();
        tab.toggle_bookmark_at(1);
        settle(&mut tab, &ctx);
        assert_eq!(tab.result.as_ref().unwrap().lines, [3, 5]);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A search chip's click removes its query: the others are searched
    /// again; removing the last one clears the search.
    /// The line window's Fields: every field of the line, with its value
    /// (DLT's ten and `proto`; a JSON record's keys, missing ones empty).
    #[test]
    fn the_line_window_lists_every_field() {
        use nerka_core::input::dlt::writer::{Msg, Value};
        let mut buf = Vec::new();
        Msg::log(nerka_core::Level::Warn, "ECU1", "NAV", "CTX", vec![Value::Str("hi".into()).into()]).encode(&mut buf);
        let dlt = nerka_core::DltFile::from_bytes("t.dlt", buf);
        let d = LineDetail::of(&dlt, 0).unwrap();
        let get = |d: &LineDetail, n: &str| d.fields.iter().find(|f| f.0 == n).and_then(|f| f.1.clone());
        assert_eq!(d.fields.len(), 11);
        assert_eq!(
            (get(&d, "ecu"), get(&d, "apid"), get(&d, "proto")),
            (Some("ECU1".into()), Some("NAV".into()), Some("DLT".into()))
        );
        assert_eq!(get(&d, "mid"), None, "a verbose message has no message id");

        use nerka_core::input::records::{RecordFile, RecordSplit};
        let text = b"{\"a\": 1, \"b\": {\"c\": \"x\"}}
{\"a\": 20}
"
        .to_vec();
        let json = RecordFile::from_bytes("t", text, RecordSplit::Json);
        let d = LineDetail::of(&json, 1).unwrap();
        assert_eq!(
            d.fields.iter().map(|f| (f.0.as_str(), f.1.clone())).collect::<Vec<_>>(),
            [("a", Some("20 (0x14)".into())), ("b.c", None)]
        );
    }

    #[test]
    fn removing_a_search_chip_drops_its_query() {
        use nerka_core::input::dlt::writer::Msg;
        let dir = std::env::temp_dir().join(format!("nerka-gui-chip-remove-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let dlt = dir.join("t.dlt");
        let mut buf = Vec::new();
        for apid in ["NAV", "DIAG", "NAV", "COM"] {
            Msg::log(nerka_core::Level::Info, "ECU1", apid, "C", vec![]).encode(&mut buf);
        }
        std::fs::write(&dlt, buf).unwrap();
        let mut lib = Library::new();
        let pack = "pack p\nquery nav\n    apid == NAV\nquery diag\n    apid == DIAG\n";
        lib.add(pack::parse_pack("p.nerka-pack", pack).unwrap()).unwrap();
        let ctx = egui::Context::default();
        let mut tab = Tab::open(&ctx, dlt, &SearchPrefs::default());
        settle(&mut tab, &ctx);
        tab.search_for(&ctx, &lib, &Target::Library("p.nav".into()), false);
        tab.search_for(&ctx, &lib, &Target::Library("p.diag".into()), true);
        settle(&mut tab, &ctx);
        assert_eq!(hits(&tab).iter().map(|(l, _)| *l).collect::<Vec<_>>(), [0, 1, 2]);

        tab.remove_search_term(&ctx, 0);
        settle(&mut tab, &ctx);
        assert_eq!(tab.search.terms.iter().map(|t| t.name.as_str()).collect::<Vec<_>>(), ["diag"]);
        assert_eq!(tab.search.pattern, SearchBox::text_of(&tab.search.terms));
        assert_eq!(hits(&tab).iter().map(|(l, _)| *l).collect::<Vec<_>>(), [1]);

        tab.remove_search_term(&ctx, 0);
        assert!(tab.search.active.is_none() && tab.search.pattern.is_empty());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn library_picks_filter_then_search_inside_it() {
        let dir = std::env::temp_dir().join(format!("nerka-gui-lib-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let log = dir.join("app.log");
        std::fs::write(&log, LOG).unwrap();
        let mut lib = Library::new();
        lib.add(pack::parse_pack("vision.nerka-pack", PACK).unwrap()).unwrap();

        let ctx = egui::Context::default();
        let mut tab = Tab::open(&ctx, log, &SearchPrefs::default());
        settle(&mut tab, &ctx);

        // Filter: errors about cam1, without timeouts.
        tab.picks.add(&lib, "vision.cam", Use::Include, None);
        tab.picks.add(&lib, "vision.timeouts", Use::Exclude, None);
        assert_eq!(tab.apply_filter_picks(&ctx, &lib), None, "a missing parameter is reported, not run");
        assert!(tab.picks.error.as_deref().unwrap().contains("'$cam'"), "{:?}", tab.picks.error);
        tab.picks.params.insert("cam".into(), ("cam1".into(), false));
        let nfd = tab.apply_filter_picks(&ctx, &lib).expect("runs");
        assert_eq!(nfd, "(level >= ERROR) AND (raw ~ /cam1 /) AND NOT (/timeout/)");
        assert_eq!(tab.mode, Mode::Query);
        settle(&mut tab, &ctx);
        assert_eq!(tab.result.as_ref().unwrap().lines, [1]);

        // Search inside the filter result only; then widen the filter and
        // the search follows it.
        tab.search_for(&ctx, &lib, &Target::Library("vision.drops".into()), false);
        tab.search_for(&ctx, &lib, &Target::Library("vision.timeouts".into()), true);
        settle(&mut tab, &ctx);
        assert_eq!(hits(&tab), [(1, vec!["drops"])]);
        let colors = &tab.search.hits.as_ref().unwrap().0.colors;
        assert_eq!(colors[0], library::parse_color("violet").unwrap());
        assert_ne!(colors[1], colors[0]);

        let excluded = tab.picks.nodes.iter().find(|n| n.role == Use::Exclude).unwrap().key;
        tab.picks.remove(excluded);
        tab.apply_selection(&ctx, &lib).unwrap();
        settle(&mut tab, &ctx);
        assert_eq!(tab.result.as_ref().unwrap().lines, [1, 5]);
        assert_eq!(hits(&tab), [(1, vec!["drops"]), (5, vec!["timeouts"])]);

        // Line colours are off by default; switched on, the later pick wins
        // where a line matches several.
        assert!(tab.line_colors.is_none());
        tab.picks.light_source("vision.cam");
        tab.recolor(&ctx);
        settle(&mut tab, &ctx);
        let (a, h) = tab.line_colors.clone().expect("line colours");
        assert_eq!(h.lines, [1, 5]);
        assert_eq!(a.names, ["cam"]);

        // The main view shows only the kept lines; going to a hidden line
        // lands on the next kept one. n / N jump between search hits.
        assert_eq!(tab.main_rows(), 2);
        tab.goto_line(0);
        assert_eq!(tab.current_line(), 1);
        tab.goto_line(3);
        assert_eq!(tab.current_line(), 5);
        tab.jump_match(false);
        assert_eq!(tab.current_line(), 1);
        tab.jump_match(true);
        assert_eq!(tab.current_line(), 5);
        assert_eq!(tab.hits_view.selected, Some(1));

        // Editing the box drops the chips; the text is a plain query.
        tab.search.pattern.push_str(" OR /disk/");
        assert!(tab.search.current_terms().is_none());
        tab.run_search_box(&ctx);
        settle(&mut tab, &ctx);
        assert_eq!(tab.search.hits.as_ref().unwrap().1.lines, [1, 5]);

        // Without a filter the main view and the search cover the whole
        // file; the cursor stays on its line. Clear removes the filter.
        tab.clear_filter(&ctx);
        assert!(tab.pattern.is_empty());
        settle(&mut tab, &ctx);
        assert_eq!(tab.main_rows(), 6);
        assert_eq!(tab.current_line(), 5);
        assert_eq!(tab.search.hits.as_ref().unwrap().1.lines, [0, 1, 2, 3, 4, 5]);
        tab.clear_search();
        assert!(tab.search.hits.is_none());

        // Typed text, as a user would: literal by default, then a regex.
        tab.search.mode = Mode::Text;
        tab.search.pattern = "Frame drop".into();
        tab.search.ignore_case = true;
        tab.run_search_box(&ctx);
        settle(&mut tab, &ctx);
        assert_eq!(tab.search.hits.as_ref().unwrap().1.lines, [0, 1, 2]);
        tab.search.pattern = "cam[2]".into();
        tab.run_search_box(&ctx);
        settle(&mut tab, &ctx);
        assert!(tab.search.hits.as_ref().unwrap().1.is_empty(), "literal by default");
        tab.search.regex = true;
        tab.run_search_box(&ctx);
        settle(&mut tab, &ctx);
        assert_eq!(tab.search.hits.as_ref().unwrap().1.lines, [2]);

        // Add to search: the typed regex becomes a query, OR'ed with the
        // library query, each with its own hits and colour.
        tab.search_for(&ctx, &lib, &Target::Library("vision.timeouts".into()), true);
        assert_eq!(tab.search.pattern, "/cam[2]/i OR (/timeout/)");
        assert_eq!(tab.search.mode, Mode::Query);
        settle(&mut tab, &ctx);
        assert_eq!(hits(&tab), [(2, vec!["cam[2]"]), (3, vec!["timeouts"]), (5, vec!["timeouts"])]);
        // Search for it (replace): only the new query is left.
        tab.search_for(&ctx, &lib, &Target::Library("vision.drops".into()), false);
        settle(&mut tab, &ctx);
        assert_eq!(tab.search.pattern, "(/frame drop/)");
        assert_eq!(hits(&tab), [(0, vec!["drops"]), (1, vec!["drops"]), (2, vec!["drops"])]);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn named_groups_of_picked_queries_are_signals() {
        let dir = std::env::temp_dir().join(format!("nerka-gui-qsig-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let log = dir.join("app.log");
        std::fs::write(
            &log,
            "10:00:01 v=10 gear=P
boot
10:00:02 v=60 gear=D
10:00:03 WARN v=80 gear=D
",
        )
        .unwrap();
        let mut lib = Library::new();
        let pack_text = r"pack p
query speed
    /v=(?<v>\d+) gear=(?<gear>\w)/
query clock  signals: t:time
    /^(?<t>\S+)/
";
        lib.add(pack::parse_pack("p.nerka-pack", pack_text).unwrap()).unwrap();
        let ctx = egui::Context::default();
        let mut tab = Tab::open(&ctx, log, &SearchPrefs::default());
        settle(&mut tab, &ctx);
        let mut saved = Signals::default();
        tab.refresh_signals(&ctx, &lib, &saved, 0);
        assert!(tab.signals.is_empty(), "nothing picked, no time in the file");

        // A signal pick: its groups are signals; the filter stays empty.
        tab.picks.add(&lib, "p.speed", Use::Signal, None);
        tab.picks.add(&lib, "p.clock", Use::Signal, None);
        tab.refresh_signals(&ctx, &lib, &saved, 0);
        let kinds: Vec<(&str, &str)> = tab.signals.iter().map(|d| (d.name(), d.kind().as_str())).collect();
        assert_eq!(kinds, [("v", "num"), ("gear", "enum"), ("t", "time")]);
        assert_eq!(tab.picks.expand(&lib).unwrap().exp.nfd, "");
        tab.mode = Mode::Query;
        tab.pattern = "@gear == D AND level >= WARN".into();
        tab.start_search(&ctx);
        settle(&mut tab, &ctx);
        assert_eq!(tab.result.as_ref().unwrap().lines, [3]);

        // A chosen kind wins over the values; a saved signal over the query's.
        tab.query_kind.insert("v".into(), SignalKind::Enum);
        tab.choices_gen += 1;
        saved.set(nerka_core::SignalDef::parse_spec(r"gear:enum=/gear=(?<gear>[A-Z])/").unwrap());
        tab.refresh_signals(&ctx, &lib, &saved, 1);
        assert_eq!(tab.signals.get("v").unwrap().kind(), SignalKind::Enum);
        assert!(tab.signals.get("gear").unwrap().queries().is_empty(), "the saved one");

        // A filtered signal can't be used in a filter.
        tab.query_scope.insert("v".into(), SignalScope::Filtered);
        tab.choices_gen += 1;
        tab.refresh_signals(&ctx, &lib, &saved, 1);
        tab.pattern = "@v == 10".into();
        assert!(!tab.start_search(&ctx));
        assert!(tab.status.contains("filtered lines only"), "{}", tab.status);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_tab_filters_by_its_signals() {
        let dir = std::env::temp_dir().join(format!("nerka-gui-signals-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let log = dir.join("app.log");
        std::fs::write(&log, "v=10\nv=60\nboot\nWARN v=80\n").unwrap();
        let ctx = egui::Context::default();
        let mut tab = Tab::open(&ctx, log, &SearchPrefs::default());
        settle(&mut tab, &ctx);

        // Unknown until the tab defines it: Auto reads it as text.
        tab.mode = Mode::Auto;
        tab.pattern = "@v > 50".into();
        let sig = (&tab.signals, tab.signals_gen);
        let p = tab.filter_preview.get(tab.mode, &tab.pattern, &tab.options, &[], sig);
        assert!(p_why(&p).is_some_and(|w| w.contains("unknown signal '@v'")), "{p:?}");

        tab.signals.set(nerka_core::SignalDef::parse_spec(r"v=/v=(?<v>\d+)/").unwrap());
        tab.signals_changed(&ctx);
        settle(&mut tab, &ctx);
        // The state holds on "boot" too.
        assert_eq!(tab.result.as_ref().unwrap().lines, [1, 2, 3]);
        // The panel's events were collected in the background.
        let file = tab.file.clone().unwrap();
        let series = tab.signals.get("v").unwrap().cached_series(&*file).expect("collected");
        assert_eq!(series.state(2).map(|(row, v)| (row, v.to_value().to_string())), Some((1, "60".into())));
        tab.pattern = "@v.event".into();
        tab.start_search(&ctx);
        settle(&mut tab, &ctx);
        assert_eq!(tab.result.as_ref().unwrap().lines, [0, 1, 3]);
        tab.pattern = "@v > 50".into();
        tab.start_search(&ctx);
        settle(&mut tab, &ctx);
        let sig = (&tab.signals, tab.signals_gen);
        let p = tab.filter_preview.get(tab.mode, &tab.pattern, &tab.options, &[], sig);
        assert!(matches!(&p, Preview::AutoQuery(q) if q == "@v > 50"), "{p:?}");

        // The Search box knows them too.
        tab.search.pattern = "level >= WARN AND @v > 70".into();
        tab.run_search_box(&ctx);
        settle(&mut tab, &ctx);
        assert_eq!(hits(&tab).iter().map(|h| h.0).collect::<Vec<_>>(), [3]);

        // Removing the signal runs the filter again: Auto now reads the box
        // as text, and NFD mode says why it can't run.
        tab.signals.remove("v");
        tab.signals_changed(&ctx);
        settle(&mut tab, &ctx);
        assert!(tab.result.as_ref().unwrap().lines.is_empty());
        tab.mode = Mode::Query;
        assert!(!tab.start_search(&ctx));
        assert!(tab.status.contains("unknown signal '@v' (no signals are defined)"), "{}", tab.status);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
