//! Virtualized log view.
//!
//! egui's `ScrollArea` positions rows in `f32` pixels, which loses precision
//! beyond ~10M rows. This view keeps its scroll position as a `u64` row
//! index instead, paints only the visible rows, and draws its own
//! scrollbar, so it behaves the same on a 1 KB file and a 100 M-line file.
//!
//! Text selection: drag, Shift+click, double-click (word), Shift+arrows
//! (whole lines), Ctrl+A. Ctrl+C copies the selection, or the current line
//! when nothing is selected.
//!
//! Picked lines (issue #12): Ctrl+click (or Ctrl+Space on the cursor line)
//! picks or unpicks single lines anywhere; Ctrl+C then copies the picked
//! lines whole, in file order. A plain click or Esc clears them. Picks are
//! file lines, so they stay when the filter changes.
//!
//! Long lines scroll sideways: the horizontal scrollbar (shown when the
//! visible lines are wider than the view), Shift+wheel or ← / →. Enter or
//! the context menu asks for the whole current line (`ViewOutput::detail`).
//!
//! Sources with columns (DLT) are shown as a table: each line's
//! tab-separated cells are aligned under a header row whose dividers can be
//! dragged to resize columns. The line text stays the unit of selection,
//! search highlighting and copy (copied cells keep their tabs, so they
//! paste into a spreadsheet as columns).

use eframe::egui::{
    self, Align2, Color32, CursorIcon, FontId, Key, Rect, Sense, Stroke, StrokeKind, Ui, pos2,
    text::{LayoutJob, TextFormat},
    vec2,
};
use nerka_core::{Bookmarks, ColumnDef, Highlighter, LineNo, LogSource, SearchHits};
use std::borrow::Cow;
use std::ops::Range;

/// Lines longer than this are cut for display (search still sees them).
const MAX_DISPLAY_BYTES: usize = 4096;
/// An unfolded record shows this much of itself at most, in this many lines.
const MAX_UNFOLD_BYTES: usize = 64 * 1024;
const MAX_UNFOLD_LINES: usize = 500;
/// A folded record shows this much of its other lines, joined, after its first.
const FOLDED_PREVIEW_CHARS: usize = 400;
/// Copying more lines than this is cut short (the clipboard is not a file).
const MAX_COPY_LINES: u64 = 100_000;
const SCROLLBAR_W: f32 = 14.0;
const TAB_COLS: usize = 4;
/// Rows scrolled per mouse-wheel notch (klogg uses 3).
const LINES_PER_NOTCH: u64 = 3;
/// The stripe of a bookmarked line (and of its note).
const BOOKMARK_COLOR: Color32 = Color32::from_rgb(230, 120, 40);

/// A position in the view: row and display column (see [`Cells`]).
/// `col == usize::MAX` means "end of line".
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub struct TextPos {
    pub row: u64,
    pub col: usize,
}

#[derive(Default)]
pub struct ViewState {
    /// First visible row.
    pub top: u64,
    /// Horizontal scroll in pixels.
    pub x_off: f32,
    /// Cursor row (index into the view's rows, not a file line).
    pub selected: Option<u64>,
    /// Number of rows that fit in the last frame.
    pub page: u64,
    /// Text selection as (anchor, head); empty when both are equal.
    pub sel: Option<(TextPos, TextPos)>,
    /// Lines picked one by one (Ctrl+click), as file lines.
    pub picked: std::collections::BTreeSet<LineNo>,
    /// Takes ↑ / ↓ / Page Up / Page Down (and the focus) when no text box
    /// or other log view has the focus — after a click on a button, or
    /// before any click (the main view).
    pub free_keys: bool,
    /// Records of several lines shown whole, one row per line, as file
    /// lines; the others show their first line and `▸ +N lines`.
    pub unfolded: std::collections::BTreeSet<LineNo>,
    /// Every record of several lines shown whole.
    pub unfold_all: bool,
    /// Slots of the top row scrolled past (an unfolded record's lines, a
    /// note), as (that row, how many): the wheel scrolls through a record.
    /// Moving `top` any other way leaves it for another row: none skipped.
    skip: (u64, usize),
    dragging: bool,
    /// Fraction of a row scrolled but not applied yet (trackpads).
    scroll_acc: f32,
    /// Table column whose right divider is being dragged.
    resizing: Option<usize>,
    /// Width of the widest line painted in the last frame, in pixels.
    content_w: f32,
}

/// Column layout of a table view. A tab's views share one, so they stay
/// aligned. Empty for plain text.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Columns {
    pub names: Vec<Cow<'static, str>>,
    /// Cell widths in characters, including a one-character gap. The last
    /// column is unbounded; its entry is unused.
    pub widths: Vec<usize>,
}

/// The header of the bookmark-note column ([`NoteMode::Column`]).
const NOTE_COLUMN: &str = "🔖 note";

/// Where bookmark notes show in the views (Bookmarks panel, saved in prefs).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum NoteMode {
    Off,
    /// On a row of its own above the line (never scrolled sideways).
    #[default]
    LineBefore,
    /// Dimmed after the line's text.
    EndOfLine,
    /// A last column of tables; plain text shows it on the line before.
    Column,
}

impl NoteMode {
    pub const ALL: [NoteMode; 4] = [NoteMode::Off, NoteMode::LineBefore, NoteMode::EndOfLine, NoteMode::Column];

    pub fn key(self) -> &'static str {
        match self {
            NoteMode::Off => "off",
            NoteMode::LineBefore => "before",
            NoteMode::EndOfLine => "end",
            NoteMode::Column => "column",
        }
    }

    pub fn parse(s: &str) -> Option<NoteMode> {
        NoteMode::ALL.into_iter().find(|m| m.key() == s)
    }

    pub fn label(self) -> &'static str {
        match self {
            NoteMode::Off => "Off",
            NoteMode::LineBefore => "Line before",
            NoteMode::EndOfLine => "End of line",
            NoteMode::Column => "Column",
        }
    }
}

/// A row of the view on screen: a note above a line, or a line of a row (a
/// record of several lines unfolded takes one per line).
#[derive(Debug, Clone, PartialEq)]
enum Slot {
    Note(String),
    Line(usize),
}

/// A bookmark's name as the views show it: `?` after an unsure one's.
fn shown_name(b: &Bookmarks, line: LineNo) -> Option<String> {
    let name = b.name(line)?;
    Some(if b.is_unsure(line) { format!("{name}?") } else { name.to_owned() })
}

/// A bookmark's note as the views show it (`name · note`); none without a note.
fn shown_note(b: &Bookmarks, line: LineNo) -> Option<String> {
    let note = b.note(line).filter(|n| !n.is_empty())?;
    Some(format!("{} · {note}", shown_name(b, line)?))
}

impl Columns {
    /// Add or remove the last column for bookmark notes (tables only). The
    /// column before it, unbounded until then, gets a width of its own.
    pub fn note_column(&mut self, on: bool) {
        let has = self.names.last().is_some_and(|n| n == NOTE_COLUMN);
        if on && !has && !self.names.is_empty() {
            if let Some(w) = self.widths.last_mut() {
                *w = (*w).max(60);
            }
            self.names.push(Cow::Borrowed(NOTE_COLUMN));
            self.widths.push(NOTE_COLUMN.chars().count() + 1);
        } else if !on && has {
            self.names.pop();
            self.widths.pop();
        }
    }

    pub fn from_defs(defs: &[ColumnDef]) -> Self {
        Columns {
            names: defs.iter().map(|d| d.name.clone()).collect(),
            widths: defs.iter().map(|d| (d.width as usize).max(d.name.len()) + 1).collect(),
        }
    }

    fn cells(&self) -> Cells<'_> {
        Cells { widths: &self.widths }
    }

    /// First display column of each cell.
    fn starts(&self) -> Vec<usize> {
        let mut at = 0;
        self.widths
            .iter()
            .map(|w| {
                let s = at;
                at += w;
                s
            })
            .collect()
    }
}

impl ViewState {
    /// A view that takes the keys nothing else uses ([`Self::free_keys`]).
    pub fn with_free_keys() -> Self {
        Self { free_keys: true, ..Default::default() }
    }

    pub fn select(&mut self, row: u64, rows: u64) {
        let row = row.min(rows.saturating_sub(1));
        self.selected = Some(row);
        self.sel = None;
        self.scroll_to(row);
    }

    /// Select `row` and put it in the middle of the view.
    pub fn center_on(&mut self, row: u64, rows: u64) {
        self.selected = Some(row);
        self.sel = None;
        self.top = row.saturating_sub(self.page / 2).min(rows.saturating_sub(self.page.max(1)));
    }

    fn scroll_to(&mut self, row: u64) {
        let page = self.page.max(1);
        if row < self.top {
            self.top = row;
        } else if row >= self.top + page {
            self.top = row + 1 - page;
        }
    }

    /// Slots of the top row scrolled past.
    fn skipped(&self) -> usize {
        if self.skip.0 == self.top { self.skip.1 } else { 0 }
    }

    /// Selection ordered as (start, end), if not empty.
    pub fn selection(&self) -> Option<(TextPos, TextPos)> {
        let (a, h) = self.sel?;
        (a != h).then(|| if a <= h { (a, h) } else { (h, a) })
    }
}

pub struct Highlight<'a> {
    pub highlighter: Option<&'a Highlighter>,
    pub bookmarks: Option<&'a Bookmarks>,
    pub search: Option<&'a SearchMarks<'a>>,
    /// Whole-line colours: the last matching query (in pick order) wins.
    pub lines: Option<&'a SearchMarks<'a>>,
    /// Where the bookmarks' notes show.
    pub notes: NoteMode,
    /// Where lifecycles start (file lines, ascending): a line above the row,
    /// labelled, red after one that ended badly (`doc/lifecycles.md`).
    pub lifecycles: Option<&'a [Boundary]>,
}

/// A lifecycle's start, as the view marks it.
pub struct Boundary {
    pub line: LineNo,
    /// `boot ECU1#2`.
    pub label: String,
    /// The lifecycle before it (of its key) ended badly.
    pub bad: bool,
}

/// Search hits to mark, coloured per search query.
pub struct SearchMarks<'a> {
    pub hits: &'a SearchHits,
    /// Per include query of the search: its colour and what to highlight.
    pub colors: &'a [Color32],
    pub highlighters: &'a [Highlighter],
}

impl SearchMarks<'_> {
    /// Include queries that matched `line`, if it is a hit.
    fn matched(&self, line: LineNo) -> Option<Vec<usize>> {
        let i = self.hits.lines.binary_search(&line).ok()?;
        Some(self.hits.matched(i).collect())
    }
}

/// Background for search hits of a query colour.
fn hit_bg(c: Color32, dark: bool) -> Color32 {
    Color32::from_rgba_unmultiplied(c.r(), c.g(), c.b(), if dark { 110 } else { 150 })
}

#[derive(Default)]
pub struct ViewOutput {
    pub clicked: Option<u64>,
    /// The view has the keyboard focus (the tests check it keeps it).
    #[cfg_attr(not(test), allow(dead_code))]
    pub has_focus: bool,
    /// Lines copied to the clipboard and whether the copy was cut short.
    pub copied: Option<(u64, bool)>,
    /// "Toggle bookmark" chosen from the context menu, for this row.
    pub toggle_bookmark: Option<u64>,
    /// The whole line was asked for (Enter or the context menu), for this row.
    pub detail: Option<u64>,
    /// The selection moved to this row in this frame (a click, the keys).
    pub moved_to: Option<u64>,
}

/// Draw `rows` rows; `line_of_row` maps a view row to a file line.
#[allow(clippy::too_many_arguments)]
pub fn log_view(
    ui: &mut Ui,
    id: &str,
    state: &mut ViewState,
    rows: u64,
    line_of_row: &dyn Fn(u64) -> LineNo,
    source: &dyn LogSource,
    columns: &mut Columns,
    hl: &Highlight<'_>,
) -> ViewOutput {
    let selected_before = state.selected;
    let table = !columns.names.is_empty();
    let font = FontId::monospace(13.0);
    // Whole physical pixels per row and per text origin keep glyphs crisp.
    let ppp = ui.ctx().pixels_per_point();
    let snap = |v: f32| (v * ppp).round() / ppp;
    let row_h = snap(ui.fonts_mut(|f| f.row_height(&font)) + 2.0);
    let char_w = ui.fonts_mut(|f| f.glyph_width(&font, '0'));
    let total_lines = source.line_count();
    let gutter_w = (digits(total_lines) as f32 + 1.5) * char_w;

    let (outer, response) = ui.allocate_exact_size(ui.available_size(), Sense::click_and_drag());
    let id = ui.scope_id().with(id);
    // A horizontal scrollbar while the lines painted last frame were wider
    // than the text area.
    let clip_w = (outer.width() - SCROLLBAR_W - gutter_w).max(0.0);
    let max_x = |content_w: f32| (content_w - clip_w + char_w * 2.0).max(0.0);
    let hbar_h = if max_x(state.content_w) > 0.0 { SCROLLBAR_W } else { 0.0 };
    let text_rect = Rect::from_min_max(outer.min, pos2(outer.max.x - SCROLLBAR_W, outer.max.y - hbar_h));
    let bar_rect = Rect::from_min_max(pos2(text_rect.max.x, outer.min.y), pos2(outer.max.x, text_rect.max.y));
    let hbar_rect =
        Rect::from_min_max(pos2(text_rect.min.x + gutter_w, text_rect.max.y), pos2(text_rect.max.x, outer.max.y));
    // Rows go below the table header, if there is one.
    let rows_rect =
        Rect::from_min_max(pos2(text_rect.min.x, text_rect.min.y + if table { row_h } else { 0.0 }), text_rect.max);
    let text_clip = Rect::from_min_max(pos2(text_rect.min.x + gutter_w, rows_rect.min.y), text_rect.max);
    let header_rect =
        Rect::from_min_max(pos2(text_clip.min.x, text_rect.min.y), pos2(text_rect.max.x, rows_rect.min.y));
    let text_x0 = text_clip.min.x + char_w * 0.5;

    // Notes on rows of their own ([`NoteMode::LineBefore`]; a text log's
    // Column too) take a slot above their line. `slots(top, until)`: the
    // rows (and note slots) that fit from `top`, with their y; a note slot
    // belongs to the row after it, so a click there picks that line.
    let note_column = table && columns.names.last().is_some_and(|n| n == NOTE_COLUMN);
    let note_rows = match hl.notes {
        NoteMode::LineBefore => hl.bookmarks,
        NoteMode::Column if !note_column => hl.bookmarks,
        _ => None,
    };
    // A record of several lines unfolded takes a slot per line (`Line(k)`);
    // folded, or one line, a slot. `open` says which rows are unfolded.
    let line_count = |row: u64| {
        let bytes = source.line_bytes(line_of_row(row)).unwrap_or_default();
        let head = &bytes[..bytes.len().min(MAX_UNFOLD_BYTES)];
        (head.iter().filter(|&&b| b == b'\n').count() + 1).min(MAX_UNFOLD_LINES)
    };
    // A row's slots: its note (if shown) and its lines.
    let row_slots = |row: u64, open: &dyn Fn(u64) -> bool| -> (Option<String>, usize) {
        let note = note_rows.filter(|_| row < rows).and_then(|b| shown_note(b, line_of_row(row)));
        (note, if row < rows && open(row) { line_count(row) } else { 1 })
    };
    // `skip`: slots of the top row scrolled past (one is always shown).
    let slots = |top: u64, skip: usize, until: u64, open: &dyn Fn(u64) -> bool| -> Vec<(f32, u64, Slot)> {
        let mut out = Vec::new();
        let (mut y, mut row) = (snap(rows_rect.top()), top);
        'rows: while y < rows_rect.bottom() && row < until {
            let (note, n) = row_slots(row, open);
            let has_note = usize::from(note.is_some());
            let skip = if row == top { skip.min(has_note + n - 1) } else { 0 };
            let row_slots = note.map(Slot::Note).into_iter().chain((0..n).map(Slot::Line)).skip(skip);
            for slot in row_slots {
                out.push((y, row, slot));
                y += row_h;
                if y >= rows_rect.bottom() {
                    break 'rows;
                }
            }
            row += 1;
        }
        out
    };
    let is_open = |state: &ViewState| {
        let (all, set) = (state.unfold_all, state.unfolded.clone());
        move |row: u64| all || set.contains(&line_of_row(row))
    };
    // Rows that fit whole, counted as if the file went on past its end.
    let fits = |top: u64, skip: usize, open: &dyn Fn(u64) -> bool| {
        let slots = slots(top, skip, u64::MAX, open);
        let last_whole =
            |r: u64| slots.iter().filter(|(_, row, _)| *row == r).all(|(y, ..)| y + row_h <= rows_rect.bottom());
        let mut whole: Vec<u64> = slots.iter().map(|(_, r, _)| *r).collect();
        whole.dedup();
        whole.into_iter().filter(|&r| last_whole(r)).count()
    };
    state.page = (fits(state.top, state.skipped(), &is_open(state)) as u64).max(1);
    // How far down the view scrolls: (top row, its slots skipped) with the
    // last row's last slot at the bottom. Not `rows - page`: a long last
    // record then fit "whole" only with rows past the end below it, and the
    // view jumped back each time it scrolled into it.
    let end = {
        let open = is_open(state);
        let mut left = (((rows_rect.bottom() - snap(rows_rect.top())) / row_h).floor() as usize).max(1);
        let mut end = (0, 0);
        for row in (0..rows).rev() {
            let (note, n) = row_slots(row, &open);
            let count = usize::from(note.is_some()) + n;
            if count >= left {
                end = (row, count - left);
                break;
            }
            left -= count;
        }
        end
    };
    let max_top = end.0;
    let line_str = |row: u64| source.line_text(line_of_row(row)).unwrap_or_default();

    // ----- input -----
    if response.clicked() || response.drag_started() || response.secondary_clicked() {
        response.request_focus();
    }
    let mut has_focus = response.has_focus();
    // Every log view's id, so a view knows when another one has the focus.
    let views_id = egui::Id::unique("nerka-log-views");
    ui.data_mut(|d| d.get_temp_mut_or_default::<std::collections::HashSet<egui::Id>>(views_id).insert(response.id));
    if state.free_keys && !has_focus && rows > 0 {
        let focused = ui.memory(|m| m.focused());
        let views = ui.data(|d| d.get_temp::<std::collections::HashSet<egui::Id>>(views_id)).unwrap_or_default();
        let free = focused.is_none_or(|f| !ui.ctx().text_edit_focused() && !views.contains(&f));
        let nav = [Key::ArrowUp, Key::ArrowDown, Key::PageUp, Key::PageDown];
        if free && ui.input(|i| !i.modifiers.any() && nav.iter().any(|&k| i.key_pressed(k))) {
            response.request_focus();
            has_focus = true;
        }
    }
    if has_focus {
        // The view's keys: without this, egui also moves the focus to the
        // nearest widget in an arrow's direction (or drops it on Esc), and
        // the next arrow goes through buttons instead of the log.
        let keys = egui::EventFilter { horizontal_arrows: true, vertical_arrows: true, escape: true, tab: false };
        ui.memory_mut(|m| m.set_focus_lock_filter(response.id, keys));
        // The filter counts from the frame after the view got the focus;
        // an arrow in that first frame would still move it.
        let arrow = [Key::ArrowUp, Key::ArrowDown, Key::ArrowLeft, Key::ArrowRight];
        if ui.input(|i| arrow.iter().any(|&k| i.key_pressed(k))) {
            ui.memory_mut(|m| m.move_focus(egui::FocusDirection::None));
        }
    }
    if response.hovered() {
        // Raw wheel events, not egui's smoothed delta: a notch moves a fixed
        // number of whole rows at once, so text never sits between rows.
        let (rows_delta, x_delta) = ui.input_mut(|i| {
            i.smooth_scroll_delta = egui::Vec2::ZERO;
            let (mut dy, mut dx) = (0.0f32, 0.0f32);
            for e in &i.events {
                let egui::Event::MouseWheel { unit, delta, modifiers, .. } = e else { continue };
                if modifiers.command {
                    continue; // Ctrl+wheel is zoom
                }
                // Shift+wheel scrolls sideways.
                let d = if modifiers.shift && delta.x == 0.0 { egui::vec2(delta.y, 0.0) } else { *delta };
                match unit {
                    egui::MouseWheelUnit::Line => {
                        dy -= d.y * LINES_PER_NOTCH as f32;
                        dx -= d.x * char_w * 8.0;
                    }
                    egui::MouseWheelUnit::Point => {
                        dy -= d.y / row_h;
                        dx -= d.x;
                    }
                    egui::MouseWheelUnit::Page => {
                        dy -= d.y * state.page as f32;
                        dx -= d.x * text_clip.width();
                    }
                }
            }
            (dy, dx)
        });
        // Trackpads send small pixel deltas: keep the remainder for later.
        state.scroll_acc += rows_delta;
        let whole = state.scroll_acc.trunc();
        state.scroll_acc -= whole;
        // A slot at a time: through an unfolded record's lines, not past it.
        let open = is_open(state);
        let count = |row: u64| {
            let (note, n) = row_slots(row, &open);
            usize::from(note.is_some()) + n
        };
        for _ in 0..(whole.abs() as u64).min(10 * rows_rect.height() as u64) {
            let skip = state.skipped();
            if whole > 0.0 {
                if (state.top, skip) >= end {
                    break;
                } else if skip + 1 < count(state.top) {
                    state.skip = (state.top, skip + 1);
                } else {
                    state.top += 1;
                }
            } else if skip > 0 {
                state.skip = (state.top, skip - 1);
            } else if state.top > 0 {
                state.top -= 1;
                state.skip = (state.top, count(state.top) - 1);
            } else {
                break;
            }
        }
        state.x_off = (state.x_off + x_delta).max(0.0);
    }

    // Mouse → text position (row clamped to the rows that exist).
    // The columns as they are now (the header may resize them below).
    let widths_now = columns.widths.clone();
    let cells_now = Cells { widths: &widths_now };
    let hit = |state: &ViewState, p: egui::Pos2| -> TextPos {
        let col = ((p.x - text_x0 + state.x_off) / char_w).round().max(0.0) as usize;
        let simple = note_rows.is_none() && !state.unfold_all && state.unfolded.is_empty();
        let (row, k) = if p.y < rows_rect.top() {
            (state.top.saturating_sub(1), 0)
        } else if simple {
            (state.top + ((p.y - rows_rect.top()) / row_h).floor() as u64, 0)
        } else {
            match slots(state.top, state.skipped(), rows, &is_open(state)).iter().rev().find(|(y, ..)| *y <= p.y) {
                Some((y, row, slot)) if p.y < y + row_h => (*row, if let Slot::Line(k) = slot { *k } else { 0 }),
                Some((_, row, _)) => (row + 1, 0),
                None => (state.top, 0),
            }
        };
        let row = row.min(rows.saturating_sub(1));
        // The columns before the record's k-th line.
        let before = if k == 0 {
            0
        } else {
            let text = line_str(row);
            let start = text.match_indices('\n').nth(k - 1).map_or(text.len(), |(i, _)| i + 1);
            cells_now.width(&text[..start])
        };
        TextPos { row, col: before + col }
    };
    let mut clicked = None;
    let (pressed, released, shift, command, pointer) = ui.input(|i| {
        (
            i.pointer.primary_pressed(),
            i.pointer.primary_released(),
            i.modifiers.shift,
            i.modifiers.command,
            i.pointer.interact_pos(),
        )
    });
    // ----- table header: drag a divider to resize its column -----
    let x0 = snap(text_x0 - state.x_off);
    let divider_x =
        |starts: &[usize], widths: &[usize], k: usize| x0 + (starts[k] + widths[k]) as f32 * char_w - char_w * 0.5;
    let mut over_divider = false;
    if table {
        let starts = columns.starts();
        let n = columns.widths.len().saturating_sub(1);
        let near = pointer
            .filter(|p| header_rect.contains(*p) && response.contains_pointer())
            .and_then(|p| (0..n).find(|&k| (p.x - divider_x(&starts, &columns.widths, k)).abs() <= 4.0));
        over_divider = near.is_some();
        if pressed && near.is_some() {
            state.resizing = near;
        }
        if let Some(k) = state.resizing {
            if !ui.input(|i| i.pointer.primary_down()) {
                state.resizing = None;
            } else if let Some(p) = pointer {
                let cell_x = x0 + starts[k] as f32 * char_w;
                columns.widths[k] = ((p.x - cell_x) / char_w + 0.5).round().max(2.0) as usize;
            }
        }
    }

    let columns = &*columns;
    let cells = columns.cells();

    if rows > 0
        && pressed
        && let Some(p) = pointer
        && rows_rect.contains(p)
        && response.contains_pointer()
    {
        let pos = hit(state, p);
        if command && !shift {
            // Pick or unpick this line; no text selection.
            let line = line_of_row(pos.row);
            if !state.picked.remove(&line) {
                state.picked.insert(line);
            }
            state.sel = None;
            state.selected = Some(pos.row);
            clicked = Some(pos.row);
        } else {
            if !shift {
                state.picked.clear();
            }
            select_from(state, shift, pos);
            clicked = Some(pos.row);
        }
    }
    fn select_from(state: &mut ViewState, shift: bool, pos: TextPos) {
        let anchor = match (shift, state.sel, state.selected) {
            (true, Some((a, _)), _) => a,
            (true, None, Some(r)) => TextPos { row: r, col: 0 },
            _ => pos,
        };
        state.sel = Some((anchor, pos));
        state.selected = Some(pos.row);
        state.dragging = true;
    }
    if state.dragging {
        if released || !ui.input(|i| i.pointer.primary_down()) {
            state.dragging = false;
        } else if let Some(p) = pointer {
            // Auto-scroll while dragging past the edges.
            if p.y < rows_rect.top() {
                state.top = state.top.saturating_sub(1);
                ui.ctx().request_repaint();
            } else if p.y > rows_rect.bottom() {
                state.top = (state.top + 1).min(max_top);
                ui.ctx().request_repaint();
            }
            let pos = hit(state, p);
            if let Some((_, h)) = &mut state.sel {
                *h = pos;
            }
            state.selected = Some(pos.row);
        }
    }
    if response.double_clicked()
        && let Some(p) = response.interact_pointer_pos()
        && rows_rect.contains(p)
        && rows > 0
    {
        let pos = hit(state, p);
        let (a, b) = word_at(cells, &line_str(pos.row), pos.col);
        state.sel = Some((TextPos { row: pos.row, col: a }, TextPos { row: pos.row, col: b }));
    }
    if response.secondary_clicked()
        && let Some(p) = response.interact_pointer_pos()
        && rows_rect.contains(p)
        && rows > 0
    {
        // Right-click outside the selection moves the cursor there.
        let pos = hit(state, p);
        let inside = state.selection().is_some_and(|(s, e)| (s.row..=e.row).contains(&pos.row));
        if !inside {
            state.sel = None;
            state.selected = Some(pos.row);
        }
    }

    let mut copy = false;
    let mut detail = None;
    if has_focus && rows > 0 {
        let sel = state.selected.unwrap_or(state.top);
        let page = state.page;
        let (ctrl, mut target) = ui.input(|i| (i.modifiers.command, None::<u64>));
        ui.input(|i| {
            if i.key_pressed(Key::ArrowDown) {
                target = Some(sel + 1)
            }
            if i.key_pressed(Key::ArrowUp) {
                target = Some(sel.saturating_sub(1))
            }
            if i.key_pressed(Key::PageDown) {
                target = Some(sel + page)
            }
            if i.key_pressed(Key::PageUp) {
                target = Some(sel.saturating_sub(page))
            }
            if i.key_pressed(Key::Home) && ctrl {
                target = Some(0)
            }
            if i.key_pressed(Key::End) && ctrl {
                target = Some(rows - 1)
            }
            // On a record of several lines → unfolds it and ← folds it;
            // elsewhere they scroll sideways.
            let line = line_of_row(sel.min(rows - 1));
            let several = || source.line_bytes(line).is_some_and(|b| b.contains(&b'\n'));
            let open = state.unfold_all || state.unfolded.contains(&line);
            if i.key_pressed(Key::ArrowRight) {
                if !open && several() {
                    state.unfolded.insert(line);
                } else {
                    state.x_off += char_w * 8.0
                }
            }
            if i.key_pressed(Key::ArrowLeft) {
                if state.unfolded.contains(&line) {
                    state.unfolded.remove(&line);
                } else {
                    state.x_off = (state.x_off - char_w * 8.0).max(0.0)
                }
            }
            if i.key_pressed(Key::Escape) {
                state.sel = None;
                state.picked.clear();
            }
            if i.key_pressed(Key::Space) && ctrl {
                let line = line_of_row(sel.min(rows - 1));
                if !state.picked.remove(&line) {
                    state.picked.insert(line);
                }
            }
            if i.key_pressed(Key::Enter) && !i.modifiers.any() {
                detail = Some(sel.min(rows - 1))
            }
            if i.key_pressed(Key::A) && ctrl {
                state.sel = Some((TextPos { row: 0, col: 0 }, TextPos { row: rows - 1, col: usize::MAX }))
            }
            copy |= i.events.iter().any(|e| matches!(e, egui::Event::Copy));
        });
        if let Some(t) = target {
            let t = t.min(rows - 1);
            if shift {
                // Shift+arrows select whole lines from the anchor row.
                let anchor_row = state.sel.map_or(sel, |(a, _)| a.row);
                let (a, h) = if t >= anchor_row { (0, usize::MAX) } else { (usize::MAX, 0) };
                state.sel = Some((TextPos { row: anchor_row, col: a }, TextPos { row: t, col: h }));
                state.selected = Some(t);
                state.scroll_to(t);
            } else {
                state.select(t, rows);
            }
        }
    }

    let mut toggle_bookmark = None;
    response.context_menu(|ui| {
        let has_sel = state.selection().is_some();
        let label = match state.picked.len() {
            0 if has_sel => "Copy".to_owned(),
            0 => "Copy line".to_owned(),
            1 => "Copy the picked line".to_owned(),
            n => format!("Copy the {n} picked lines"),
        };
        if ui.add_enabled(rows > 0, egui::Button::new(label)).clicked() {
            copy = true;
            ui.close();
        }
        if !state.picked.is_empty() && ui.button("Unpick all  (Esc)").clicked() {
            state.picked.clear();
            ui.close();
        }
        if ui.add_enabled(rows > 0, egui::Button::new("Select all")).clicked() {
            state.sel = Some((TextPos { row: 0, col: 0 }, TextPos { row: rows - 1, col: usize::MAX }));
            ui.close();
        }
        if ui.add_enabled(state.selected.is_some(), egui::Button::new("Show whole line…  (Enter)")).clicked() {
            detail = state.selected;
            ui.close();
        }
        ui.separator();
        if ui.add_enabled(state.selected.is_some(), egui::Button::new("Toggle bookmark  (m)")).clicked() {
            toggle_bookmark = state.selected;
            ui.close();
        }
    });

    // Notes go along with copied lines where the view shows them, as
    // `// name · note` (the note alone, without the line's other fields):
    // a line before, or after the text (End of line: two spaces; Column:
    // one more cell). A partial line gets it only at the end it covers.
    let note_after = match hl.notes {
        NoteMode::EndOfLine => Some("  "),
        NoteMode::Column if note_column => Some("\t"),
        _ => None,
    };
    let with_note = |out: &mut String, text: &str, line: LineNo, from_start: bool, to_end: bool| {
        let note = hl.bookmarks.and_then(|b| shown_note(b, line)).filter(|_| hl.notes != NoteMode::Off);
        if let Some(n) = note.as_ref().filter(|_| note_rows.is_some() && from_start) {
            out.push_str(&format!("// {n}\n"));
        }
        out.push_str(text);
        if let (Some(n), Some(sep), true) = (&note, note_after, to_end) {
            out.push_str(&format!("{sep}// {n}"));
        }
    };
    let mut copied = None;
    if copy && !state.picked.is_empty() {
        // Picked lines, whole, in file order.
        let mut out = String::new();
        let n = state.picked.len().min(MAX_COPY_LINES as usize);
        for (k, &line) in state.picked.iter().take(n).enumerate() {
            if k > 0 {
                out.push('\n');
            }
            with_note(&mut out, &source.line_text(line).unwrap_or_default(), line, true, true);
        }
        ui.ctx().copy_text(out);
        copied = Some((n as u64, n < state.picked.len()));
    } else if copy && rows > 0 {
        let (start, end) = state.selection().unwrap_or_else(|| {
            let r = state.selected.unwrap_or(state.top);
            (TextPos { row: r, col: 0 }, TextPos { row: r, col: usize::MAX })
        });
        let last = end.row.min(start.row + MAX_COPY_LINES - 1);
        let mut out = String::new();
        for row in start.row..=last {
            let s = line_str(row);
            let a = if row == start.row { col_to_byte(cells, &s, start.col) } else { 0 };
            let b = if row == end.row { col_to_byte(cells, &s, end.col) } else { s.len() };
            with_note(&mut out, &s[a..b.max(a)], line_of_row(row), a == 0, b >= s.len());
            if row < end.row {
                out.push('\n');
            }
        }
        ui.ctx().copy_text(out);
        copied = Some((last - start.row + 1, last < end.row));
    }

    // ----- scrollbar -----
    let bar = ui.interact(bar_rect, id.with("bar"), Sense::click_and_drag());
    let bar_h = bar_rect.height().max(0.0);
    let thumb_h = if rows == 0 {
        bar_h
    } else {
        // min ≤ max even when the panel is shorter than the minimum thumb.
        (bar_h * state.page as f32 / rows as f32).clamp(32.0_f32.min(bar_h), bar_h)
    };
    let track = (bar_rect.height() - thumb_h).max(1.0);
    if (bar.dragged() || bar.clicked())
        && let Some(p) = bar.interact_pointer_pos()
    {
        let f = ((p.y - bar_rect.top() - thumb_h / 2.0) / track).clamp(0.0, 1.0) as f64;
        state.top = (f * max_top as f64).round() as u64;
        if f >= 1.0 {
            state.skip = end;
        }
    }
    if (state.top, state.skipped()) > end {
        (state.top, state.skip) = (end.0, end);
    }
    let hbar = ui.interact(hbar_rect, id.with("hbar"), Sense::click_and_drag());
    let hmax = max_x(state.content_w);
    let hthumb_w = if hmax > 0.0 {
        (hbar_rect.width() * clip_w / (clip_w + hmax)).clamp(32.0_f32.min(hbar_rect.width()), hbar_rect.width())
    } else {
        hbar_rect.width()
    };
    let htrack = (hbar_rect.width() - hthumb_w).max(1.0);
    if hbar_h > 0.0
        && (hbar.dragged() || hbar.clicked())
        && let Some(p) = hbar.interact_pointer_pos()
    {
        let f = ((p.x - hbar_rect.left() - hthumb_w / 2.0) / htrack).clamp(0.0, 1.0);
        state.x_off = (f * hmax).round();
    }
    if over_divider || state.resizing.is_some() {
        ui.ctx().set_cursor_icon(CursorIcon::ResizeColumn);
    } else if response.hovered() && !bar.hovered() {
        ui.ctx().set_cursor_icon(CursorIcon::Text);
    }

    // ----- paint -----
    let vis = ui.visuals().clone();
    let painter = ui.painter_at(outer);
    painter.rect_filled(outer, 0.0, vis.extreme_bg_color);
    painter.rect_filled(
        Rect::from_min_size(text_rect.min, vec2(gutter_w, text_rect.height())),
        0.0,
        vis.faint_bg_color,
    );
    let text_color = vis.text_color();
    let dim = vis.weak_text_color();
    let match_bg = Color32::from_rgba_unmultiplied(255, 200, 0, if vis.dark_mode { 90 } else { 140 });
    // Neutral grey for the cursor line, so it doesn't read as a colour code.
    let cursor_bg = if vis.dark_mode {
        Color32::from_rgba_unmultiplied(255, 255, 255, 18)
    } else {
        Color32::from_rgba_unmultiplied(0, 0, 0, 16)
    };
    let sel_bg = vis.selection.bg_fill;
    let selection = state.selection();

    // Note and unfolded slots can push a row that just moved into view below it.
    if let Some(sel) = state.selected.filter(|_| state.selected != selected_before) {
        // Keys go to a row's start; a click or a drag picked a line in
        // view, so an unfolded record scrolled into stays where it is.
        let by_pointer = clicked.is_some() || state.dragging;
        if sel < state.top || (sel == state.top && !by_pointer) {
            state.skip = (0, 0);
        }
        for _ in 0..state.page {
            let shown = slots(state.top, state.skipped(), rows, &is_open(state))
                .iter()
                .any(|(y, r, n)| *r == sel && *n == Slot::Line(0) && y + row_h <= rows_rect.bottom());
            if shown || state.top >= sel.min(max_top) {
                break;
            }
            state.top += 1;
        }
    }
    let note_job = |text: String| {
        let format = TextFormat { font_id: font.clone(), color: dim, italics: true, ..Default::default() };
        LayoutJob::single_section(text, format)
    };

    let mut max_w = 0.0f32;
    // Painted last, over the table's column lines.
    let mut note_slots = Vec::new();
    // Fold markers clicked: (file line, unfold).
    let mut folds: Vec<(LineNo, bool)> = Vec::new();
    let mut numbered = None;
    for (y, row, slot) in slots(state.top, state.skipped(), rows, &is_open(state)) {
        let line = line_of_row(row);
        let k = match slot {
            Slot::Note(note) => {
                note_slots.push((y, note));
                continue;
            }
            Slot::Line(k) => k,
        };
        let row_rect = Rect::from_min_size(pos2(text_rect.min.x, y), vec2(text_rect.width(), row_h));
        if let Some(lc) = hl.lines
            && let Some(&q) = lc.matched(line).as_deref().and_then(<[usize]>::last)
            && let Some(&c) = lc.colors.get(q)
        {
            let bg = Color32::from_rgba_unmultiplied(c.r(), c.g(), c.b(), if vis.dark_mode { 70 } else { 90 });
            painter.rect_filled(row_rect, 0.0, bg);
        }
        if state.selected == Some(row) {
            painter.rect_filled(row_rect, 0.0, cursor_bg);
        }
        if state.picked.contains(&line) {
            let text_part = Rect::from_min_max(pos2(text_clip.min.x, row_rect.min.y), row_rect.max);
            painter.rect_filled(text_part, 0.0, sel_bg);
        }
        let matched = hl.search.and_then(|s| s.matched(line));
        if let (Some(qs), Some(s)) = (&matched, hl.search) {
            // One thin stripe per matched search query, right of the
            // bookmark stripe, so queries without text spans show too.
            for (k, &q) in qs.iter().take(4).enumerate() {
                let x = row_rect.min.x + char_w * (0.8 + 0.45 * k as f32);
                let r = Rect::from_min_size(pos2(x, row_rect.min.y + 1.0), vec2(char_w * 0.35, row_h - 2.0));
                painter.rect_filled(r, 1.0, s.colors[q % s.colors.len().max(1)]);
            }
        }
        if hl.bookmarks.is_some_and(|b| b.contains(line)) {
            painter.rect_filled(Rect::from_min_size(row_rect.min, vec2(char_w * 0.6, row_h)), 0.0, BOOKMARK_COLOR);
        }
        // A lifecycle starts here: a line above the row, its label at the
        // right (on the row's first slot).
        if numbered != Some(row)
            && let Some(b) =
                hl.lifecycles.and_then(|bs| bs.binary_search_by_key(&line, |b| b.line).ok().map(|i| &bs[i]))
        {
            let c = if b.bad { Color32::from_rgb(220, 60, 60) } else { dim };
            painter.line_segment(
                [pos2(text_rect.min.x, row_rect.min.y), pos2(text_rect.max.x, row_rect.min.y)],
                Stroke::new(1.0, c),
            );
            // On the panel's colour, so the line's text under it doesn't mix in.
            let galley = painter.layout_no_wrap(b.label.clone(), FontId::monospace(10.0), c);
            let at = pos2(text_rect.max.x - char_w - galley.size().x, row_rect.min.y + 1.0);
            painter.rect_filled(Rect::from_min_size(at, galley.size()).expand(1.0), 2.0, vis.panel_fill);
            painter.galley(at, galley, c);
        }
        // Its number on its first slot shown (a record scrolled into).
        if numbered != Some(row) {
            numbered = Some(row);
            painter.text(
                pos2(text_rect.min.x + gutter_w - char_w * 0.75, snap(y + 1.0)),
                Align2::RIGHT_TOP,
                (line + 1).to_string(),
                font.clone(),
                dim,
            );
        }
        let open = state.unfold_all || state.unfolded.contains(&line);
        let bytes = source.line_bytes(line).unwrap_or_default();
        // A record of several lines: all of it (folded, its preview shows
        // where it matches).
        let several = bytes[..bytes.len().min(MAX_DISPLAY_BYTES)].contains(&b'\n');
        let limit = if open || several { MAX_UNFOLD_BYTES } else { MAX_DISPLAY_BYTES };
        let cut = bytes.len() > limit;
        let whole = String::from_utf8_lossy(&bytes[..bytes.len().min(limit)]);
        // A record of several lines: this slot's line of it (byte range in
        // `whole`), and how many there are.
        let breaks: Vec<usize> = whole.match_indices('\n').map(|(i, _)| i).collect();
        let lines_in = breaks.len() + 1;
        let start = if k == 0 { 0 } else { breaks.get(k - 1).map_or(whole.len(), |i| i + 1) };
        let end = breaks.get(k).copied().unwrap_or(whole.len());
        let last = k + 1 >= lines_in.min(if open { MAX_UNFOLD_LINES } else { 1 });
        let shown = std::borrow::Cow::Borrowed(whole[start..end].strip_suffix('\r').unwrap_or(&whole[start..end]));
        if let Some((s, e)) = selection
            && (s.row..=e.row).contains(&row)
        {
            // The selection in this line's columns (columns count the whole
            // record, a line break as one).
            let before = cells.width(&whole[..start]);
            let width = cells.width(&shown);
            let c0 = if row == s.row { s.col.saturating_sub(before).min(width) } else { 0 };
            let c1 = if row == e.row && e.col < before + width + 1 { e.col.saturating_sub(before) } else { width + 1 };
            let inside = row != s.row || s.col <= before + width;
            if inside && c1 > c0 {
                let x = snap(text_x0 - state.x_off);
                let r = Rect::from_min_max(pos2(x + c0 as f32 * char_w, y), pos2(x + c1 as f32 * char_w, y + row_h));
                painter.with_clip_rect(text_clip).rect_filled(r, 0.0, sel_bg);
            }
        }
        // The note column: the note as one more cell (shown only, not copied).
        let shown = match hl.bookmarks.and_then(|b| b.note(line)).filter(|n| note_column && !n.is_empty()) {
            Some(note) => std::borrow::Cow::Owned(format!("{shown}	{note}")),
            None => shown,
        };
        // Highlights over the whole record, then this line's share of them.
        let mut spans: Vec<(Range<usize>, Color32)> = Vec::new();
        if let Some(h) = hl.highlighter {
            spans.extend(h.spans(whole.as_bytes()).into_iter().map(|r| (r, match_bg)));
        }
        if let (Some(qs), Some(s)) = (&matched, hl.search) {
            for &q in qs {
                let (Some(h), Some(&c)) = (s.highlighters.get(q), s.colors.get(q)) else { continue };
                spans.extend(h.spans(whole.as_bytes()).into_iter().map(|r| (r, hit_bg(c, vis.dark_mode))));
            }
        }
        let record_spans = spans;
        let spans: Vec<(Range<usize>, Color32)> = record_spans
            .iter()
            .filter(|(r, _)| r.start < start + shown.len() && r.end > start)
            .map(|(r, c)| (r.start.max(start) - start..r.end.min(start + shown.len()) - start, *c))
            .collect();
        let job = line_job(&shown, cells, cut && last, &spans, &font, text_color, dim);
        let galley = painter.layout_job(job);
        let mut width = galley.size().x;
        let x = snap(text_x0 - state.x_off);
        painter.with_clip_rect(text_clip).galley(pos2(x, snap(y + 1.0)), galley, text_color);
        // A record of several lines: `▸ +N lines` after its first line
        // (folded), `▾` (unfolded); a click folds or unfolds it.
        if k == 0 && lines_in > 1 {
            let text = if open { "  ▾".to_owned() } else { format!("  ▸ +{} lines", lines_in - 1) };
            let galley = painter.layout_job(note_job(text));
            let at = pos2(x + width, snap(y + 1.0));
            let r = Rect::from_min_size(at, galley.size()).intersect(text_clip);
            width += galley.size().x;
            painter.with_clip_rect(text_clip).galley(at, galley, dim);
            if r.is_positive() {
                let tip = if open { "Fold the record (←)" } else { "Show the whole record (→)" };
                let click = ui.interact(r, response.id.with(("fold", line)), Sense::click()).on_hover_text(tip);
                if click.clicked() {
                    folds.push((line, !open));
                }
            }
            // Folded: the rest of the record after the marker, its lines
            // joined (a pretty JSON record's first line is only `{`).
            if !open {
                let (rest, rest_spans) = folded_preview(&whole, end, &record_spans);
                let galley = painter.layout_job(spans_job(&format!("  {rest}"), 2, &rest_spans, &font, dim));
                let at = pos2(x + width, snap(y + 1.0));
                width += galley.size().x;
                painter.with_clip_rect(text_clip).galley(at, galley, dim);
            }
        }
        if hl.notes == NoteMode::EndOfLine
            && last
            && let Some(note) = hl.bookmarks.and_then(|b| shown_note(b, line))
        {
            let galley = painter.layout_job(note_job(format!("◂ {note}")));
            let at = width + char_w * 2.0;
            width = at + galley.size().x;
            painter.with_clip_rect(text_clip).galley(pos2(x + at, snap(y + 1.0)), galley, dim);
        }
        // Only rows that stay in view under a horizontal scrollbar count:
        // the cut-off last row would show the bar, which then covers it, so
        // the bar would go again — and flicker on every repaint.
        if y < outer.max.y - SCROLLBAR_W {
            max_w = max_w.max(width);
        }
    }
    state.content_w = max_w;
    state.x_off = state.x_off.min(max_x(max_w));
    for (line, open) in folds {
        if open {
            state.unfolded.insert(line);
        } else {
            state.unfolded.remove(&line);
            // A record folded with "unfold all" on stays folded: the rest
            // stay unfolded.
            state.unfold_all = false;
        }
    }

    if table {
        let full = Rect::from_min_max(text_rect.min, header_rect.max);
        painter.rect_filled(full, 0.0, vis.faint_bg_color);
        painter.hline(text_rect.x_range(), header_rect.bottom(), vis.widgets.noninteractive.bg_stroke);
        let starts = columns.starts();
        let head = painter.with_clip_rect(header_rect);
        let header_font = FontId::proportional(12.5);
        for (k, name) in columns.names.iter().enumerate() {
            head.text(
                pos2(x0 + starts[k] as f32 * char_w, header_rect.center().y),
                Align2::LEFT_CENTER,
                name.as_ref(),
                header_font.clone(),
                vis.strong_text_color(),
            );
        }
        let line = vis.widgets.noninteractive.bg_stroke.color.gamma_multiply(0.6);
        for k in 0..columns.widths.len().saturating_sub(1) {
            let x = snap(divider_x(&starts, &columns.widths, k));
            let stroke = if state.resizing == Some(k) { vis.selection.stroke } else { Stroke::new(1.0, line) };
            painter.with_clip_rect(Rect::from_min_max(pos2(text_clip.min.x, text_rect.min.y), text_clip.max)).vline(
                x,
                text_rect.y_range(),
                stroke,
            );
        }
    }

    for (y, note) in note_slots {
        // The note above its line: no number, never scrolled sideways.
        let r = Rect::from_min_size(pos2(text_rect.min.x, y), vec2(text_rect.width(), row_h));
        painter.rect_filled(r, 0.0, vis.extreme_bg_color);
        painter.rect_filled(r, 0.0, vis.faint_bg_color);
        painter.rect_filled(Rect::from_min_size(r.min, vec2(char_w * 0.6, row_h)), 0.0, BOOKMARK_COLOR);
        let galley = painter.layout_job(note_job(format!("🔖 {note}")));
        painter.with_clip_rect(text_clip).galley(pos2(snap(text_x0), snap(y + 1.0)), galley, dim);
    }

    // scrollbar paint
    painter.rect_filled(bar_rect, 0.0, vis.faint_bg_color);
    // Where the view is, in rows: inside an unfolded record scrolled into,
    // the share of its slots scrolled past, so the thumb moves through it.
    let at_row = |(row, skip): (u64, usize)| -> f64 {
        if skip == 0 {
            return row as f64;
        }
        let (note, n) = row_slots(row, &is_open(state));
        row as f64 + skip as f64 / (usize::from(note.is_some()) + n) as f64
    };
    let (pos, last) = (at_row((state.top, state.skipped())), at_row(end));
    let thumb_y = bar_rect.top() + if last <= 0.0 { 0.0 } else { track * (pos / last).min(1.0) as f32 };
    let thumb = Rect::from_min_size(pos2(bar_rect.left() + 2.0, thumb_y), vec2(SCROLLBAR_W - 4.0, thumb_h));
    painter.vline(bar_rect.left(), bar_rect.y_range(), vis.widgets.noninteractive.bg_stroke);
    // Derived from the text colour so the thumb stands out in both themes.
    let thumb_col = text_color.gamma_multiply(if bar.dragged() || bar.hovered() { 0.7 } else { 0.5 });
    painter.rect_filled(thumb, 4.0, thumb_col);
    if bar.hovered() {
        ui.ctx().set_cursor_icon(CursorIcon::Default);
    }
    if hbar_h > 0.0 {
        painter.rect_filled(
            hbar_rect.union(Rect::from_min_max(pos2(text_rect.min.x, hbar_rect.min.y), hbar_rect.max)),
            0.0,
            vis.faint_bg_color,
        );
        painter.hline(hbar_rect.x_range(), hbar_rect.top(), vis.widgets.noninteractive.bg_stroke);
        let x = hbar_rect.left() + if hmax == 0.0 { 0.0 } else { htrack * (state.x_off / hmax).clamp(0.0, 1.0) };
        let thumb = Rect::from_min_size(pos2(x, hbar_rect.top() + 2.0), vec2(hthumb_w, SCROLLBAR_W - 4.0));
        let col = text_color.gamma_multiply(if hbar.dragged() || hbar.hovered() { 0.7 } else { 0.5 });
        painter.rect_filled(thumb, 4.0, col);
        if hbar.hovered() {
            ui.ctx().set_cursor_icon(CursorIcon::Default);
        }
    }
    if has_focus {
        painter.rect_stroke(outer.shrink(0.5), 0.0, Stroke::new(1.0, vis.selection.stroke.color), StrokeKind::Inside);
    }
    // Hovering a bookmarked line shows the bookmark's name and note.
    if let Some(b) = hl.bookmarks
        && response.hovered()
        && !bar.hovered()
        && rows > 0
        && let Some(p) = response.hover_pos()
        && rows_rect.contains(p)
        && let line = line_of_row(hit(state, p).row)
        && let Some(name) = shown_name(b, line)
    {
        let tip = match b.note(line).unwrap_or_default() {
            "" => format!("🔖 {name}"),
            note => format!("🔖 {name}\n{note}"),
        };
        response.clone().on_hover_text_at_pointer(tip);
    }

    let moved_to = state.selected.filter(|_| state.selected != selected_before);
    ViewOutput { clicked, has_focus, copied, toggle_bookmark, detail, moved_to }
}

/// How a line's characters map to display columns. In plain text a tab is
/// [`TAB_COLS`] wide. In a table, a tab advances to the start of the next
/// cell, and text that doesn't fit its cell is hidden (zero width) with a
/// "…" drawn in the cell's gap.
#[derive(Clone, Copy, Default)]
struct Cells<'a> {
    /// See [`Columns::widths`]; empty for plain text.
    widths: &'a [usize],
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct Glyph {
    byte: usize,
    ch: char,
    col: usize,
    /// Columns taken; 0 for text hidden by a narrow cell.
    w: usize,
    /// A tab ending a cell whose text was cut.
    cut: bool,
}

impl Cells<'_> {
    fn glyphs(&self, s: &str) -> Vec<Glyph> {
        let mut out = Vec::with_capacity(s.len());
        let (mut col, mut cell, mut cell_end, mut cut) = (0, 0, self.widths.first().copied().unwrap_or(0), false);
        for (byte, ch) in s.char_indices() {
            // The last cell (and plain text) is unbounded.
            let bounded = cell + 1 < self.widths.len();
            let mut g = Glyph { byte, ch, col, w: 1, cut: false };
            if ch == '\t' && bounded {
                g.w = cell_end - col;
                g.cut = cut;
                cell += 1;
                cell_end += self.widths[cell];
                cut = false;
            } else if ch == '\t' {
                g.w = TAB_COLS;
            } else if bounded && col + 1 >= cell_end {
                // Keep the last column of the cell free as a gap.
                g.w = 0;
                cut = true;
            }
            col += g.w;
            out.push(g);
        }
        out
    }

    /// Display width of `s` in columns.
    fn width(&self, s: &str) -> usize {
        self.glyphs(s).last().map_or(0, |g| g.col + g.w)
    }
}

/// Byte offset in `s` of display column `col` (clamped to the line end;
/// a column inside a tab rounds to the nearer edge). Hidden text belongs to
/// the end of its cell.
fn col_to_byte(cells: Cells<'_>, s: &str, col: usize) -> usize {
    for g in cells.glyphs(s).into_iter().filter(|g| g.w > 0) {
        if col < g.col + g.w.div_ceil(2) {
            return g.byte;
        }
        if col < g.col + g.w {
            return g.byte + g.ch.len_utf8();
        }
    }
    s.len()
}

/// Display-column range of the word (or run of other non-space chars) at `col`.
fn word_at(cells: Cells<'_>, s: &str, col: usize) -> (usize, usize) {
    let is_word = |c: char| c.is_alphanumeric() || c == '_';
    let glyphs = cells.glyphs(s);
    let Some(i) = glyphs.iter().rposition(|g| g.col <= col && g.w > 0) else { return (0, 0) };
    let class = |c: char| {
        if is_word(c) {
            0
        } else if c.is_whitespace() {
            1
        } else {
            2
        }
    };
    let k = class(glyphs[i].ch);
    let mut a = i;
    while a > 0 && class(glyphs[a - 1].ch) == k {
        a -= 1;
    }
    let mut b = i + 1;
    while b < glyphs.len() && class(glyphs[b].ch) == k {
        b += 1;
    }
    let end = glyphs.get(b).map_or_else(|| cells.width(s), |g| g.col);
    (glyphs[a].col, end)
}

/// Lay out a line. `spans` are byte ranges with a background colour; a
/// later span wins where they overlap (search colours over the filter's).
/// A folded record's preview: its lines from `from` on, trimmed and joined
/// by a space, at most [`FOLDED_PREVIEW_CHARS`]; with `spans` (over the
/// record) moved onto it. If it matches past the start, it starts a little
/// before the match, after a `…`.
fn folded_preview(
    whole: &str,
    from: usize,
    spans: &[(Range<usize>, Color32)],
) -> (String, Vec<(Range<usize>, Color32)>) {
    // (start in `whole`, start in the preview, length) of each piece.
    let (mut text, mut pieces) = (String::new(), Vec::new());
    let mut at = from;
    for line in whole[from..].split('\n') {
        let trimmed = line.trim();
        if !trimmed.is_empty() {
            let lead = line.len() - line.trim_start().len();
            if !text.is_empty() {
                text.push(' ');
            }
            pieces.push((at + lead, text.len(), trimmed.len()));
            text.push_str(trimmed);
        }
        at += line.len() + 1;
    }
    let mut moved: Vec<(Range<usize>, Color32)> = Vec::new();
    for (r, c) in spans {
        for &(w, t, len) in &pieces {
            let (s, e) = (r.start.max(w), r.end.min(w + len));
            if s < e {
                moved.push((s - w + t..e - w + t, *c));
            }
        }
    }
    // Start a little before the first match if it's past the start.
    const BEFORE: usize = 20;
    let first = moved.iter().map(|(r, _)| r.start).min().unwrap_or(0);
    let mut skip = 0;
    if text[..first].chars().count() > 2 * BEFORE {
        skip = text[..first].char_indices().rev().nth(BEFORE - 1).map_or(0, |(i, _)| i);
    }
    let kept: String = text[skip..].chars().take(FOLDED_PREVIEW_CHARS).collect();
    let lead = if skip > 0 { "… " } else { "" };
    let spans = moved
        .into_iter()
        .filter(|(r, _)| r.start >= skip && r.start < skip + kept.len())
        .map(|(r, c)| (r.start - skip + lead.len()..(r.end - skip).min(kept.len()) + lead.len(), c))
        .collect();
    (format!("{lead}{kept}"), spans)
}

/// `text` in `color`, with `spans` (shifted by `shift` bytes) on a background.
fn spans_job(text: &str, shift: usize, spans: &[(Range<usize>, Color32)], font: &FontId, color: Color32) -> LayoutJob {
    let mut job = LayoutJob::default();
    job.wrap.max_width = f32::INFINITY;
    let plain = TextFormat { font_id: font.clone(), color, ..Default::default() };
    let mut at = 0;
    for (r, c) in spans {
        let (s, e) = ((r.start + shift).max(at).min(text.len()), (r.end + shift).min(text.len()));
        if s >= e {
            continue;
        }
        job.append(&text[at..s], 0.0, plain.clone());
        job.append(&text[s..e], 0.0, TextFormat { background: *c, ..plain.clone() });
        at = e;
    }
    job.append(&text[at..], 0.0, plain);
    job
}

fn line_job(
    text: &str,
    cells: Cells<'_>,
    cut: bool,
    spans: &[(Range<usize>, Color32)],
    font: &FontId,
    color: Color32,
    dim: Color32,
) -> LayoutJob {
    let mut job = LayoutJob::default();
    job.wrap.max_width = f32::INFINITY;
    let plain = TextFormat { font_id: font.clone(), color, ..Default::default() };
    let format = |bg: Option<Color32>| match bg {
        Some(background) => TextFormat { background, ..plain.clone() },
        None => plain.clone(),
    };
    let mut bg_of: Vec<Option<Color32>> = Vec::new();
    if !spans.is_empty() {
        bg_of = vec![None; text.len()];
        for (r, c) in spans {
            bg_of[r.start.min(text.len())..r.end.min(text.len())].fill(Some(*c));
        }
    }

    let (mut buf, mut in_bg) = (String::new(), None);
    for g in cells.glyphs(text) {
        let bg = bg_of.get(g.byte).copied().flatten();
        if bg != in_bg && !buf.is_empty() {
            job.append(&std::mem::take(&mut buf), 0.0, format(in_bg));
        }
        in_bg = bg;
        match g.ch {
            '\t' => {
                buf.push(if g.cut { '…' } else { ' ' });
                buf.extend(std::iter::repeat_n(' ', g.w - 1));
            }
            _ if g.w == 0 => {}
            c if c.is_control() => buf.push('·'),
            c => buf.push(c),
        }
    }
    if !buf.is_empty() {
        job.append(&buf, 0.0, format(in_bg));
    }
    if cut {
        job.append(" …[truncated]", 0.0, TextFormat { color: dim, ..plain });
    }
    job
}

fn digits(n: u64) -> u32 {
    n.max(1).ilog10() + 1
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEXT: Cells<'static> = Cells { widths: &[] };

    #[test]
    fn columns_map_to_bytes() {
        let s = "a\tbé c";
        // cols: a=0, tab=1..5, b=5, é=6, ' '=7, c=8
        assert_eq!(TEXT.width(s), 9);
        assert_eq!(col_to_byte(TEXT, s, 0), 0);
        assert_eq!(col_to_byte(TEXT, s, 1), 1);
        assert_eq!(col_to_byte(TEXT, s, 2), 1);
        assert_eq!(col_to_byte(TEXT, s, 3), 2);
        assert_eq!(col_to_byte(TEXT, s, 5), 2);
        assert_eq!(col_to_byte(TEXT, s, 7), 5);
        assert_eq!(col_to_byte(TEXT, s, 9), s.len());
        assert_eq!(col_to_byte(TEXT, s, usize::MAX), s.len());
    }

    fn rendered(cells: Cells<'_>, s: &str) -> String {
        let job = line_job(s, cells, false, &[], &FontId::monospace(13.0), Color32::WHITE, Color32::GRAY);
        job.text
    }

    #[test]
    fn table_cells_align_and_cut() {
        // Cells 4 and 3 wide (gap included), last one unbounded.
        let cells = Cells { widths: &[4, 3, 0] };
        assert_eq!(rendered(cells, "ab\tc\tpayload"), "ab  c  payload");
        // "abcdef" doesn't fit: shows "abc", hides "def", marks the gap.
        assert_eq!(rendered(cells, "abcdef\tc\tp\tq"), "abc…c  p    q");
        let s = "abcdef\tc\tp";
        assert_eq!(cells.width(s), 8);
        // Column 3 (the gap) maps to the tab, so selecting up to it copies
        // the hidden text too; column 2 stops before "c".
        assert_eq!(col_to_byte(cells, s, 3), 6);
        assert_eq!(col_to_byte(cells, s, 2), 2);
        assert_eq!(col_to_byte(cells, s, 4), 7);
        assert_eq!(word_at(cells, s, 1), (0, 3));
        assert_eq!(word_at(cells, "ab\tc\tpay load", 8), (7, 10));
        // Missing cells are fine.
        assert_eq!(rendered(cells, "x"), "x");
    }

    #[test]
    fn highlight_spans_follow_cells() {
        let h = nerka_core::Filter::Regex(nerka_core::Matcher::new("c\tp", Default::default()).unwrap()).highlighter();
        let text = "abcdef\tc\tpq";
        let spans: Vec<_> = h.spans(text.as_bytes()).into_iter().map(|r| (r, Color32::YELLOW)).collect();
        let job = line_job(
            text,
            Cells { widths: &[4, 3, 0] },
            false,
            &spans,
            &FontId::monospace(13.0),
            Color32::WHITE,
            Color32::GRAY,
        );
        assert_eq!(backgrounds(&job), [("c  p", Color32::YELLOW)]);
    }

    /// Text and background of every highlighted section.
    fn backgrounds(job: &LayoutJob) -> Vec<(&str, Color32)> {
        job.sections
            .iter()
            .filter(|s| s.format.background != Color32::TRANSPARENT)
            .map(|s| (&job.text[s.byte_range.start.0..s.byte_range.end.0], s.format.background))
            .collect()
    }

    #[test]
    fn later_spans_win_where_they_overlap() {
        // Filter highlight on "frame drop", search query colours on "cam1"
        // and "drop": each keeps its own colour.
        let (y, r, b) = (Color32::YELLOW, Color32::RED, Color32::BLUE);
        let spans = [(5..15, y), (0..4, r), (11..15, b)];
        let job =
            line_job("cam1 frame drop!", TEXT, false, &spans, &FontId::monospace(13.0), Color32::WHITE, Color32::GRAY);
        assert_eq!(backgrounds(&job), [("cam1", r), ("frame ", y), ("drop", b)]);
    }

    /// One frame of the app's layout: filter view in a bottom panel under a
    /// header row, main view in the centre. Returns the filter view's rect.
    fn frame(
        ctx: &egui::Context,
        t: f64,
        events: Vec<egui::Event>,
        file: &nerka_core::TextFile,
        main: &mut ViewState,
        filter: &mut ViewState,
    ) -> Rect {
        let rows: Vec<u64> = (0..file.line_count()).step_by(2).collect();
        let screen = Rect::from_min_size(pos2(0.0, 0.0), vec2(1000.0, 800.0));
        let input = egui::RawInput { screen_rect: Some(screen), time: Some(t), events, ..Default::default() };
        let hl = Highlight {
            highlighter: None,
            bookmarks: None,
            search: None,
            lines: None,
            notes: NoteMode::Off,
            lifecycles: None,
        };
        let mut rect = Rect::NOTHING;
        let mut out = ctx.run_ui(input, |ui| {
            egui::Panel::bottom("filter").resizable(true).default_size(260.0).min_size(120.0).show(ui, |ui| {
                ui.horizontal(|ui| ui.label("filter"));
                rect = ui.available_rect_before_wrap();
                log_view(
                    ui,
                    "filter",
                    filter,
                    rows.len() as u64,
                    &|r| rows[r as usize],
                    file,
                    &mut Columns::default(),
                    &hl,
                );
            });
            egui::CentralPanel::default().show(ui, |ui| {
                log_view(ui, "main", main, file.line_count(), &|r| r, file, &mut Columns::default(), &hl);
            });
        });
        out.textures_delta.clear();
        rect
    }

    fn wheel(notches: f32) -> egui::Event {
        egui::Event::MouseWheel {
            unit: egui::MouseWheelUnit::Line,
            delta: vec2(0.0, -notches),
            phase: egui::TouchPhase::Move,
            source: egui::MouseWheelSource::Wheel,
            modifiers: egui::Modifiers::NONE,
        }
    }

    #[test]
    fn wheel_scrolls_hovered_view_by_whole_lines() {
        let text: String = (0..1000).map(|i| format!("line {i}\n")).collect();
        let file = nerka_core::TextFile::from_bytes("t", text.into_bytes());
        let ctx = egui::Context::default();
        let (mut main, mut filter) = (ViewState::default(), ViewState::default());
        let mut t = 0.0;
        let mut step = |events: Vec<egui::Event>, main: &mut ViewState, filter: &mut ViewState| {
            t += 1.0 / 60.0;
            frame(&ctx, t, events, &file, main, filter)
        };
        let rect = step(vec![], &mut main, &mut filter);
        step(vec![egui::Event::PointerMoved(rect.center())], &mut main, &mut filter);
        // One wheel notch, then let any smoothing run out.
        step(vec![wheel(1.0)], &mut main, &mut filter);
        for _ in 0..60 {
            step(vec![], &mut main, &mut filter);
        }
        assert_eq!(main.top, 0, "main view must not scroll");
        assert_eq!(filter.top, LINES_PER_NOTCH, "one notch scrolls a few lines");
        step(vec![wheel(-1.0)], &mut main, &mut filter);
        for _ in 0..60 {
            step(vec![], &mut main, &mut filter);
        }
        assert_eq!(filter.top, 0);
    }

    #[test]
    fn dragging_a_header_divider_resizes_its_column() {
        let file = nerka_core::TextFile::from_bytes("t", b"ab\tc\tpayload\n".to_vec());
        let ctx = egui::Context::default();
        let (mut state, mut columns) = (
            ViewState::default(),
            Columns { names: vec!["a".into(), "b".into(), "rest".into()], widths: vec![4, 3, 0] },
        );
        let hl = Highlight {
            highlighter: None,
            bookmarks: None,
            search: None,
            lines: None,
            notes: NoteMode::Off,
            lifecycles: None,
        };
        let mut t = 0.0;
        let mut step = |events: Vec<egui::Event>, state: &mut ViewState, columns: &mut Columns| {
            t += 1.0 / 60.0;
            let screen = Rect::from_min_size(pos2(0.0, 0.0), vec2(800.0, 400.0));
            let input = egui::RawInput { screen_rect: Some(screen), time: Some(t), events, ..Default::default() };
            let mut out = ctx.run_ui(input, |ui| {
                egui::CentralPanel::default().show(ui, |ui| {
                    log_view(ui, "v", state, 1, &|r| r, &file, columns, &hl);
                });
            });
            out.textures_delta.clear();
        };
        let button = |pos, pressed| egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };
        step(vec![], &mut state, &mut columns);
        // Find the first divider by probing along the header row.
        let y = 18.0;
        let x = (0..400)
            .map(|x| x as f32)
            .find(|&x| {
                step(vec![egui::Event::PointerMoved(pos2(x, y)), button(pos2(x, y), true)], &mut state, &mut columns);
                let found = state.resizing.is_some();
                step(vec![button(pos2(x, y), false)], &mut state, &mut columns);
                found
            })
            .expect("a divider in the header");
        assert_eq!(columns.widths, [4, 3, 0], "pressing alone must not resize");
        // Drag it 60 px to the right: the first column gets wider.
        step(vec![egui::Event::PointerMoved(pos2(x, y)), button(pos2(x, y), true)], &mut state, &mut columns);
        step(vec![egui::Event::PointerMoved(pos2(x + 60.0, y))], &mut state, &mut columns);
        step(vec![button(pos2(x + 60.0, y), false)], &mut state, &mut columns);
        assert!(columns.widths[0] > 4, "{:?}", columns.widths);
        assert_eq!(columns.widths[1], 3);
        assert_eq!(state.resizing, None);
        assert_eq!(state.selected, None, "header drags don't select rows");
    }

    #[test]
    fn long_lines_get_a_horizontal_scrollbar_and_enter_asks_for_the_line() {
        let long = format!("start {} end\nshort\n", "x".repeat(400));
        let file = nerka_core::TextFile::from_bytes("t", long.into_bytes());
        let short = nerka_core::TextFile::from_bytes("s", b"short\nlines\n".to_vec());
        let ctx = egui::Context::default();
        let hl = Highlight {
            highlighter: None,
            bookmarks: None,
            search: None,
            lines: None,
            notes: NoteMode::Off,
            lifecycles: None,
        };
        let mut t = 0.0;
        let mut step = |events: Vec<egui::Event>, state: &mut ViewState, file: &nerka_core::TextFile| {
            t += 1.0 / 60.0;
            let screen = Rect::from_min_size(pos2(0.0, 0.0), vec2(600.0, 300.0));
            let input = egui::RawInput { screen_rect: Some(screen), time: Some(t), events, ..Default::default() };
            let mut res = None;
            let mut out = ctx.run_ui(input, |ui| {
                egui::CentralPanel::default().frame(egui::Frame::NONE).show(ui, |ui| {
                    res = Some(log_view(ui, "v", state, file.line_count(), &|r| r, file, &mut Columns::default(), &hl));
                });
            });
            out.textures_delta.clear();
            res.unwrap()
        };
        let button = |pos, pressed| egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };

        // Short lines: no scrollbar, and nothing to scroll to.
        let mut s = ViewState::default();
        step(vec![], &mut s, &short);
        step(vec![], &mut s, &short);
        assert_eq!(s.x_off, 0.0);
        assert!(s.content_w < 600.0);

        let mut s = ViewState::default();
        step(vec![], &mut s, &file);
        step(vec![], &mut s, &file);
        assert!(s.content_w > 600.0, "{}", s.content_w);
        // Drag the thumb (bottom left) to the far right: the end of the
        // line comes into view.
        let thumb = pos2(80.0, 300.0 - SCROLLBAR_W / 2.0);
        step(vec![egui::Event::PointerMoved(thumb), button(thumb, true)], &mut s, &file);
        let right = pos2(600.0, thumb.y);
        step(vec![egui::Event::PointerMoved(right)], &mut s, &file);
        step(vec![button(right, false)], &mut s, &file);
        let max = s.content_w - (600.0 - SCROLLBAR_W) + 40.0;
        assert!(s.x_off > s.content_w - 600.0 && s.x_off < max, "x_off {} content {}", s.x_off, s.content_w);
        assert_eq!(s.selected, None, "scrollbar drags don't select rows");

        // Click the first row, then Enter asks for it.
        let row = pos2(300.0, 8.0);
        step(vec![egui::Event::PointerMoved(row), button(row, true)], &mut s, &file);
        step(vec![button(row, false)], &mut s, &file);
        let enter = egui::Event::Key {
            key: Key::Enter,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        };
        let out = step(vec![enter], &mut s, &file);
        assert_eq!(out.detail, Some(0));
    }

    /// A record of several lines starts folded (one row, `▸ +N lines`); →
    /// unfolds it into a row per line, ← folds it. A click on one of its
    /// lines is a column of that line.
    #[test]
    fn records_fold_and_unfold() {
        use nerka_core::input::records::{RecordFile, RecordSplit};
        let mut text = String::new();
        for i in 0..40 {
            text.push_str(&format!("2025-10-17 rec {i}\n  first {i}\n  second {i}\n  third {i}\n"));
        }
        let file = RecordFile::from_bytes("t", text.into_bytes(), RecordSplit::parse(r"start=/\d{4}-/").unwrap());
        let ctx = egui::Context::default();
        let mut t = 0.0;
        let mut step = |events: Vec<egui::Event>, state: &mut ViewState| -> Vec<String> {
            t += 1.0 / 60.0;
            let screen = Rect::from_min_size(pos2(0.0, 0.0), vec2(600.0, 300.0));
            let input = egui::RawInput { screen_rect: Some(screen), time: Some(t), events, ..Default::default() };
            let hl = Highlight {
                highlighter: None,
                bookmarks: None,
                search: None,
                lines: None,
                notes: NoteMode::Off,
                lifecycles: None,
            };
            let mut out = ctx.run_ui(input, |ui| {
                egui::CentralPanel::default().frame(egui::Frame::NONE).show(ui, |ui| {
                    log_view(ui, "v", state, file.line_count(), &|r| r, &file, &mut Columns::default(), &hl);
                });
            });
            out.textures_delta.clear();
            out.platform_output
                .commands
                .into_iter()
                .filter_map(|c| match c {
                    egui::OutputCommand::CopyText(s) => Some(s),
                    _ => None,
                })
                .collect()
        };
        let key = |key| egui::Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        };
        let click = |y: f32| {
            let pos = pos2(200.0, y);
            let b = |pressed| egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: egui::Modifiers::NONE,
            };
            vec![egui::Event::PointerMoved(pos), b(true), b(false)]
        };
        let mut s = ViewState::default();
        step(vec![], &mut s);
        step(vec![], &mut s);
        let row_h = (ctx.fonts_mut(|f| f.row_height(&FontId::monospace(13.0))) + 2.0).round();
        let folded_page = s.page;
        assert!(folded_page >= 10, "a row per record: {folded_page}");
        // Select record 0 (a click on its row), unfold it with →.
        step(click(row_h * 0.5), &mut s);
        step(vec![key(Key::ArrowRight)], &mut s);
        step(vec![], &mut s);
        assert!(s.unfolded.contains(&0));
        assert_eq!(s.page, folded_page - 3, "its four lines take four rows");
        // A click on its third line (`  second 0`): the column there.
        step(click(row_h * 2.5), &mut s);
        assert_eq!(s.selected, Some(0));
        let (a, _) = s.sel.unwrap();
        assert!(a.col > "2025-10-17 rec 0\n  first 0\n".len(), "{a:?}");
        // Copy: the whole record, its lines included.
        let copied = step(vec![egui::Event::Copy], &mut s);
        assert_eq!(copied, ["2025-10-17 rec 0\n  first 0\n  second 0\n  third 0"]);
        // ← folds it.
        step(vec![key(Key::ArrowLeft)], &mut s);
        step(vec![], &mut s);
        assert!(s.unfolded.is_empty() && s.page == folded_page);
        // Unfold all.
        s.unfold_all = true;
        step(vec![], &mut s);
        step(vec![], &mut s);
        assert!(s.page <= folded_page / 4 + 1, "{}", s.page);
        // The wheel scrolls through a record's lines, not a record at a time.
        let at = egui::Event::PointerMoved(pos2(200.0, 50.0));
        let point = |rows: f32| egui::Event::MouseWheel {
            unit: egui::MouseWheelUnit::Point,
            delta: vec2(0.0, -rows * row_h),
            phase: egui::TouchPhase::Move,
            source: egui::MouseWheelSource::Wheel,
            modifiers: egui::Modifiers::NONE,
        };
        s.selected = None;
        step(vec![at.clone(), point(1.0)], &mut s);
        assert_eq!((s.top, s.skipped()), (0, 1), "one line into record 0");
        step(vec![at.clone(), point(4.0)], &mut s);
        assert_eq!((s.top, s.skipped()), (1, 1), "on into record 1");
        step(vec![at.clone(), point(-2.0)], &mut s);
        assert_eq!((s.top, s.skipped()), (0, 3), "back up into record 0's last line");
    }

    /// A last record longer than the view, unfolded: the wheel scrolls down
    /// through it to its last line and stops there, never jumping back.
    #[test]
    fn long_last_record_scrolls_to_its_end() {
        use nerka_core::input::records::{RecordFile, RecordSplit};
        let mut text = String::new();
        for i in 0..5 {
            text.push_str(&format!("2025-10-17 rec {i}\n  first {i}\n"));
        }
        text.push_str("2025-10-17 last\n");
        for i in 0..60 {
            text.push_str(&format!("  line {i}\n"));
        }
        let file = RecordFile::from_bytes("t", text.into_bytes(), RecordSplit::parse(r"start=/\d{4}-/").unwrap());
        let rows = file.line_count();
        assert_eq!(rows, 6);
        let ctx = egui::Context::default();
        let mut t = 0.0;
        let mut step = |events: Vec<egui::Event>, state: &mut ViewState| {
            t += 1.0 / 60.0;
            let screen = Rect::from_min_size(pos2(0.0, 0.0), vec2(600.0, 300.0));
            let input = egui::RawInput { screen_rect: Some(screen), time: Some(t), events, ..Default::default() };
            let hl = Highlight {
                highlighter: None,
                bookmarks: None,
                search: None,
                lines: None,
                notes: NoteMode::Off,
                lifecycles: None,
            };
            let mut out = ctx.run_ui(input, |ui| {
                egui::CentralPanel::default().frame(egui::Frame::NONE).show(ui, |ui| {
                    log_view(ui, "v", state, rows, &|r| r, &file, &mut Columns::default(), &hl);
                });
            });
            out.textures_delta.clear();
        };
        let mut s = ViewState { unfold_all: true, ..Default::default() };
        step(vec![], &mut s);
        let row_h = (ctx.fonts_mut(|f| f.row_height(&FontId::monospace(13.0))) + 2.0).round();
        let at = egui::Event::PointerMoved(pos2(200.0, 50.0));
        let wheel = egui::Event::MouseWheel {
            unit: egui::MouseWheelUnit::Point,
            delta: vec2(0.0, -3.0 * row_h),
            phase: egui::TouchPhase::Move,
            source: egui::MouseWheelSource::Wheel,
            modifiers: egui::Modifiers::NONE,
        };
        let mut seen = vec![(s.top, s.skipped())];
        for _ in 0..40 {
            step(vec![at.clone(), wheel.clone()], &mut s);
            seen.push((s.top, s.skipped()));
        }
        assert!(seen.windows(2).all(|w| w[0] <= w[1]), "never back up: {seen:?}");
        let (top, skipped) = *seen.last().unwrap();
        assert_eq!(top, rows - 1, "{seen:?}");
        // The record's 61 lines end at the bottom: what's left of the view
        // above them is under a row.
        let shown = 61 - skipped;
        assert!(shown as f32 * row_h <= 300.0 && (shown + 1) as f32 * row_h > 300.0 - row_h, "{seen:?}");
    }

    /// One record unfolded among folded ones: the wheel goes into it and
    /// through it; a click on one of its lines (or on another row) leaves
    /// the view where it is; the scrollbar moves while inside it.
    #[test]
    fn scrolling_through_one_unfolded_record() {
        use nerka_core::input::records::{RecordFile, RecordSplit};
        let mut text = String::new();
        for i in 0..30 {
            text.push_str(&format!("2025-10-17 rec {i}\n"));
            let n = if i == 3 { 50 } else { 2 };
            for k in 0..n {
                text.push_str(&format!("  line {k} of {i}\n"));
            }
        }
        let file = RecordFile::from_bytes("t", text.into_bytes(), RecordSplit::parse(r"start=/\d{4}-/").unwrap());
        let rows = file.line_count();
        let ctx = egui::Context::default();
        let mut t = 0.0;
        let mut step = |events: Vec<egui::Event>, state: &mut ViewState| {
            t += 1.0 / 60.0;
            let screen = Rect::from_min_size(pos2(0.0, 0.0), vec2(600.0, 300.0));
            let input = egui::RawInput { screen_rect: Some(screen), time: Some(t), events, ..Default::default() };
            let hl = Highlight {
                highlighter: None,
                bookmarks: None,
                search: None,
                lines: None,
                notes: NoteMode::Off,
                lifecycles: None,
            };
            let mut out = ctx.run_ui(input, |ui| {
                egui::CentralPanel::default().frame(egui::Frame::NONE).show(ui, |ui| {
                    log_view(ui, "v", state, rows, &|r| r, &file, &mut Columns::default(), &hl);
                });
            });
            out.textures_delta.clear();
        };
        let mut s = ViewState::default();
        s.unfolded.insert(3);
        step(vec![], &mut s);
        step(vec![], &mut s);
        let row_h = (ctx.fonts_mut(|f| f.row_height(&FontId::monospace(13.0))) + 2.0).round();
        let at = egui::Event::PointerMoved(pos2(200.0, 50.0));
        let wheel = |n: f32| egui::Event::MouseWheel {
            unit: egui::MouseWheelUnit::Point,
            delta: vec2(0.0, -n * row_h),
            phase: egui::TouchPhase::Move,
            source: egui::MouseWheelSource::Wheel,
            modifiers: egui::Modifiers::NONE,
        };
        // Three rows down, then 20 of record 3's 51 lines.
        step(vec![at.clone(), wheel(23.0)], &mut s);
        assert_eq!((s.top, s.skipped()), (3, 20));
        // A click on a line of it: the view stays.
        let click = |y: f32| {
            let pos = pos2(200.0, y);
            let b = |pressed| egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: egui::Modifiers::NONE,
            };
            vec![egui::Event::PointerMoved(pos), b(true), b(false)]
        };
        step(click(row_h * 2.5), &mut s);
        step(vec![], &mut s);
        assert_eq!(s.selected, Some(3));
        assert_eq!((s.top, s.skipped()), (3, 20), "clicking inside it doesn't jump to its start");
        // On through it and past it, one notch at a time, never backwards.
        let mut seen = vec![(s.top, s.skipped())];
        for _ in 0..20 {
            step(vec![at.clone(), wheel(3.0)], &mut s);
            seen.push((s.top, s.skipped()));
        }
        assert!(seen.windows(2).all(|w| w[0] <= w[1]), "{seen:?}");
        // Three lines a notch while inside it, then three rows a notch.
        assert_eq!(&seen[..3], [(3, 20), (3, 23), (3, 26)]);
        assert!(seen.contains(&(3, 50)) && seen.contains(&(6, 0)), "{seen:?}");
        assert!(s.top > 3, "past it: {seen:?}");
        // A click on a row below it: the view stays too.
        let before = (s.top, s.skipped());
        step(click(row_h * 4.5), &mut s);
        step(vec![], &mut s);
        assert_eq!((s.top, s.skipped()), before);
        // And back up into it from below: its last lines first.
        while s.top > 3 {
            step(vec![at.clone(), wheel(-1.0)], &mut s);
        }
        assert_eq!((s.top, s.skipped()), (3, 50));
    }

    /// A folded record's preview: its other lines joined, starting near the
    /// first match (with a `…`) when that's further on, and its highlights.
    #[test]
    fn folded_preview_shows_the_match() {
        let red = Color32::RED;
        let rec = "{\n  \"a\": 1,\n  \"b\": 2\n}";
        let from = rec.find('\n').unwrap();
        let (text, spans) = folded_preview(rec, from, &[]);
        assert_eq!((text.as_str(), spans.len()), ("\"a\": 1, \"b\": 2 }", 0));
        let b = rec.find("\"b\"").unwrap();
        let (text, spans) = folded_preview(rec, from, &[(b..b + 3, red)]);
        assert_eq!(&text[spans[0].0.clone()], "\"b\"");
        // Far on: starts a little before it.
        let long = format!("{{\n{}\n  \"hit\": 1\n}}", "  \"x\": 0,\n".repeat(40));
        let at = long.find("hit").unwrap();
        let (text, spans) = folded_preview(&long, 1, &[(at..at + 3, red)]);
        assert!(text.starts_with("… "), "{text}");
        assert_eq!(&text[spans[0].0.clone()], "hit");
        assert!(text.find("hit").unwrap() < 40, "{text}");
    }

    /// A note on the line before takes a row of the view: fewer lines fit,
    /// and a click on the note picks its line.
    #[test]
    fn notes_on_the_line_before_take_a_row() {
        let text: String = (0..100).map(|i| format!("line {i}\n")).collect();
        let file = nerka_core::TextFile::from_bytes("t", text.into_bytes());
        let mut marks = Bookmarks::default();
        marks.toggle(2);
        marks.set_note(2, "look here");
        marks.toggle(4); // no note: no row of its own
        let ctx = egui::Context::default();
        let mut t = 0.0;
        let mut step = |events: Vec<egui::Event>, state: &mut ViewState, notes: NoteMode| {
            t += 1.0 / 60.0;
            let screen = Rect::from_min_size(pos2(0.0, 0.0), vec2(600.0, 300.0));
            let input = egui::RawInput { screen_rect: Some(screen), time: Some(t), events, ..Default::default() };
            let hl = Highlight {
                highlighter: None,
                bookmarks: Some(&marks),
                search: None,
                lines: None,
                notes,
                lifecycles: None,
            };
            let mut out = ctx.run_ui(input, |ui| {
                egui::CentralPanel::default().frame(egui::Frame::NONE).show(ui, |ui| {
                    log_view(ui, "v", state, file.line_count(), &|r| r, &file, &mut Columns::default(), &hl);
                });
            });
            out.textures_delta.clear();
        };
        let click = |k: f32, row_h: f32| {
            let pos = pos2(300.0, (k + 0.5) * row_h);
            let button = |pressed| egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: egui::Modifiers::NONE,
            };
            vec![egui::Event::PointerMoved(pos), button(true), button(false)]
        };

        let mut off = ViewState::default();
        step(vec![], &mut off, NoteMode::Off);
        step(vec![], &mut off, NoteMode::Off);
        let row_h = (ctx.fonts_mut(|f| f.row_height(&FontId::monospace(13.0))) + 2.0).round();
        let mut before = ViewState::default();
        step(vec![], &mut before, NoteMode::LineBefore);
        step(vec![], &mut before, NoteMode::LineBefore);
        assert_eq!(before.page + 1, off.page);

        // Slots: line 0, 1, the note, line 2, 3, 4 (no note), 5…
        for (slot, line) in [(1.0, 1), (2.0, 2), (3.0, 2), (4.0, 3), (6.0, 5)] {
            step(click(slot, row_h), &mut before, NoteMode::LineBefore);
            assert_eq!(before.selected, Some(line), "slot {slot}");
        }
        step(click(3.0, row_h), &mut off, NoteMode::Off);
        assert_eq!(off.selected, Some(3));
    }

    /// Copied lines take their bookmark's note as the view shows it, as a
    /// `// name · note` comment without the line's other fields.
    #[test]
    fn copied_lines_take_their_notes() {
        let file = nerka_core::TextFile::from_bytes("t", b"line 0\nline 1\nline 2\n".to_vec());
        let mut marks = Bookmarks::default();
        marks.toggle(1);
        marks.set_note(1, "look here");
        let ctx = egui::Context::default();
        let mut t = 0.0;
        let mut copy = |notes: NoteMode, picked: &[LineNo]| {
            let mut state = ViewState::default();
            let mut copied = Vec::new();
            // A Ctrl+click gives the view the focus; then the picks, copied.
            let pos = pos2(200.0, 6.0);
            let ctrl = egui::Modifiers::COMMAND;
            let button = |pressed| egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: ctrl,
            };
            let click =
                vec![egui::Event::ModifiersChanged(ctrl), egui::Event::PointerMoved(pos), button(true), button(false)];
            for events in [vec![], click, vec![egui::Event::Copy]] {
                if events.first() == Some(&egui::Event::Copy) {
                    state.picked = picked.iter().copied().collect();
                }
                t += 1.0 / 60.0;
                let screen = Rect::from_min_size(pos2(0.0, 0.0), vec2(600.0, 300.0));
                let input = egui::RawInput { screen_rect: Some(screen), time: Some(t), events, ..Default::default() };
                let hl = Highlight {
                    highlighter: None,
                    bookmarks: Some(&marks),
                    search: None,
                    lines: None,
                    notes,
                    lifecycles: None,
                };
                let mut out = ctx.run_ui(input, |ui| {
                    egui::CentralPanel::default().frame(egui::Frame::NONE).show(ui, |ui| {
                        log_view(ui, "v", &mut state, file.line_count(), &|r| r, &file, &mut Columns::default(), &hl);
                    });
                });
                out.textures_delta.clear();
                copied.extend(out.platform_output.commands.into_iter().filter_map(|c| match c {
                    egui::OutputCommand::CopyText(s) => Some(s),
                    _ => None,
                }));
            }
            copied
        };
        assert_eq!(copy(NoteMode::LineBefore, &[0, 1, 2]), ["line 0\n// B1 · look here\nline 1\nline 2"]);
        assert_eq!(copy(NoteMode::EndOfLine, &[1, 2]), ["line 1  // B1 · look here\nline 2"]);
        // A plain text log has no columns: its notes go on the line before.
        assert_eq!(copy(NoteMode::Column, &[1]), ["// B1 · look here\nline 1"]);
        assert_eq!(copy(NoteMode::Off, &[1]), ["line 1"]);
    }

    /// ↑ / ↓ in a focused view move its cursor line, and the view keeps the
    /// focus: egui moved it to the nearest button in that direction too, so
    /// the next press went through buttons instead of the log.
    #[test]
    fn arrows_move_through_the_log_not_through_buttons() {
        let text: String = (0..100).map(|i| format!("line {i}\n")).collect();
        let file = nerka_core::TextFile::from_bytes("t", text.into_bytes());
        let ctx = egui::Context::default();
        let hl = Highlight {
            highlighter: None,
            bookmarks: None,
            search: None,
            lines: None,
            notes: NoteMode::Off,
            lifecycles: None,
        };
        let mut t = 0.0;
        let mut step = |events: Vec<egui::Event>, state: &mut ViewState| {
            t += 1.0 / 60.0;
            let screen = Rect::from_min_size(pos2(0.0, 0.0), vec2(600.0, 400.0));
            let input = egui::RawInput { screen_rect: Some(screen), time: Some(t), events, ..Default::default() };
            let mut res = None;
            let mut out = ctx.run_ui(input, |ui| {
                egui::Panel::top("buttons").show(ui, |ui| {
                    ui.horizontal(|ui| {
                        let _ = ui.button("above") | ui.button("beside");
                    });
                });
                egui::Panel::bottom("more").show(ui, |ui| {
                    let _ = ui.button("below");
                });
                egui::CentralPanel::default().show(ui, |ui| {
                    res = Some(log_view(ui, "v", state, 100, &|r| r, &file, &mut Columns::default(), &hl));
                });
            });
            out.textures_delta.clear();
            res.unwrap()
        };
        let key = |key, pressed| egui::Event::Key {
            key,
            physical_key: None,
            pressed,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        };
        let button = |pos, pressed| egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };
        let mut s = ViewState::default();
        step(vec![], &mut s);
        let row = pos2(300.0, 100.0);
        step(vec![egui::Event::PointerMoved(row), button(row, true)], &mut s);
        step(vec![button(row, false)], &mut s);
        let start = s.selected.expect("a clicked row");
        for k in [Key::ArrowDown, Key::ArrowDown, Key::ArrowDown, Key::ArrowUp, Key::ArrowDown] {
            step(vec![key(k, true)], &mut s);
            let out = step(vec![key(k, false)], &mut s);
            assert!(out.has_focus, "the view lost the focus on {k:?}");
        }
        assert_eq!(s.selected, Some(start + 3));
        // Esc clears the selection but leaves the focus in the view.
        step(vec![key(Key::Escape, true)], &mut s);
        assert!(step(vec![key(Key::Escape, false)], &mut s).has_focus);

        // After a click on a button, the view (one that takes free keys)
        // gets the arrows back.
        s.free_keys = true;
        let at = s.selected.unwrap();
        let above = pos2(20.0, 12.0);
        step(vec![egui::Event::PointerMoved(above), button(above, true)], &mut s);
        assert!(!step(vec![button(above, false)], &mut s).has_focus, "the button took the focus");
        for _ in 0..2 {
            step(vec![key(Key::ArrowDown, true)], &mut s);
            assert!(step(vec![key(Key::ArrowDown, false)], &mut s).has_focus);
        }
        assert_eq!(s.selected, Some(at + 2));
    }

    /// A long line in the half-visible row at the bottom must not make the
    /// horizontal scrollbar come and go: showing the bar covered that row,
    /// which then no longer counted, so the bar went away and the row came
    /// back, every repaint (a hover is enough).
    #[test]
    fn a_long_last_row_does_not_make_the_scrollbar_flicker() {
        let ctx = egui::Context::default();
        let hl = Highlight {
            highlighter: None,
            bookmarks: None,
            search: None,
            lines: None,
            notes: NoteMode::Off,
            lifecycles: None,
        };
        let mut t = 0.0;
        // Wherever the long line is, one of these rows is the cut-off last one.
        for long in 0..30 {
            let text: String =
                (0..40).map(|i| if i == long { format!("{}\n", "x".repeat(400)) } else { format!("l{i}\n") }).collect();
            let file = nerka_core::TextFile::from_bytes("t", text.into_bytes());
            let mut s = ViewState::default();
            let mut widths = Vec::new();
            for _ in 0..8 {
                t += 1.0 / 60.0;
                let screen = Rect::from_min_size(pos2(0.0, 0.0), vec2(600.0, 300.0));
                let input = egui::RawInput { screen_rect: Some(screen), time: Some(t), ..Default::default() };
                let mut out = ctx.run_ui(input, |ui| {
                    egui::CentralPanel::default().frame(egui::Frame::NONE).show(ui, |ui| {
                        log_view(ui, "v", &mut s, file.line_count(), &|r| r, &file, &mut Columns::default(), &hl);
                    });
                });
                out.textures_delta.clear();
                widths.push(s.content_w);
            }
            assert!(widths[2..].windows(2).all(|w| w[0] == w[1]), "long line {long}: widths {widths:?}");
        }
    }

    #[test]
    fn ctrl_click_picks_single_lines_and_copy_takes_them() {
        let text: String = (0..20).map(|i| format!("line {i}\n")).collect();
        let file = nerka_core::TextFile::from_bytes("t", text.into_bytes());
        // Every second line, as a filtered view shows them.
        let rows: Vec<u64> = (0..20).step_by(2).collect();
        let ctx = egui::Context::default();
        let hl = Highlight {
            highlighter: None,
            bookmarks: None,
            search: None,
            lines: None,
            notes: NoteMode::Off,
            lifecycles: None,
        };
        let mut t = 0.0;
        let mut step = |events: Vec<egui::Event>, state: &mut ViewState| {
            t += 1.0 / 60.0;
            let screen = Rect::from_min_size(pos2(0.0, 0.0), vec2(600.0, 400.0));
            // Ctrl held for the frame when the events carry it.
            let modifiers = events
                .iter()
                .find_map(|e| match e {
                    egui::Event::PointerButton { modifiers, .. } | egui::Event::Key { modifiers, .. } => {
                        Some(*modifiers)
                    }
                    _ => None,
                })
                .unwrap_or_default();
            let mut events = events;
            events.insert(0, egui::Event::ModifiersChanged(modifiers));
            let input = egui::RawInput { screen_rect: Some(screen), time: Some(t), events, ..Default::default() };
            let mut res = None;
            let mut out = ctx.run_ui(input, |ui| {
                egui::CentralPanel::default().frame(egui::Frame::NONE).show(ui, |ui| {
                    let r = log_view(
                        ui,
                        "v",
                        state,
                        rows.len() as u64,
                        &|r| rows[r as usize],
                        &file,
                        &mut Columns::default(),
                        &hl,
                    );
                    res = Some(r);
                });
            });
            out.textures_delta.clear();
            let copied: Vec<String> = out
                .platform_output
                .commands
                .iter()
                .filter_map(|c| match c {
                    egui::OutputCommand::CopyText(s) => Some(s.clone()),
                    _ => None,
                })
                .collect();
            (res.unwrap(), copied)
        };
        let ctrl = egui::Modifiers::COMMAND;
        let click = |y: f32, modifiers| {
            let pos = pos2(200.0, y);
            vec![
                egui::Event::PointerMoved(pos),
                egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed: true, modifiers },
                egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed: false, modifiers },
            ]
        };
        let key =
            |key, modifiers| egui::Event::Key { key, physical_key: None, pressed: true, repeat: false, modifiers };
        let mut s = ViewState::default();
        step(vec![], &mut s);
        let row_h = 17.0;
        // Ctrl+click rows 3 and 1 (lines 6 and 2), and row 5 twice (unpicked).
        for row in [3.0, 1.0, 5.0, 5.0] {
            step(click(row * row_h + 6.0, ctrl), &mut s);
        }
        assert_eq!(s.picked.iter().copied().collect::<Vec<_>>(), [2, 6], "{:?}", s.picked);
        assert_eq!(s.sel, None, "picking selects no text");
        let (out, copied) = step(vec![egui::Event::Copy], &mut s);
        assert_eq!(out.copied, Some((2, false)));
        assert_eq!(copied, ["line 2\nline 6"]);
        // Ctrl+Space picks the cursor line; a plain click clears the picks.
        step(vec![key(Key::Space, ctrl)], &mut s);
        assert_eq!(s.picked.len(), 3);
        step(click(row_h * 2.0 + 6.0, egui::Modifiers::NONE), &mut s);
        assert!(s.picked.is_empty());
    }

    #[test]
    fn words() {
        let s = "ERROR [db_pool] timeout=30s";
        assert_eq!(word_at(TEXT, s, 2), (0, 5));
        assert_eq!(word_at(TEXT, s, 8), (7, 14));
        assert_eq!(word_at(TEXT, s, 5), (5, 6));
        assert_eq!(word_at(TEXT, s, 24), (24, 27));
        assert_eq!(word_at(TEXT, s, 100), (24, 27));
        assert_eq!(word_at(TEXT, "", 3), (0, 0));
    }
}
