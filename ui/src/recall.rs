//! Browsing a history with ↑ / ↓ in a text box, shell style: ↑ steps to
//! older entries, ↓ back to newer ones and finally to what was typed
//! before browsing started. Typing ends browsing.

use eframe::egui;
use nerka_core::SearchHistory;

/// A history entry: `(tag, pattern)`, as [`SearchHistory`] stores it.
pub type Entry = (String, String);

/// Where a box is in its history while browsing.
#[derive(Debug, Clone, Default)]
pub struct Recall {
    /// Index of the entry shown, most recent first; `None` when not browsing.
    pos: Option<usize>,
    /// What the box held before browsing started.
    draft: Option<Entry>,
}

impl Recall {
    /// Stop browsing (the user typed): the next ↑ starts from the newest
    /// entry again.
    pub fn reset(&mut self) {
        *self = Recall::default();
    }

    /// One step: `older` for ↑, otherwise ↓. `current` is what the box
    /// holds now. Returns what the box should hold, or `None` to leave it.
    /// An entry equal to the text being browsed from is skipped, so the
    /// first ↑ always changes the box.
    pub fn step(&mut self, history: &SearchHistory, current: (&str, &str), older: bool) -> Option<Entry> {
        let entries: Vec<(&str, &str)> = history.iter().collect();
        let draft = self.draft.clone().unwrap_or_else(|| (current.0.to_owned(), current.1.to_owned()));
        let is_draft = |e: &(&str, &str)| e.0 == draft.0 && e.1 == draft.1;
        let mut next = match (self.pos, older) {
            (None, false) => return None,
            (None, true) => 0,
            (Some(p), true) => p + 1,
            (Some(0), false) => {
                self.reset();
                return Some(draft);
            }
            (Some(p), false) => p - 1,
        };
        while entries.get(next).is_some_and(is_draft) {
            match (older, next) {
                (true, _) => next += 1,
                (false, 0) => {
                    self.reset();
                    return Some(draft);
                }
                (false, _) => next -= 1,
            }
        }
        let (tag, pattern) = entries.get(next)?;
        self.draft = Some(draft);
        self.pos = Some(next);
        Some(((*tag).to_owned(), (*pattern).to_owned()))
    }
}

/// Handle ↑ / ↓ for a focused single-line box `r` holding `current`;
/// typing in it resets the browsing. Returns the entry to put in the box.
/// The cursor is put at the end of the new text.
pub fn keys(
    ui: &egui::Ui,
    r: &egui::Response,
    recall: &mut Recall,
    history: &SearchHistory,
    current: (&str, &str),
) -> Option<Entry> {
    if r.changed() {
        recall.reset();
    }
    if !r.has_focus() {
        return None;
    }
    let (up, down) = ui.input(|i| (i.key_pressed(egui::Key::ArrowUp), i.key_pressed(egui::Key::ArrowDown)));
    let entry = match (up, down) {
        (true, false) => recall.step(history, current, true),
        (false, true) => recall.step(history, current, false),
        _ => None,
    }?;
    if let Some(mut state) = egui::TextEdit::load_state(ui.ctx(), r.id) {
        let end = egui::text::CCursor::new(entry.1.chars().count());
        state.cursor.set_char_range(Some(egui::text::CCursorRange::one(end)));
        state.store(ui.ctx(), r.id);
    }
    Some(entry)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn history(entries: &[(&str, &str)]) -> SearchHistory {
        let mut h = SearchHistory::default();
        for (t, p) in entries.iter().rev() {
            h.push(t, p);
        }
        h
    }

    fn e(t: &str, p: &str) -> Option<Entry> {
        Some((t.into(), p.into()))
    }

    #[test]
    fn up_goes_older_down_back_to_the_draft() {
        let h = history(&[("text", "c"), ("regex", "b"), ("query", "a")]);
        let mut r = Recall::default();
        assert_eq!(r.step(&h, ("text", "typed"), false), None);
        assert_eq!(r.step(&h, ("text", "typed"), true), e("text", "c"));
        assert_eq!(r.step(&h, ("text", "c"), true), e("regex", "b"));
        assert_eq!(r.step(&h, ("regex", "b"), true), e("query", "a"));
        // The oldest stays.
        assert_eq!(r.step(&h, ("query", "a"), true), None);
        assert_eq!(r.step(&h, ("query", "a"), false), e("regex", "b"));
        assert_eq!(r.step(&h, ("regex", "b"), false), e("text", "c"));
        assert_eq!(r.step(&h, ("text", "c"), false), e("text", "typed"));
        assert_eq!(r.step(&h, ("text", "typed"), false), None);
    }

    #[test]
    fn the_entry_already_in_the_box_is_skipped() {
        let h = history(&[("text", "c"), ("regex", "b")]);
        let mut r = Recall::default();
        assert_eq!(r.step(&h, ("text", "c"), true), e("regex", "b"));
        assert_eq!(r.step(&h, ("regex", "b"), false), e("text", "c"));
        assert_eq!(r.step(&h, ("text", "c"), true), e("regex", "b"));
        r.reset();
        assert_eq!(r.step(&h, ("regex", "b"), true), e("text", "c"));
        assert_eq!(r.step(&h, ("text", "c"), true), None);
        // An empty history never changes the box.
        let mut r = Recall::default();
        assert_eq!(r.step(&SearchHistory::default(), ("text", ""), true), None);
    }
}
