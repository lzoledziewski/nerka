//! nerka-gui — klogg-style desktop viewer: one tab per open file, each with
//! a main view on top, a filter view below and a bookmarks panel on the
//! right. Filter, scroll position, bookmarks and column widths belong to
//! the tab. Plain text and DLT files are recognised by their content; DLT
//! is shown as a table.
//!
//! Open with Ctrl+O, drag & drop, or from a shell:
//!     nerka-gui app.log other.log
//!     nerka-gui app.log --query 'level >= ERROR AND raw ~ /SSL/'
//!     nerka-gui app.log --text 'connection reset'
//!     nerka-gui app.log --regex 'timeout|refused'
//! Keys: Ctrl+F filter · Enter run · n/N next/prev match · Ctrl+G go to line
//! · m toggle bookmark · b/B next/prev bookmark · Ctrl+C copy selection
//! (or line) · Ctrl+A select all · Ctrl+Home/End
//! · Ctrl+Tab / Ctrl+Shift+Tab next/prev tab · Ctrl+W close tab · F1 about.
//!
//! Recent filters are kept in `<config dir>/nerka/history` and offered by
//! the 🕘 button next to the filter box.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod icon;
mod job;
mod prefs;
mod view;

use eframe::egui::{self, Key, KeyboardShortcut, Modifiers};
use job::{Job, Poll};
use nerka_core::{
    Bookmarks, Filter, Highlighter, LineNo, LogSource, Matcher, MatcherOptions, Query, SearchHistory, SearchResult,
};
use prefs::SearchPrefs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use view::{Columns, Highlight, ViewOutput, ViewState, log_view};

fn main() -> eframe::Result {
    let mut initial: Vec<PathBuf> = Vec::new();
    // (history tag, pattern): the same tags the search history uses.
    let mut preset: Option<(&str, String)> = None;
    let mut args = std::env::args_os().skip(1);
    while let Some(a) = args.next() {
        let tag = match a.to_str() {
            Some("--query" | "-q") => "query",
            Some("--regex" | "-r") => "regex",
            Some("--text" | "-t") => "text",
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
            let mut app = App::default();
            app.load_history();
            app.load_prefs();
            for p in initial {
                app.open(&cc.egui_ctx, p);
                // A filter given on the command line applies to every file it names.
                if let (Some((tag, pattern)), Some(tab)) = (&preset, app.tabs.last_mut()) {
                    tab.set_filter(tag, pattern);
                }
            }
            app.active = 0;
            Ok(Box::new(app))
        }),
    )
}

#[derive(Debug, Default, PartialEq, Clone, Copy)]
enum Mode {
    /// klogg-style search box: plain text, or a regex when "Regex" is ticked.
    #[default]
    Text,
    /// NFD query: `level >= ERROR AND raw ~ /SSL/`.
    Query,
}

/// One open file with its own filter, view positions and bookmarks.
struct Tab {
    path: PathBuf,
    file: Option<Arc<dyn LogSource>>,
    loading: Option<Job<Box<dyn LogSource>>>,
    main_view: ViewState,
    filter_view: ViewState,
    columns: Columns,

    mode: Mode,
    pattern: String,
    options: MatcherOptions,
    highlighter: Option<Arc<Highlighter>>,
    result: Option<Arc<SearchResult>>,
    searching: Option<(Arc<Filter>, Job<SearchResult>)>,

    bookmarks: Bookmarks,
    status: String,
}

impl Tab {
    fn open(ctx: &egui::Context, path: PathBuf, prefs: &SearchPrefs) -> Self {
        let p = path.clone();
        Self {
            path,
            file: None,
            loading: Some(Job::spawn(ctx, move |prog| nerka_core::open(&p, prog))),
            main_view: ViewState::default(),
            filter_view: ViewState::default(),
            columns: Columns::default(),
            mode: prefs.mode,
            pattern: String::new(),
            options: prefs.options.clone(),
            highlighter: None,
            result: None,
            searching: None,
            bookmarks: Bookmarks::default(),
            status: String::new(),
        }
    }

    fn title(&self) -> String {
        self.path.file_name().map_or_else(|| self.path.display().to_string(), |n| n.to_string_lossy().into_owned())
    }

    fn is_busy(&self) -> bool {
        self.loading.is_some() || self.searching.is_some()
    }

    fn cancel_jobs(&mut self) {
        if let Some(j) = self.loading.take() {
            j.cancel();
        }
        if let Some((_, j)) = self.searching.take() {
            j.cancel();
        }
    }

    fn search_prefs(&self) -> SearchPrefs {
        SearchPrefs { mode: self.mode, options: self.options.clone() }
    }

    /// History tag for the current filter: `text`, `regex` or `query`.
    fn filter_tag(&self) -> &'static str {
        match self.mode {
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
            _ => return false,
        }
        self.pattern = pattern.to_owned();
        true
    }

    fn build_filter(&self) -> Result<Filter, String> {
        match self.mode {
            Mode::Text => Matcher::new(&self.pattern, self.options.clone()).map(Filter::Regex).map_err(|e| {
                let detail = std::error::Error::source(&e).map(|s| s.to_string()).unwrap_or_default();
                format!("Invalid regex: {}", detail.lines().last().unwrap_or(""))
            }),
            Mode::Query => {
                let schema = self.file.as_ref().map(|f| f.schema()).unwrap_or_default();
                Query::compile_for(&self.pattern, schema).map(Filter::Query).map_err(|e| format!("Query error: {e}"))
            }
        }
    }

    /// Start searching with the current filter; returns whether a search
    /// was started (so the caller can record it in the history).
    fn start_search(&mut self, ctx: &egui::Context) -> bool {
        let Some(file) = self.file.clone() else { return false };
        if let Some((_, j)) = self.searching.take() {
            j.cancel();
        }
        if self.pattern.trim().is_empty() {
            self.highlighter = None;
            self.result = None;
            self.status.clear();
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
        if let Some((f, job)) = &self.searching {
            match job.poll() {
                Poll::Running => ctx.request_repaint_after(Duration::from_millis(50)),
                Poll::Done(r, took) => {
                    match r {
                        Ok(res) => {
                            self.status = format!("{} matching lines in {took:.2?}", res.lines.len());
                            self.highlighter = Some(Arc::new(f.highlighter()));
                            self.result = Some(Arc::new(res));
                            self.filter_view = ViewState::default();
                        }
                        Err(nerka_core::Error::Cancelled) => {}
                        Err(e) => self.status = format!("Search failed: {e}"),
                    }
                    self.searching = None;
                }
            }
        }
        true
    }

    fn total_lines(&self) -> LineNo {
        self.file.as_ref().map_or(0, |f| f.line_count())
    }

    fn current_line(&self) -> LineNo {
        self.main_view.selected.unwrap_or(self.main_view.top)
    }

    fn goto_line(&mut self, line: LineNo) {
        let n = self.total_lines();
        self.main_view.center_on(line.min(n.saturating_sub(1)), n);
    }

    /// Jump the main view to the next/previous match after the selection.
    fn jump_match(&mut self, forward: bool) {
        let Some(res) = self.result.clone() else { return };
        if res.lines.is_empty() {
            return;
        }
        let cur = self.current_line();
        let i = res.position_at_or_after(if forward { cur + 1 } else { cur });
        let i = if forward { i.min(res.lines.len() - 1) } else { i.saturating_sub(1) };
        self.goto_line(res.lines[i]);
        self.filter_view.select(i as u64, res.lines.len() as u64);
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
        egui::ScrollArea::vertical().show(ui, |ui| {
            for (line, note) in self.bookmarks.iter() {
                ui.horizontal(|ui| {
                    if ui.link(format!("{}", line + 1)).clicked() {
                        jump = Some(line);
                    }
                    let preview: String = file
                        .line_text(line)
                        .unwrap_or_default()
                        .chars()
                        .map(|c| if c == '\t' { ' ' } else { c })
                        .take(48)
                        .collect();
                    ui.add(egui::Label::new(egui::RichText::new(preview).monospace().weak()).truncate());
                });
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

    /// Returns the filter that was searched for, if a search started.
    fn filter_panel(
        &mut self,
        ui: &mut egui::Ui,
        file: &dyn LogSource,
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
                .on_hover_text("Recent filters");
            });
            // Mode and checkboxes only change how the box is read; only
            // Enter or Search runs it.
            ui.selectable_value(&mut self.mode, Mode::Text, "Text");
            ui.selectable_value(&mut self.mode, Mode::Query, "Query")
                .on_hover_text("level >= ERROR AND raw ~ /SSL|CERT/i AND NOT /healthcheck/");
            let hint = match self.mode {
                Mode::Text if self.options.fixed_string => "text — Enter to search",
                Mode::Text => "regex — Enter to search",
                Mode::Query => "level >= ERROR AND raw ~ /SSL/ — Enter to search",
            };
            // Right to left: buttons first, the text box takes what is left,
            // so the row never grows wider than the panel.
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                go |= ui.button("Search").clicked();
                if self.mode == Mode::Text {
                    ui.checkbox(&mut self.options.invert, "Invert");
                    ui.checkbox(&mut self.options.case_insensitive, "Ignore case");
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
                go |= r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
            });
            if go && self.start_search(ui.ctx()) {
                searched = Some((self.filter_tag(), self.pattern.clone()));
            }
        });
        ui.add_space(2.0);
        if let Some(res) = self.result.clone() {
            let out = log_view(
                ui,
                "filter",
                &mut self.filter_view,
                res.lines.len() as u64,
                &|row| res.lines[row as usize],
                file,
                &mut self.columns,
                &Highlight { highlighter: self.highlighter.as_deref(), bookmarks: Some(&self.bookmarks) },
            );
            self.view_output(&out, |row| res.lines[row as usize]);
            // Selecting in the filter view moves the main view.
            let picked = out.clicked.or(if out.has_focus { self.filter_view.selected } else { None });
            if let Some(row) = picked {
                let line = res.lines[row as usize];
                if self.main_view.selected != Some(line) {
                    self.goto_line(line);
                }
            }
        } else {
            ui.weak("No filter applied.");
            // Fill the pane: panels remember their content size.
            ui.allocate_space(ui.available_size());
        }
        searched
    }

    fn main_panel(&mut self, ui: &mut egui::Ui, file: &dyn LogSource) {
        let out = log_view(
            ui,
            "main",
            &mut self.main_view,
            file.line_count(),
            &|row| row,
            file,
            &mut self.columns,
            &Highlight { highlighter: self.highlighter.as_deref(), bookmarks: Some(&self.bookmarks) },
        );
        self.view_output(&out, |row| row);
    }
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
    /// Search mode and options for new tabs: the last ones the user picked.
    prefs: SearchPrefs,
    /// Shown when no tab is open (e.g. why the last open failed).
    status: String,
    focus_search: bool,
    window_title: String,
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

/// Per-user config directory for nerka, if one can be determined.
fn config_dir() -> Option<PathBuf> {
    let base = if cfg!(windows) {
        std::env::var_os("APPDATA").map(PathBuf::from)
    } else {
        std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
    };
    base.map(|b| b.join("nerka"))
}

fn history_path() -> Option<PathBuf> {
    config_dir().map(|d| d.join("history"))
}

fn prefs_path() -> Option<PathBuf> {
    config_dir().map(|d| d.join("prefs"))
}

impl App {
    fn load_history(&mut self) {
        let Some(path) = history_path() else { return };
        match SearchHistory::load(&path) {
            Ok(h) => self.history = h,
            Err(e) => self.status = format!("Could not read search history: {e}"),
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

    fn record_search(&mut self, tag: &str, pattern: &str) {
        if self.history.push(tag, pattern)
            && let Some(path) = history_path()
            && let Err(e) = self.history.save(&path)
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
            .add_filter("Logs", &["log", "txt", "out"])
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
            ui.label(format!("Go to line (1 – {lines}):"));
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
        let before = tab.search_prefs();
        let searched = egui::Panel::bottom("filter")
            .resizable(true)
            .default_size(260.0)
            .min_size(120.0)
            .show(ui, |ui| tab.filter_panel(ui, &*file, &mut focus_search, &self.history))
            .inner;
        let after = tab.search_prefs();
        egui::CentralPanel::default().show(ui, |ui| tab.main_panel(ui, &*file));
        if after != before {
            self.save_prefs_from(after);
        }
        if let Some((tag, pattern)) = searched {
            self.record_search(tag, &pattern);
        }
    }
}
