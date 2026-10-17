//! nerka-gui — klogg-style desktop viewer: main view on top, filter view
//! below, bookmarks panel on the right.
//!
//! Open with Ctrl+O, drag & drop, or from a shell:
//!     nerka-gui app.log
//!     nerka-gui app.log --query 'level >= ERROR AND raw ~ /SSL/'
//!     nerka-gui app.log --regex 'timeout|refused'
//! Keys: Ctrl+F filter · Enter run · n/N next/prev match · m toggle bookmark
//! · b/B next/prev bookmark · Ctrl+C copy line · Ctrl+Home/End.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod job;
mod view;

use eframe::egui::{self, Key, KeyboardShortcut, Modifiers};
use job::{Job, Poll};
use nerka_core::{
    Bookmarks, Filter, Highlighter, LineNo, LogSource, Matcher, MatcherOptions, Query, SearchResult, TextFile,
};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use view::{Highlight, ViewState, log_view};

fn main() -> eframe::Result {
    let mut initial: Option<PathBuf> = None;
    let mut preset: Option<(Mode, String)> = None;
    let mut args = std::env::args_os().skip(1);
    while let Some(a) = args.next() {
        match a.to_str() {
            Some("--query" | "-q") => preset = args.next().map(|v| (Mode::Query, v.to_string_lossy().into_owned())),
            Some("--regex" | "-r") => preset = args.next().map(|v| (Mode::Regex, v.to_string_lossy().into_owned())),
            _ => initial = Some(PathBuf::from(a)),
        }
    }
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_inner_size([1280.0, 800.0]).with_title("nerka"),
        ..Default::default()
    };
    eframe::run_native(
        "nerka",
        options,
        Box::new(move |cc| {
            let mut app = App::default();
            if let Some((mode, pattern)) = preset {
                app.mode = mode;
                app.pattern = pattern;
            }
            if let Some(p) = initial {
                app.open(&cc.egui_ctx, p);
            }
            Ok(Box::new(app))
        }),
    )
}

#[derive(Default, PartialEq, Clone, Copy)]
enum Mode {
    /// klogg-style regex box.
    #[default]
    Regex,
    /// NFD query: `level >= ERROR AND raw ~ /SSL/`.
    Query,
}

#[derive(Default)]
struct App {
    file: Option<Arc<TextFile>>,
    loading: Option<(PathBuf, Job<TextFile>)>,
    main_view: ViewState,
    filter_view: ViewState,

    mode: Mode,
    pattern: String,
    options: MatcherOptions,
    highlighter: Option<Arc<Highlighter>>,
    result: Option<Arc<SearchResult>>,
    searching: Option<(Arc<Filter>, Job<SearchResult>)>,

    bookmarks: Bookmarks,
    show_bookmarks: bool,

    goto: String,
    status: String,
    focus_search: bool,
}

impl App {
    fn open(&mut self, ctx: &egui::Context, path: PathBuf) {
        if let Some((_, j)) = self.loading.take() {
            j.cancel();
        }
        let p = path.clone();
        self.loading = Some((path, Job::spawn(ctx, move |prog| TextFile::open(&p, prog))));
    }

    fn build_filter(&self) -> Result<Filter, String> {
        match self.mode {
            Mode::Regex => Matcher::new(&self.pattern, self.options.clone()).map(Filter::Regex).map_err(|e| {
                let detail = std::error::Error::source(&e).map(|s| s.to_string()).unwrap_or_default();
                format!("Invalid regex: {}", detail.lines().last().unwrap_or(""))
            }),
            Mode::Query => Query::compile(&self.pattern).map(Filter::Query).map_err(|e| format!("Query error: {e}")),
        }
    }

    fn start_search(&mut self, ctx: &egui::Context) {
        let Some(file) = self.file.clone() else { return };
        if let Some((_, j)) = self.searching.take() {
            j.cancel();
        }
        if self.pattern.trim().is_empty() {
            self.highlighter = None;
            self.result = None;
            self.status.clear();
            return;
        }
        match self.build_filter() {
            Ok(f) => {
                let f = Arc::new(f);
                let ff = f.clone();
                let job = Job::spawn(ctx, move |prog| nerka_core::search(&*file, &ff, prog));
                self.searching = Some((f, job));
            }
            Err(e) => self.status = e,
        }
    }

    fn poll_jobs(&mut self, ctx: &egui::Context) {
        if let Some((path, job)) = &self.loading {
            match job.poll() {
                Poll::Running => ctx.request_repaint_after(Duration::from_millis(50)),
                Poll::Done(r, took) => {
                    match r {
                        Ok(f) => {
                            self.status = format!("Indexed {} lines in {took:.2?}", f.line_count());
                            self.bookmarks = Bookmarks::load_for(path).unwrap_or_else(|e| {
                                self.status = format!("Could not read bookmarks: {e}");
                                Bookmarks::default()
                            });
                            self.file = Some(Arc::new(f));
                            self.main_view = ViewState::default();
                            self.filter_view = ViewState::default();
                            self.result = None;
                            ctx.send_viewport_cmd(egui::ViewportCommand::Title(format!("nerka — {}", path.display())));
                        }
                        Err(nerka_core::Error::Cancelled) => {}
                        Err(e) => {
                            let src = std::error::Error::source(&e).map(|s| format!(" ({s})")).unwrap_or_default();
                            self.status = format!("Cannot open {}: {e}{src}", path.display());
                        }
                    }
                    self.loading = None;
                    if self.file.is_some() && !self.pattern.trim().is_empty() {
                        self.start_search(ctx);
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
        if self.file.is_none() {
            return;
        }
        let line = self.current_line();
        let on = self.bookmarks.toggle(line);
        self.status = format!("Bookmark {} line {}", if on { "added on" } else { "removed from" }, line + 1);
        self.save_bookmarks();
    }

    fn save_bookmarks(&mut self) {
        if let Some(path) = self.file.as_ref().and_then(|f| f.path())
            && let Err(e) = self.bookmarks.save_for(path)
        {
            self.status = format!("Could not save bookmarks: {e}");
        }
    }

    fn handle_input(&mut self, ctx: &egui::Context) {
        let dropped = ctx.input(|i| i.raw.dropped_files.first().map(|f| f.path().to_path_buf()));
        if let Some(p) = dropped.filter(|p| !p.as_os_str().is_empty()) {
            self.open(ctx, p);
        }
        let open = KeyboardShortcut::new(Modifiers::COMMAND, Key::O);
        let find = KeyboardShortcut::new(Modifiers::COMMAND, Key::F);
        if ctx.input_mut(|i| i.consume_shortcut(&open)) {
            self.pick_file(ctx);
        }
        if ctx.input_mut(|i| i.consume_shortcut(&find)) {
            self.focus_search = true;
        }
        // Single-key commands (klogg style) only when not typing in a field.
        if !ctx.egui_wants_keyboard_input() {
            let (shift, n, m, b) =
                ctx.input(|i| (i.modifiers.shift, i.key_pressed(Key::N), i.key_pressed(Key::M), i.key_pressed(Key::B)));
            if n {
                self.jump_match(!shift);
            }
            if m {
                self.toggle_bookmark();
            }
            if b {
                self.jump_bookmark(!shift);
            }
        }
    }

    fn pick_file(&mut self, ctx: &egui::Context) {
        if let Some(p) = rfd::FileDialog::new()
            .add_filter("Logs", &["log", "txt", "out"])
            .add_filter("All files", &["*"])
            .pick_file()
        {
            self.open(ctx, p);
        }
    }

    fn bookmarks_panel(&mut self, ui: &mut egui::Ui, file: &TextFile) {
        ui.heading("Bookmarks");
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
                    let preview: String = file.line_text(line).unwrap_or_default().chars().take(48).collect();
                    ui.add(egui::Label::new(egui::RichText::new(preview).monospace().weak()).truncate());
                });
                ui.horizontal(|ui| {
                    let mut text = note.to_owned();
                    let r = ui.add(
                        egui::TextEdit::singleline(&mut text)
                            .hint_text("note…")
                            .desired_width(ui.available_width() - 28.0),
                    );
                    if r.changed() {
                        edits.push((line, text));
                    }
                    if ui.small_button("✖").on_hover_text("Remove").clicked() {
                        remove = Some(line);
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
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.poll_jobs(&ctx);
        self.handle_input(&ctx);

        egui::Panel::top("toolbar").show(ui, |ui| {
            ui.horizontal(|ui| {
                if ui.button("📂 Open…").on_hover_text("Ctrl+O").clicked() {
                    self.pick_file(&ctx);
                }
                ui.separator();
                ui.label("Go to line:");
                let r = ui.add(egui::TextEdit::singleline(&mut self.goto).desired_width(90.0));
                if r.lost_focus()
                    && ui.input(|i| i.key_pressed(Key::Enter))
                    && let Ok(n) = self.goto.trim().parse::<u64>()
                {
                    self.goto_line(n.saturating_sub(1));
                }
                ui.separator();
                ui.toggle_value(&mut self.show_bookmarks, format!("🔖 Bookmarks ({})", self.bookmarks.len()));
                if let Some(f) = &self.file {
                    ui.separator();
                    ui.label(format!("{} lines · {:.1} MiB", f.line_count(), f.size_bytes() as f64 / 1048576.0));
                }
            });
        });

        egui::Panel::bottom("status").show(ui, |ui| {
            ui.horizontal(|ui| {
                if let Some((p, j)) = &self.loading {
                    ui.add(egui::ProgressBar::new(j.progress.fraction()).desired_width(200.0).show_percentage());
                    ui.label(format!("Indexing {}", p.display()));
                } else if let Some((_, j)) = &self.searching {
                    ui.add(egui::ProgressBar::new(j.progress.fraction()).desired_width(200.0).show_percentage());
                    if ui.button("Cancel").clicked() {
                        j.cancel();
                    }
                } else {
                    ui.label(&self.status);
                }
            });
        });

        let Some(file) = self.file.clone() else {
            egui::CentralPanel::default().show(ui, |ui| {
                ui.centered_and_justified(|ui| {
                    ui.label(if self.loading.is_some() {
                        "Indexing…"
                    } else {
                        "Open a log file (Ctrl+O) or drop it here"
                    });
                });
            });
            return;
        };

        if self.show_bookmarks {
            egui::Panel::right("bookmarks").resizable(true).default_size(300.0).show(ui, |ui| {
                self.bookmarks_panel(ui, &file);
            });
        }

        egui::Panel::bottom("filter").resizable(true).default_size(260.0).min_size(120.0).show(ui, |ui| {
            ui.horizontal(|ui| {
                let mut go = false;
                go |= ui.selectable_value(&mut self.mode, Mode::Regex, "Regex").changed();
                go |= ui
                    .selectable_value(&mut self.mode, Mode::Query, "Query")
                    .on_hover_text("level >= ERROR AND raw ~ /SSL|CERT/i AND NOT /healthcheck/")
                    .changed();
                let hint = match self.mode {
                    Mode::Regex => "regex — Enter to search",
                    Mode::Query => "level >= ERROR AND raw ~ /SSL/ — Enter to search",
                };
                let r = ui.add(
                    egui::TextEdit::singleline(&mut self.pattern)
                        .hint_text(hint)
                        .font(egui::TextStyle::Monospace)
                        .desired_width(ui.available_width() - if self.mode == Mode::Regex { 260.0 } else { 70.0 }),
                );
                if self.focus_search {
                    r.request_focus();
                    self.focus_search = false;
                }
                go |= r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
                if self.mode == Mode::Regex {
                    go |= ui.checkbox(&mut self.options.case_insensitive, "Ignore case").changed();
                    go |= ui.checkbox(&mut self.options.fixed_string, "Literal").changed();
                    go |= ui.checkbox(&mut self.options.invert, "Invert").changed();
                }
                go |= ui.button("Search").clicked();
                if go {
                    self.start_search(&ctx);
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
                    &*file,
                    &Highlight {
                        highlighter: self.highlighter.as_deref(),
                        bookmarks: Some(&self.bookmarks),
                        tint: None,
                    },
                );
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
        });

        egui::CentralPanel::default().show(ui, |ui| {
            log_view(
                ui,
                "main",
                &mut self.main_view,
                file.line_count(),
                &|row| row,
                &*file,
                &Highlight {
                    highlighter: self.highlighter.as_deref(),
                    bookmarks: Some(&self.bookmarks),
                    tint: self.result.as_deref(),
                },
            );
        });
    }
}
