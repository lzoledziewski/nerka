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
use std::ops::Range;

/// Lines longer than this are cut for display (search still sees them).
const MAX_DISPLAY_BYTES: usize = 4096;
/// Copying more lines than this is cut short (the clipboard is not a file).
const MAX_COPY_LINES: u64 = 100_000;
const SCROLLBAR_W: f32 = 14.0;
const TAB_COLS: usize = 4;
/// Rows scrolled per mouse-wheel notch (klogg uses 3).
const LINES_PER_NOTCH: u64 = 3;

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
    pub names: Vec<&'static str>,
    /// Cell widths in characters, including a one-character gap. The last
    /// column is unbounded; its entry is unused.
    pub widths: Vec<usize>,
}

impl Columns {
    pub fn from_defs(defs: &[ColumnDef]) -> Self {
        Columns {
            names: defs.iter().map(|d| d.name).collect(),
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

pub struct ViewOutput {
    pub clicked: Option<u64>,
    pub has_focus: bool,
    /// Lines copied to the clipboard and whether the copy was cut short.
    pub copied: Option<(u64, bool)>,
    /// "Toggle bookmark" chosen from the context menu, for this row.
    pub toggle_bookmark: Option<u64>,
    /// The whole line was asked for (Enter or the context menu), for this row.
    pub detail: Option<u64>,
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

    state.page = ((rows_rect.height() / row_h).floor() as u64).max(1);
    let max_top = rows.saturating_sub(state.page);
    let line_str = |row: u64| source.line_text(line_of_row(row)).unwrap_or_default();

    // ----- input -----
    if response.clicked() || response.drag_started() || response.secondary_clicked() {
        response.request_focus();
    }
    let has_focus = response.has_focus();
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
        state.top = state.top.saturating_add_signed(whole as i64).min(max_top);
        state.x_off = (state.x_off + x_delta).max(0.0);
    }

    // Mouse → text position (row clamped to the rows that exist).
    let hit = |state: &ViewState, p: egui::Pos2| -> TextPos {
        let r = ((p.y - rows_rect.top()) / row_h).floor();
        let row = if r < 0.0 { state.top.saturating_sub(1) } else { state.top + r as u64 };
        let col = ((p.x - text_x0 + state.x_off) / char_w).round().max(0.0) as usize;
        TextPos { row: row.min(rows.saturating_sub(1)), col }
    };
    let mut clicked = None;
    let (pressed, released, shift, pointer) = ui.input(|i| {
        (i.pointer.primary_pressed(), i.pointer.primary_released(), i.modifiers.shift, i.pointer.interact_pos())
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
        let anchor = match (shift, state.sel, state.selected) {
            (true, Some((a, _)), _) => a,
            (true, None, Some(r)) => TextPos { row: r, col: 0 },
            _ => pos,
        };
        state.sel = Some((anchor, pos));
        state.selected = Some(pos.row);
        state.dragging = true;
        clicked = Some(pos.row);
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
            if i.key_pressed(Key::ArrowRight) {
                state.x_off += char_w * 8.0
            }
            if i.key_pressed(Key::ArrowLeft) {
                state.x_off = (state.x_off - char_w * 8.0).max(0.0)
            }
            if i.key_pressed(Key::Escape) {
                state.sel = None
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
        if ui.add_enabled(rows > 0, egui::Button::new(if has_sel { "Copy" } else { "Copy line" })).clicked() {
            copy = true;
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

    let mut copied = None;
    if copy && rows > 0 {
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
            out.push_str(&s[a..b.max(a)]);
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
    }
    state.top = state.top.min(max_top);
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

    let mut max_w = 0.0f32;
    let last = (state.top + state.page + 1).min(rows);
    for (i, row) in (state.top..last).enumerate() {
        let y = snap(rows_rect.top()) + i as f32 * row_h;
        let line = line_of_row(row);
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
            painter.rect_filled(
                Rect::from_min_size(row_rect.min, vec2(char_w * 0.6, row_h)),
                0.0,
                Color32::from_rgb(230, 120, 40),
            );
        }
        painter.text(
            pos2(text_rect.min.x + gutter_w - char_w * 0.75, snap(y + 1.0)),
            Align2::RIGHT_TOP,
            (line + 1).to_string(),
            font.clone(),
            dim,
        );
        let bytes = source.line_bytes(line).unwrap_or_default();
        let cut = bytes.len() > MAX_DISPLAY_BYTES;
        let shown = String::from_utf8_lossy(&bytes[..bytes.len().min(MAX_DISPLAY_BYTES)]);
        if let Some((s, e)) = selection
            && (s.row..=e.row).contains(&row)
        {
            let width = cells.width(&shown);
            let c0 = if row == s.row { s.col.min(width) } else { 0 };
            // Selected line breaks show as one extra column.
            let c1 = if row == e.row { e.col.min(width) } else { width + 1 };
            if c1 > c0 {
                let x = snap(text_x0 - state.x_off);
                let r = Rect::from_min_max(pos2(x + c0 as f32 * char_w, y), pos2(x + c1 as f32 * char_w, y + row_h));
                painter.with_clip_rect(text_clip).rect_filled(r, 0.0, sel_bg);
            }
        }
        let mut spans: Vec<(Range<usize>, Color32)> = Vec::new();
        if let Some(h) = hl.highlighter {
            spans.extend(h.spans(shown.as_bytes()).into_iter().map(|r| (r, match_bg)));
        }
        if let (Some(qs), Some(s)) = (&matched, hl.search) {
            for &q in qs {
                let (Some(h), Some(&c)) = (s.highlighters.get(q), s.colors.get(q)) else { continue };
                spans.extend(h.spans(shown.as_bytes()).into_iter().map(|r| (r, hit_bg(c, vis.dark_mode))));
            }
        }
        let job = line_job(&shown, cells, cut, &spans, &font, text_color, dim);
        let galley = painter.layout_job(job);
        max_w = max_w.max(galley.size().x);
        painter.with_clip_rect(text_clip).galley(pos2(snap(text_x0 - state.x_off), snap(y + 1.0)), galley, text_color);
    }
    state.content_w = max_w;
    state.x_off = state.x_off.min(max_x(max_w));

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
                *name,
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

    // scrollbar paint
    painter.rect_filled(bar_rect, 0.0, vis.faint_bg_color);
    let thumb_y = bar_rect.top() + if max_top == 0 { 0.0 } else { track * (state.top as f64 / max_top as f64) as f32 };
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

    ViewOutput { clicked, has_focus, copied, toggle_bookmark, detail }
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
        let hl = Highlight { highlighter: None, bookmarks: None, search: None, lines: None };
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
        let (mut state, mut columns) =
            (ViewState::default(), Columns { names: vec!["a", "b", "rest"], widths: vec![4, 3, 0] });
        let hl = Highlight { highlighter: None, bookmarks: None, search: None, lines: None };
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
        let hl = Highlight { highlighter: None, bookmarks: None, search: None, lines: None };
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
