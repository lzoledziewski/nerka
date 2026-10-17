//! Virtualized log view.
//!
//! egui's `ScrollArea` positions rows in `f32` pixels, which loses precision
//! beyond ~10M rows. This view keeps its scroll position as a `u64` row
//! index instead, paints only the visible rows, and draws its own
//! scrollbar, so it behaves the same on a 1 KB file and a 100 M-line file.

use eframe::egui::{
    self, Align2, Color32, CursorIcon, FontId, Key, Rect, Sense, Stroke, StrokeKind, Ui, pos2,
    text::{LayoutJob, TextFormat},
    vec2,
};
use nerka_core::{Bookmarks, Highlighter, LineNo, LogSource, SearchResult};

/// Lines longer than this are cut for display (search still sees them).
const MAX_DISPLAY_BYTES: usize = 4096;
const SCROLLBAR_W: f32 = 12.0;

#[derive(Default)]
pub struct ViewState {
    /// First visible row.
    pub top: u64,
    /// Horizontal scroll in pixels.
    pub x_off: f32,
    /// Selected row (index into the view's rows, not a file line).
    pub selected: Option<u64>,
    /// Number of rows that fit in the last frame.
    pub page: u64,
}

impl ViewState {
    pub fn select(&mut self, row: u64, rows: u64) {
        let row = row.min(rows.saturating_sub(1));
        self.selected = Some(row);
        let page = self.page.max(1);
        if row < self.top {
            self.top = row;
        } else if row >= self.top + page {
            self.top = row + 1 - page;
        }
    }

    /// Select `row` and put it in the middle of the view.
    pub fn center_on(&mut self, row: u64, rows: u64) {
        self.selected = Some(row);
        self.top = row.saturating_sub(self.page / 2).min(rows.saturating_sub(self.page.max(1)));
    }
}

pub struct Highlight<'a> {
    pub highlighter: Option<&'a Highlighter>,
    pub bookmarks: Option<&'a Bookmarks>,
    /// Lines to tint (the main view tints lines that are in the result).
    pub tint: Option<&'a SearchResult>,
}

pub struct ViewOutput {
    pub clicked: Option<u64>,
    pub has_focus: bool,
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

    state.page = ((text_rect.height() / row_h).floor() as u64).max(1);
    let max_top = rows.saturating_sub(state.page);

    // ----- input -----
    if response.clicked() || response.drag_started() {
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
        });
        if let Some(t) = target {
            state.select(t, rows);
        }
        // Ctrl+C copies the selected line.
        if ui.input(|i| i.events.iter().any(|e| matches!(e, egui::Event::Copy)))
            && let Some(s) = state.selected
            && let Some(t) = source.line_text(line_of_row(s))
        {
            ui.ctx().copy_text(t.into_owned());
        }
    }
    let mut clicked = None;
    if response.clicked()
        && let Some(p) = response.interact_pointer_pos()
        && text_rect.contains(p)
    {
        let row = state.top + ((p.y - text_rect.top()) / row_h) as u64;
        if row < rows {
            state.selected = Some(row);
            clicked = Some(row);
        }
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
    let tint_bg = Color32::from_rgba_unmultiplied(80, 140, 255, if vis.dark_mode { 28 } else { 36 });

    let text_clip = Rect::from_min_max(pos2(text_rect.min.x + gutter_w, text_rect.min.y), text_rect.max);
    let mut max_w = 0.0f32;
    let last = (state.top + state.page + 1).min(rows);
    for (i, row) in (state.top..last).enumerate() {
        let y = text_rect.top() + i as f32 * row_h;
        let line = line_of_row(row);
        let row_rect = Rect::from_min_size(pos2(text_rect.min.x, y), vec2(text_rect.width(), row_h));
        if state.selected == Some(row) {
            painter.rect_filled(row_rect, 0.0, vis.selection.bg_fill.gamma_multiply(0.6));
        } else if hl.tint.is_some_and(|r| r.contains(line)) {
            painter.rect_filled(row_rect, 0.0, tint_bg);
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
        let job = line_job(shown, cut, hl.highlighter, &font, text_color, dim, match_bg);
        let galley = painter.layout_job(job);
        max_w = max_w.max(galley.size().x);
        painter.with_clip_rect(text_clip).galley(
            pos2(text_clip.min.x + char_w * 0.5 - state.x_off, y + 1.0),
            galley,
            text_color,
        );
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

    ViewOutput { clicked, has_focus }
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
            '\t' => out.push_str("    "),
            c if c.is_control() => out.push('·'),
            c => out.push(c),
        }
    }
    out
}

fn digits(n: u64) -> u32 {
    n.max(1).ilog10() + 1
}
