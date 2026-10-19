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
mod job;
mod library;
mod prefs;
mod presets;
mod recall;
mod view;

use eframe::egui::{self, Color32, Key, KeyboardShortcut, Modifiers, RichText};
use job::{Job, Poll};
use library::{Applied, LibraryView, Picks, Target, Term, Use};
use nerka_core::dsl::{AutoRead, Plain, read_auto};
use nerka_core::pack::{self, Library, Role};
use nerka_core::preset::{BoxText, Preset};
use nerka_core::{
    Bookmarks, FieldDef, Filter, Highlighter, LineNo, LogSource, Matcher, MatcherOptions, Query, SearchHistory,
    SearchHits, SearchResult, SearchSet, text_to_nfd,
};
use prefs::SearchPrefs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use view::{Columns, Highlight, SearchMarks, ViewOutput, ViewState, log_view};

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
            _ => {
                initial.push(PathBuf::from(a));
                continue;
            }
        };
        preset = args.next().map(|v| (tag, v.to_string_lossy().into_owned()));
    }
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1280.0, 800.0])
            .with_title("nerka")
            .with_icon(icon::icon()),
        ..Default::default()
    };
    eframe::run_native(
        "nerka",
        options,
        Box::new(move |cc| {
            let mut app = App { pack_paths: packs, show_library, ..Default::default() };
            app.load_history();
            app.load_prefs();
            app.load_library();
            app.presets.load();
            let mut picks = Picks::default();
            picks.params = params;
            for (role, id) in &startup {
                picks.add(&app.library, id, *role, None);
            }
            for id in &highlight {
                picks.light_source(id);
            }
            for p in initial {
                app.open(&cc.egui_ctx, p);
                // A filter given on the command line applies to every file it names.
                if let (Some((tag, pattern)), Some(tab)) = (&preset, app.tabs.last_mut()) {
                    tab.set_filter(tag, pattern);
                }
                if let Some(tab) = app.tabs.last_mut() {
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

/// The mode list of a box.
fn mode_combo(ui: &mut egui::Ui, id: &str, mode: &mut Mode) {
    egui::ComboBox::from_id_salt(id)
        .width(58.0)
        .selected_text(mode.label())
        .show_ui(ui, |ui| {
            for m in [Mode::Auto, Mode::Text, Mode::Query] {
                ui.selectable_value(mode, m, m.label()).on_hover_text(m.describe());
            }
        })
        .response
        .on_hover_text(mode.describe());
}

/// How a box will be read, for the line under it.
#[derive(Debug, Clone, PartialEq)]
enum Preview {
    /// Nothing to say (empty box, or Text / valid NFD: the mode says it).
    None,
    /// Auto: literal text, with why it isn't a query, if it looked like one.
    AutoText { text: String, why: Option<String> },
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
) -> (Result<Filter, String>, Preview) {
    let text = |t: &str, o: MatcherOptions| {
        Matcher::new(t, o).map(Filter::Regex).map_err(|e| {
            let detail = std::error::Error::source(&e).map(|s| s.to_string()).unwrap_or_default();
            format!("Invalid regex: {}", detail.lines().last().unwrap_or(""))
        })
    };
    match mode {
        Mode::Text => (text(pattern, options.clone()), Preview::None),
        Mode::Query => match Query::compile_for(pattern, schema) {
            Ok(q) => (Ok(Filter::Query(q)), Preview::None),
            Err(e) if pattern.trim().is_empty() => (Err(format!("Query error: {e}")), Preview::None),
            Err(e) => (Err(format!("Query error: {e}")), Preview::QueryError(e.to_string())),
        },
        Mode::Auto => {
            let literal = MatcherOptions { fixed_string: true, ..options.clone() };
            match read_auto(pattern, options.case_insensitive, schema) {
                AutoRead::Text(t) => {
                    let preview =
                        if t.is_empty() { Preview::None } else { Preview::AutoText { text: t.clone(), why: None } };
                    (text(&t, literal), preview)
                }
                AutoRead::NotQuery { text: t, err } => {
                    let why = Some(err.to_string());
                    (text(&t, literal), Preview::AutoText { text: t, why })
                }
                AutoRead::Query { nfd, query, plain } => {
                    // A query that is only a text search is shown as one.
                    let preview = match plain {
                        Some(Plain::Text(text)) => Preview::AutoText { text, why: None },
                        Some(Plain::Regex { pattern, ignore_case }) => {
                            Preview::AutoRegex(format!("/{pattern}/{}", if ignore_case { "i" } else { "" }))
                        }
                        None => Preview::AutoQuery(nfd.clone()),
                    };
                    if !options.invert {
                        return (Ok(Filter::Query(query)), preview);
                    }
                    let not = Query::compile_for(&format!("NOT ({nfd})"), schema).map(Filter::Query);
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
        Preview::AutoText { text, why } => {
            ui.horizontal(|ui| {
                ui.label(small("→ text:".into()).weak());
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
    fn get(&mut self, mode: Mode, pattern: &str, options: &MatcherOptions, schema: &[FieldDef]) -> Preview {
        let key =
            format!("{mode:?}|{}|{}|{}|{pattern}", options.fixed_string, options.case_insensitive, options.invert);
        if self.key != key || self.preview.is_none() {
            self.preview = Some(read_box(mode, pattern, options, schema).1);
            self.key = key;
        }
        self.preview.clone().unwrap_or(Preview::None)
    }
}

/// One open file with its own filter, view positions and bookmarks.
struct Tab {
    path: PathBuf,
    file: Option<Arc<dyn LogSource>>,
    loading: Option<Job<Box<dyn LogSource>>>,
    /// The lines the filter keeps (all lines without a filter).
    main_view: ViewState,
    /// The search hits.
    hits_view: ViewState,
    columns: Columns,

    mode: Mode,
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
    /// Bookmarks whose line is unfolded in the panel.
    marks_open: std::collections::HashSet<LineNo>,
    /// The line shown whole in the line window (Enter), if open.
    detail: Option<LineDetail>,
    status: String,
}

/// One line shown whole: its cells as the source formats them (a DLT
/// payload keeps its own line breaks), read when the window opened.
struct LineDetail {
    line: LineNo,
    cells: Vec<String>,
    /// Column names of a table source; empty for plain text.
    names: Vec<&'static str>,
    wrap: bool,
}

/// Text shown in the line window at most; Copy still copies all of it.
const DETAIL_MAX_BYTES: usize = 1 << 20;

impl LineDetail {
    fn of(file: &dyn LogSource, line: LineNo) -> Option<Self> {
        let cells = file.line_detail(line)?;
        let names = file.columns().iter().map(|c| c.name).collect();
        Some(LineDetail { line, cells, names, wrap: true })
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
                    let lines = msg.lines().count().max(1);
                    let s = if lines == 1 { "" } else { "s" };
                    ui.weak(format!("{} characters, {lines} line{s}", msg.chars().count()));
                });
                if table {
                    egui::Grid::new("line-detail-cells").num_columns(2).spacing([12.0, 2.0]).show(ui, |ui| {
                        for (name, value) in self.names.iter().zip(&self.cells[..self.cells.len() - 1]) {
                            ui.label(RichText::new(*name).strong());
                            ui.monospace(value);
                            ui.end_row();
                        }
                    });
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
                let scroll = if self.wrap { egui::ScrollArea::vertical() } else { egui::ScrollArea::both() };
                scroll.auto_shrink([false, false]).show(ui, |ui| {
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
            Mode::Auto => match read_auto(text, self.ignore_case, &[]) {
                AutoRead::Query { nfd, .. } => format!("({nfd})"),
                AutoRead::Text(t) | AutoRead::NotQuery { text: t, .. } => {
                    text_to_nfd(&t, &MatcherOptions { fixed_string: true, ..self.options() })
                }
            },
        };
        let name: String = text.chars().take(24).collect();
        Some(Term { name, nfd, color: library::palette(0) })
    }
}

impl Tab {
    fn open(ctx: &egui::Context, path: PathBuf, prefs: &SearchPrefs) -> Self {
        let p = path.clone();
        Self {
            path,
            file: None,
            loading: Some(Job::spawn(ctx, move |prog| nerka_core::open(&p, prog))),
            main_view: ViewState::default(),
            hits_view: ViewState::default(),
            columns: Columns::default(),
            mode: prefs.mode,
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
            search: SearchBox::default(),
            filter_recall: recall::Recall::default(),
            chips_open: false,
            filter_preview: PreviewCache::default(),
            bookmarks: Bookmarks::default(),
            marks_open: Default::default(),
            detail: None,
            status: String::new(),
        }
    }

    fn title(&self) -> String {
        self.path.file_name().map_or_else(|| self.path.display().to_string(), |n| n.to_string_lossy().into_owned())
    }

    fn is_busy(&self) -> bool {
        self.loading.is_some() || self.searching.is_some() || self.search.job.is_some() || self.colors_job.is_some()
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
    }

    fn search_prefs(&self) -> SearchPrefs {
        SearchPrefs { mode: self.mode, options: self.options.clone() }
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

    fn build_filter(&self) -> Result<Filter, String> {
        let schema = self.file.as_ref().map(|f| f.schema()).unwrap_or_default();
        read_box(self.mode, &self.pattern, &self.options, schema).0
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
            self.goto_line(cur);
            self.status.clear();
            self.recolor(ctx);
            self.rerun_search(ctx);
            return false;
        }
        match self.build_filter() {
            Ok(f) => {
                let f = Arc::new(f);
                let ff = f.clone();
                let job = Job::spawn(ctx, move |prog| nerka_core::search(&*file, &ff, prog));
                self.searching = Some((f, job));
                true
            }
            Err(e) => {
                self.status = e;
                false
            }
        }
    }

    /// Advance background jobs. Returns `false` if the file failed to open
    /// (the tab should then be closed; `status` holds the reason).
    fn poll_jobs(&mut self, ctx: &egui::Context) -> bool {
        if let Some(job) = &self.loading {
            match job.poll() {
                Poll::Running => ctx.request_repaint_after(Duration::from_millis(50)),
                Poll::Done(r, took) => {
                    self.loading = None;
                    match r {
                        Ok(f) => {
                            self.status = format!("Indexed {} lines in {took:.2?}", f.line_count());
                            self.bookmarks = Bookmarks::load_for(&self.path).unwrap_or_else(|e| {
                                self.status = format!("Could not read bookmarks: {e}");
                                Bookmarks::default()
                            });
                            self.columns = Columns::from_defs(f.columns());
                            self.file = Some(Arc::from(f));
                            if !self.pattern.trim().is_empty() {
                                self.start_search(ctx);
                            } else {
                                self.rerun_search(ctx);
                            }
                        }
                        Err(nerka_core::Error::Cancelled) => return false,
                        Err(e) => {
                            let src = std::error::Error::source(&e).map(|s| format!(" ({s})")).unwrap_or_default();
                            self.status = format!("Cannot open {}: {e}{src}", self.path.display());
                            return false;
                        }
                    }
                }
            }
        }
        if let Some((_, job)) = &self.searching {
            match job.poll() {
                Poll::Running => ctx.request_repaint_after(Duration::from_millis(50)),
                Poll::Done(r, took) => {
                    match r {
                        Ok(res) => {
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

    /// What the Search box asks for: one query per picked library query, or
    /// the box's text as one query.
    fn build_search(&self) -> Result<ActiveSearch, String> {
        if let Some(terms) = self.search.current_terms() {
            let schema = self.file.as_ref().map(|f| f.schema()).unwrap_or_default();
            let include = terms
                .iter()
                .map(|t| {
                    Query::compile_for(&t.nfd, schema)
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
        let f = read_box(self.search.mode, &self.search.pattern, &self.search.options(), schema)
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
                self.search.active = Some(Arc::new(a));
                self.rerun_search(ctx);
            }
            Err(e) => self.status = e,
        }
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
    /// switched on, which kept lines each one matches. Runs after the
    /// filter result is in, and when a colour is switched on or off.
    fn recolor(&mut self, ctx: &egui::Context) {
        if let Some((_, j)) = self.colors_job.take() {
            j.cancel();
        }
        self.line_colors = None;
        let (Some(a), Some(res), Some(file)) = (self.colors_exp.clone(), self.result.clone(), self.file.clone()) else {
            return;
        };
        if self.searching.is_some() {
            return;
        }
        let (mut include, mut names, mut colors) = (Vec::new(), Vec::new(), Vec::new());
        for (i, p) in a.exp.parts.iter().enumerate() {
            if p.role != Role::Include || !self.picks.is_lit(a.keys[i]) {
                continue;
            }
            let Ok(q) = Query::compile(a.exp.part_nfd(p)) else { continue };
            include.push(Filter::Query(q));
            names.push(a.names[i].clone());
            colors.push(a.colors[i]);
        }
        if include.is_empty() {
            return;
        }
        let a =
            Arc::new(ActiveSearch { set: SearchSet { include, exclude: vec![] }, names, colors, highlighters: vec![] });
        let aa = a.clone();
        let job = Job::spawn(ctx, move |prog| nerka_core::search_within(&*file, Some(&res.lines), &aa.set, prog));
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

    fn save_bookmarks(&mut self) {
        if let Err(e) = self.bookmarks.save_for(&self.path) {
            self.status = format!("Could not save bookmarks: {e}");
        }
    }

    /// Status messages and context-menu actions from a log view.
    fn view_output(&mut self, out: &ViewOutput, line_of_row: impl Fn(u64) -> LineNo) {
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
            self.detail = LineDetail::of(&**file, line_of_row(row));
        }
    }

    /// Returns true when the panel's close button was clicked.
    fn bookmarks_panel(&mut self, ui: &mut egui::Ui, file: &dyn LogSource) -> bool {
        let mut close = false;
        ui.horizontal(|ui| {
            ui.heading("Bookmarks");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                close = close_button(ui).on_hover_text("Close panel").clicked();
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
        let mut fold = None;
        egui::ScrollArea::vertical().show(ui, |ui| {
            for (line, note) in self.bookmarks.iter() {
                let open = self.marks_open.contains(&line);
                ui.horizontal(|ui| {
                    if ui.link(format!("{}", line + 1)).on_hover_text("Go to the line").clicked() {
                        jump = Some(line);
                    }
                    let tip = if open { "Fold the line" } else { "Show the whole line" };
                    if ui.small_button(if open { "▾" } else { "▸" }).on_hover_text(tip).clicked() {
                        fold = Some(line);
                    }
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
        let changed = !edits.is_empty() || remove.is_some();
        for (l, t) in edits {
            self.bookmarks.set_note(l, t);
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
            let mut go = false;
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
            mode_combo(ui, "filter-mode", &mut self.mode);
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
                if self.mode != Mode::Query {
                    ui.checkbox(&mut self.options.invert, "Invert");
                    ui.checkbox(&mut self.options.case_insensitive, "Ignore case")
                        .on_hover_text("For the text; a /regex/ or \"string\" written out says itself");
                }
                if self.mode == Mode::Text {
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
            if go && self.start_search(ui.ctx()) {
                searched = Some((self.filter_tag(), self.pattern.clone()));
            }
        });
        if let Some(a) = self.filter_exp.clone().filter(|a| a.exp.nfd == self.pattern) {
            let picks = &self.picks;
            let lines = expansion_lines(&a, &|k| picks.is_lit(k));
            if let Some(key) = chips_view(ui, &lines, &mut self.chips_open) {
                self.picks.toggle_lit(key);
                self.recolor(ui.ctx());
            }
        } else {
            let schema = self.file.as_ref().map(|f| f.schema()).unwrap_or_default();
            let preview = self.filter_preview.get(self.mode, &self.pattern, &self.options, schema);
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
        if let Some((_, hits)) = self.search.hits.clone() {
            let out = log_view(
                ui,
                "hits",
                &mut self.hits_view,
                hits.len() as u64,
                &|row| hits.lines[row as usize],
                file,
                &mut self.columns,
                &Highlight { highlighter: None, bookmarks: Some(&self.bookmarks), search: marks.as_ref(), lines: None },
            );
            self.view_output(&out, |row| hits.lines[row as usize]);
            // Selecting a hit moves the main view to it.
            let picked = out.clicked.or(if out.has_focus { self.hits_view.selected } else { None });
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
            mode_combo(ui, "search-mode", &mut self.search.mode);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                go |= ui.button("Find").clicked();
                clear = ui.add_enabled(self.search.active.is_some(), egui::Button::new("Clear")).clicked();
                if self.search.mode != Mode::Query {
                    ui.checkbox(&mut self.search.ignore_case, "Ignore case");
                }
                if self.search.mode == Mode::Text {
                    ui.checkbox(&mut self.search.regex, "Regex");
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
            chips_view(ui, &lines, &mut self.chips_open);
        } else {
            let schema = self.file.as_ref().map(|f| f.schema()).unwrap_or_default();
            let s = &mut self.search;
            let preview = s.preview.get(s.mode, &s.pattern, &s.options(), schema);
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
            },
        );
        self.view_output(&out, line_of_row);
    }
}

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
/// and colour, its NFD, and (filter queries) the key of its selection node
/// with whether its lines are coloured.
struct ChipLine<'a> {
    joiner: &'static str,
    name: &'a str,
    color: Color32,
    nfd: &'a str,
    origin: &'a str,
    lit: Option<(u64, bool)>,
}

/// The picked queries under a box. Folded (the default): one row of name
/// chips, the NFD on hover. Unfolded (▸ / ▾): one line per query with its
/// NFD. A chip with `lit` switches its line colour (filled when on);
/// returns the node whose chip was clicked.
fn chips_view(ui: &mut egui::Ui, lines: &[ChipLine<'_>], open: &mut bool) -> Option<u64> {
    const MAX_LINES: usize = 8;
    let mut clicked = None;
    let mut chip = |ui: &mut egui::Ui, l: &ChipLine<'_>, folded: bool| {
        let tip = |extra: &str| {
            let nfd = if folded { format!("\n{}", l.nfd) } else { String::new() };
            format!("{}{nfd}{extra}", l.origin)
        };
        match l.lit {
            Some((key, on)) => {
                let what = if on { "\nLines coloured: click to stop" } else { "\nClick to colour its lines" };
                let r = library::chip(ui, l.name, l.color, on).interact(egui::Sense::click());
                if r.on_hover_text(tip(what)).clicked() {
                    clicked = Some(key);
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
            for l in lines {
                if !l.joiner.is_empty() {
                    ui.label(RichText::new(l.joiner).small().weak());
                }
                chip(ui, l, true);
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
            chip(ui, l, false);
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
            };
            if p.role == Role::Include {
                includes += 1;
            }
            let origin = if p.query.starts_with("_sel.") { "local query (not in a pack)" } else { p.query.as_str() };
            ChipLine {
                joiner,
                name: &a.names[i],
                color: a.colors[i],
                nfd: a.exp.part_nfd(p),
                origin,
                lit: (p.role == Role::Include).then(|| (a.keys[i], lit(a.keys[i]))),
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
            origin: "search query",
            lit: None,
        })
        .collect()
}

#[derive(Default)]
struct App {
    tabs: Vec<Tab>,
    active: usize,
    show_bookmarks: bool,
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
    presets: presets::Presets,
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

fn prefs_path() -> Option<PathBuf> {
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
            library::Action::Edited { renamed } => {
                for t in &mut self.tabs {
                    if let Some((old, new)) = &renamed {
                        t.picks.renamed(old, new);
                    }
                    t.picks.refresh(&self.library);
                }
            }
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
        let Some(path) = prefs_path() else { return };
        match SearchPrefs::load(&path) {
            Ok(p) => self.prefs = p,
            Err(e) => self.status = format!("Could not read preferences: {e}"),
        }
    }

    /// Remember the active tab's mode and options if the user changed them.
    fn save_prefs_from(&mut self, prefs: SearchPrefs) {
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
        if let Some(i) = self.tabs.iter().position(|t| same_file(&t.path, &path)) {
            self.active = i;
            return;
        }
        self.tabs.push(Tab::open(ctx, path, &self.prefs));
        self.active = self.tabs.len() - 1;
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
        for p in dropped {
            self.open(ctx, p);
        }
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
        if shortcut(Modifiers::NONE, Key::F1) {
            self.about = true;
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

    fn pick_files(&mut self, ctx: &egui::Context) {
        if let Some(paths) = rfd::FileDialog::new()
            .add_filter("Logs and traces", &["log", "txt", "out", "dlt"])
            .add_filter("DLT traces", &["dlt"])
            .add_filter("All files", &["*"])
            .pick_files()
        {
            for p in paths {
                self.open(ctx, p);
            }
        }
    }

    fn tab_bar(&mut self, ui: &mut egui::Ui) {
        let (mut select, mut close, mut close_others) = (None, None, None);
        egui::ScrollArea::horizontal().show(ui, |ui| {
            ui.horizontal(|ui| {
                for (i, t) in self.tabs.iter().enumerate() {
                    let label = if t.is_busy() { format!("⏳ {}", t.title()) } else { t.title() };
                    let r = ui.selectable_label(i == self.active, label).on_hover_text(t.path.display().to_string());
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
                        if ui.button("Copy full path").clicked() {
                            ui.ctx().copy_text(t.path.display().to_string());
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
        if let Some(i) = close {
            self.close(i);
        } else if let Some(i) = close_others {
            self.close_others(i);
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
        let title = match self.tabs.get(self.active) {
            Some(t) => format!("nerka — {}", t.path.display()),
            None => "nerka".to_owned(),
        };
        if title != self.window_title {
            ctx.send_viewport_cmd(egui::ViewportCommand::Title(title.clone()));
            self.window_title = title;
        }
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.poll_jobs(&ctx);
        self.handle_input(&ctx);
        self.update_title(&ctx);
        self.goto_dialog(&ctx);
        self.about_dialog(&ctx);
        if let Some(t) = self.tabs.get_mut(self.active)
            && let Some(d) = &mut t.detail
        {
            let (open, copied) = d.show(&ctx);
            if let Some(text) = copied {
                ctx.copy_text(text);
                t.status = "Copied".into();
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
                ui.toggle_value(&mut self.show_library, format!("📚 Library ({queries})")).on_hover_text(
                    "Saved queries from packs (*.nerka-pack): pick them for the filter or the search \
                     (Ctrl+Shift+L)",
                );
                if let Some(f) = self.tabs.get(self.active).and_then(|t| t.file.as_ref()) {
                    ui.separator();
                    ui.label(format!("{} lines · {:.1} MiB", f.line_count(), f.size_bytes() as f64 / 1048576.0));
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("ℹ About").on_hover_text("F1").clicked() {
                        self.about = true;
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

        let before = tab.search_prefs();
        let searched =
            egui::Panel::top("filter").show(ui, |ui| tab.filter_bar(ui, &mut focus_search, &self.history)).inner;
        let after = tab.search_prefs();
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
            let close = egui::Panel::right("bookmarks")
                .resizable(true)
                .default_size(300.0)
                .show(ui, |ui| tab.bookmarks_panel(ui, &*file))
                .inner;
            if close {
                self.show_bookmarks = false;
            }
        }
        let search_history = &self.search_history;
        let found = egui::Panel::bottom("search")
            .resizable(true)
            .default_size(260.0)
            .min_size(120.0)
            .show(ui, |ui| tab.search_panel(ui, &*file, search_history))
            .inner;
        egui::CentralPanel::default().show(ui, |ui| tab.main_panel(ui, &*file));
        if after != before {
            self.save_prefs_from(after);
        }
        if let Some((tag, pattern)) = searched {
            self.record_search(tag, &pattern);
        }
        if let Some((tag, pattern)) = found {
            self.record_search_box(tag, &pattern);
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
                origin: "demo.ssl",
                lit: Some((1, true)),
            },
            ChipLine {
                joiner: "OR",
                name: "disk",
                color: Color32::BLUE,
                nfd: "(/disk/)",
                origin: "demo.disk",
                lit: Some((2, false)),
            },
            ChipLine {
                joiner: "AND NOT",
                name: "noise",
                color: Color32::GRAY,
                nfd: "(/nav/)",
                origin: "demo.noise",
                lit: None,
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

    #[test]
    fn boxes_read_their_text_by_mode() {
        let lit = MatcherOptions { fixed_string: true, ..Default::default() };
        let kind = |r: &Result<Filter, String>| match r {
            Ok(Filter::Regex(_)) => "text",
            Ok(Filter::Query(_)) => "query",
            Err(_) => "error",
        };
        let read = |mode, s: &str| {
            let (f, p) = read_box(mode, s, &lit, &[]);
            (kind(&f), p)
        };
        assert_eq!(
            read(Mode::Auto, "connection reset"),
            ("text", Preview::AutoText { text: "connection reset".into(), why: None })
        );
        assert_eq!(read(Mode::Auto, "level >= ERROR"), ("query", Preview::AutoQuery("level >= ERROR".into())));
        let (k, p) = read(Mode::Auto, "level >= LOUD");
        assert!(k == "text" && matches!(p, Preview::AutoText { why: Some(_), .. }), "{p:?}");
        assert_eq!(read(Mode::Auto, ""), ("text", Preview::None));
        // Quoted text or a lone regex is a query that only searches text.
        assert_eq!(
            read(Mode::Auto, "\"ctid = BOOT\""),
            ("query", Preview::AutoText { text: "ctid = BOOT".into(), why: None })
        );
        assert_eq!(read(Mode::Auto, "/SSL|CERT/i"), ("query", Preview::AutoRegex("/SSL|CERT/i".into())));
        // Text mode never reads a query; NFD mode never falls back.
        assert_eq!(read(Mode::Text, "level >= ERROR"), ("text", Preview::None));
        assert_eq!(read(Mode::Query, "level >= ERROR"), ("query", Preview::None));
        let (k, p) = read(Mode::Query, "connection reset");
        assert!(k == "error" && matches!(p, Preview::QueryError(_)), "{p:?}");

        // Invert in Auto wraps a query in NOT; ignore case reaches phrases.
        let file = nerka_core::TextFile::from_bytes("t", LOG.as_bytes().to_vec());
        let run = |mode, s: &str, o: &MatcherOptions| {
            let f = read_box(mode, s, o, &[]).0.unwrap();
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
        // file; the cursor stays on its line.
        tab.pattern.clear();
        tab.start_search(&ctx);
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
}
