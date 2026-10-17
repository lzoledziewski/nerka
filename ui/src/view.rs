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

use eframe::egui::{
    self, Align2, Color32, CursorIcon, FontId, Key, Rect, Sense, Stroke, StrokeKind, Ui, pos2,
    text::{LayoutJob, TextFormat},
    vec2,
};
use nerka_core::{Bookmarks, Highlighter, LineNo, LogSource};

/// Lines longer than this are cut for display (search still sees them).
const MAX_DISPLAY_BYTES: usize = 4096;
/// Copying more lines than this is cut short (the clipboard is not a file).
const MAX_COPY_LINES: u64 = 100_000;
const SCROLLBAR_W: f32 = 12.0;
const TAB_COLS: usize = 4;

/// A position in the view: row and display column (tabs count as
/// [`TAB_COLS`]). `col == usize::MAX` means "end of line".
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
}

pub struct ViewOutput {
    pub clicked: Option<u64>,
    pub has_focus: bool,
    /// Lines copied to the clipboard and whether the copy was cut short.
    pub copied: Option<(u64, bool)>,
    /// "Toggle bookmark" chosen from the context menu, for this row.
    pub toggle_bookmark: Option<u64>,
}

/// Draw `rows` rows; `line_of_row` maps a view row to a file line.
pub fn log_view(
    ui: &mut Ui,
    id: &str,
    state: &mut ViewState,
    rows: u64,
    line_of_row: &dyn Fn(u64) -> LineNo,
    source: &dyn LogSource,
    hl: &Highlight<'_>,
) -> ViewOutput {
    let font = FontId::monospace(13.0);
    let row_h = ui.fonts_mut(|f| f.row_height(&font)) + 2.0;
    let char_w = ui.fonts_mut(|f| f.glyph_width(&font, '0'));
    let total_lines = source.line_count();
    let gutter_w = (digits(total_lines) as f32 + 1.5) * char_w;

    let (outer, response) = ui.allocate_exact_size(ui.available_size(), Sense::click_and_drag());
    let id = ui.scope_id().with(id);
    let text_rect = Rect::from_min_max(outer.min, pos2(outer.max.x - SCROLLBAR_W, outer.max.y));
    let bar_rect = Rect::from_min_max(pos2(text_rect.max.x, outer.min.y), outer.max);
    let text_clip = Rect::from_min_max(pos2(text_rect.min.x + gutter_w, text_rect.min.y), text_rect.max);
    let text_x0 = text_clip.min.x + char_w * 0.5;

    state.page = ((text_rect.height() / row_h).floor() as u64).max(1);
    let max_top = rows.saturating_sub(state.page);
    let line_str = |row: u64| source.line_text(line_of_row(row)).unwrap_or_default();

    // ----- input -----
    if response.clicked() || response.drag_started() || response.secondary_clicked() {
        response.request_focus();
    }
    let has_focus = response.has_focus();
    if response.hovered() {
        let d = ui.input(|i| i.smooth_scroll_delta);
        if d.y != 0.0 {
            let lines = (-d.y / row_h).round() as i64;
            let lines = if lines == 0 { -d.y.signum() as i64 } else { lines };
            state.top = state.top.saturating_add_signed(lines * 3 / 2 + lines.signum()).min(max_top);
        }
        state.x_off = (state.x_off - d.x).max(0.0);
    }

    // Mouse → text position (row clamped to the rows that exist).
    let hit = |state: &ViewState, p: egui::Pos2| -> TextPos {
        let r = ((p.y - text_rect.top()) / row_h).floor();
        let row = if r < 0.0 { state.top.saturating_sub(1) } else { state.top + r as u64 };
        let col = ((p.x - text_x0 + state.x_off) / char_w).round().max(0.0) as usize;
        TextPos { row: row.min(rows.saturating_sub(1)), col }
    };
    let mut clicked = None;
    let (pressed, released, shift, pointer) = ui.input(|i| {
        (i.pointer.primary_pressed(), i.pointer.primary_released(), i.modifiers.shift, i.pointer.interact_pos())
    });
    if rows > 0
        && pressed
        && let Some(p) = pointer
        && text_rect.contains(p)
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
            if p.y < text_rect.top() {
                state.top = state.top.saturating_sub(1);
                ui.ctx().request_repaint();
            } else if p.y > text_rect.bottom() {
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
        && text_rect.contains(p)
        && rows > 0
    {
        let pos = hit(state, p);
        let (a, b) = word_at(&line_str(pos.row), pos.col);
        state.sel = Some((TextPos { row: pos.row, col: a }, TextPos { row: pos.row, col: b }));
    }
    if response.secondary_clicked()
        && let Some(p) = response.interact_pointer_pos()
        && text_rect.contains(p)
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
            let a = if row == start.row { col_to_byte(&s, start.col) } else { 0 };
            let b = if row == end.row { col_to_byte(&s, end.col) } else { s.len() };
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
        (bar_h * state.page as f32 / rows as f32).clamp(24.0_f32.min(bar_h), bar_h)
    };
    let track = (bar_rect.height() - thumb_h).max(1.0);
    if (bar.dragged() || bar.clicked())
        && let Some(p) = bar.interact_pointer_pos()
    {
        let f = ((p.y - bar_rect.top() - thumb_h / 2.0) / track).clamp(0.0, 1.0) as f64;
        state.top = (f * max_top as f64).round() as u64;
    }
    state.top = state.top.min(max_top);
    if response.hovered() && !bar.hovered() {
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
        let y = text_rect.top() + i as f32 * row_h;
        let line = line_of_row(row);
        let row_rect = Rect::from_min_size(pos2(text_rect.min.x, y), vec2(text_rect.width(), row_h));
        if state.selected == Some(row) {
            painter.rect_filled(row_rect, 0.0, cursor_bg);
        }
        if hl.bookmarks.is_some_and(|b| b.contains(line)) {
            painter.rect_filled(
                Rect::from_min_size(row_rect.min, vec2(char_w * 0.6, row_h)),
                0.0,
                Color32::from_rgb(230, 120, 40),
            );
        }
        painter.text(
            pos2(text_rect.min.x + gutter_w - char_w * 0.75, y + 1.0),
            Align2::RIGHT_TOP,
            (line + 1).to_string(),
            font.clone(),
            dim,
        );
        let bytes = source.line_bytes(line).unwrap_or_default();
        let cut = bytes.len() > MAX_DISPLAY_BYTES;
        let shown = &bytes[..bytes.len().min(MAX_DISPLAY_BYTES)];
        if let Some((s, e)) = selection
            && (s.row..=e.row).contains(&row)
        {
            let width = display_cols(&String::from_utf8_lossy(shown));
            let c0 = if row == s.row { s.col.min(width) } else { 0 };
            // Selected line breaks show as one extra column.
            let c1 = if row == e.row { e.col.min(width) } else { width + 1 };
            if c1 > c0 {
                let x = text_x0 - state.x_off;
                let r = Rect::from_min_max(pos2(x + c0 as f32 * char_w, y), pos2(x + c1 as f32 * char_w, y + row_h));
                painter.with_clip_rect(text_clip).rect_filled(r, 0.0, sel_bg);
            }
        }
        let job = line_job(shown, cut, hl.highlighter, &font, text_color, dim, match_bg);
        let galley = painter.layout_job(job);
        max_w = max_w.max(galley.size().x);
        painter.with_clip_rect(text_clip).galley(pos2(text_x0 - state.x_off, y + 1.0), galley, text_color);
    }
    state.x_off = state.x_off.min((max_w - text_clip.width() * 0.5).max(0.0));

    // scrollbar paint
    painter.rect_filled(bar_rect, 0.0, vis.faint_bg_color);
    let thumb_y = bar_rect.top() + if max_top == 0 { 0.0 } else { track * (state.top as f64 / max_top as f64) as f32 };
    let thumb = Rect::from_min_size(pos2(bar_rect.left() + 2.0, thumb_y), vec2(SCROLLBAR_W - 4.0, thumb_h));
    let thumb_col =
        if bar.dragged() || bar.hovered() { vis.widgets.hovered.bg_fill } else { vis.widgets.inactive.bg_fill };
    painter.rect_filled(thumb, 4.0, thumb_col);
    if bar.hovered() {
        ui.ctx().set_cursor_icon(CursorIcon::Default);
    }
    if has_focus {
        painter.rect_stroke(outer.shrink(0.5), 0.0, Stroke::new(1.0, vis.selection.stroke.color), StrokeKind::Inside);
    }

    ViewOutput { clicked, has_focus, copied, toggle_bookmark }
}

fn char_cols(c: char) -> usize {
    if c == '\t' { TAB_COLS } else { 1 }
}

/// Display width of `s` in columns (matches [`clean`]).
fn display_cols(s: &str) -> usize {
    s.chars().map(char_cols).sum()
}

/// Byte offset in `s` of display column `col` (clamped to the line end;
/// a column inside a tab rounds to the nearer edge).
fn col_to_byte(s: &str, col: usize) -> usize {
    let mut c = 0;
    for (b, ch) in s.char_indices() {
        let w = char_cols(ch);
        if col < c + w.div_ceil(2) {
            return b;
        }
        if col < c + w {
            return b + ch.len_utf8();
        }
        c += w;
    }
    s.len()
}

/// Display-column range of the word (or run of other non-space chars) at `col`.
fn word_at(s: &str, col: usize) -> (usize, usize) {
    let is_word = |c: char| c.is_alphanumeric() || c == '_';
    let chars: Vec<(usize, char)> = {
        let mut c = 0;
        s.chars()
            .map(|ch| {
                let at = c;
                c += char_cols(ch);
                (at, ch)
            })
            .collect()
    };
    let Some(i) = chars.iter().rposition(|&(at, _)| at <= col) else { return (0, 0) };
    let class = |c: char| {
        if is_word(c) {
            0
        } else if c.is_whitespace() {
            1
        } else {
            2
        }
    };
    let k = class(chars[i].1);
    let mut a = i;
    while a > 0 && class(chars[a - 1].1) == k {
        a -= 1;
    }
    let mut b = i + 1;
    while b < chars.len() && class(chars[b].1) == k {
        b += 1;
    }
    let end = chars.get(b).map_or_else(|| display_cols(s), |&(at, _)| at);
    (chars[a].0, end)
}

fn line_job(
    bytes: &[u8],
    cut: bool,
    highlighter: Option<&Highlighter>,
    font: &FontId,
    color: Color32,
    dim: Color32,
    match_bg: Color32,
) -> LayoutJob {
    let mut job = LayoutJob::default();
    job.wrap.max_width = f32::INFINITY;
    let plain = TextFormat { font_id: font.clone(), color, ..Default::default() };
    let hit = TextFormat { background: match_bg, ..plain.clone() };

    let spans = highlighter.map(|h| h.spans(bytes)).unwrap_or_default();
    let mut pos = 0;
    for s in spans {
        if s.start > pos {
            job.append(&clean(&bytes[pos..s.start]), 0.0, plain.clone());
        }
        job.append(&clean(&bytes[s.clone()]), 0.0, hit.clone());
        pos = s.end;
    }
    if pos < bytes.len() {
        job.append(&clean(&bytes[pos..]), 0.0, plain.clone());
    }
    if cut {
        job.append(" …[truncated]", 0.0, TextFormat { color: dim, ..plain });
    }
    job
}

/// Lossy UTF-8 with tabs expanded and control chars made visible.
fn clean(b: &[u8]) -> String {
    let s = String::from_utf8_lossy(b);
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\t' => out.push_str(&" ".repeat(TAB_COLS)),
            c if c.is_control() => out.push('·'),
            c => out.push(c),
        }
    }
    out
}

fn digits(n: u64) -> u32 {
    n.max(1).ilog10() + 1
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn columns_map_to_bytes() {
        let s = "a\tbé c";
        // cols: a=0, tab=1..5, b=5, é=6, ' '=7, c=8
        assert_eq!(display_cols(s), 9);
        assert_eq!(col_to_byte(s, 0), 0);
        assert_eq!(col_to_byte(s, 1), 1);
        assert_eq!(col_to_byte(s, 2), 1);
        assert_eq!(col_to_byte(s, 3), 2);
        assert_eq!(col_to_byte(s, 5), 2);
        assert_eq!(col_to_byte(s, 7), 5);
        assert_eq!(col_to_byte(s, 9), s.len());
        assert_eq!(col_to_byte(s, usize::MAX), s.len());
    }

    #[test]
    fn words() {
        let s = "ERROR [db_pool] timeout=30s";
        assert_eq!(word_at(s, 2), (0, 5));
        assert_eq!(word_at(s, 8), (7, 14));
        assert_eq!(word_at(s, 5), (5, 6));
        assert_eq!(word_at(s, 24), (24, 27));
        assert_eq!(word_at(s, 100), (24, 27));
        assert_eq!(word_at("", 3), (0, 0));
    }
}
