//! The search manager's library panel: queries from packs, grouped by
//! label or by pack, picked for the Filter (include / exclude) or the
//! Search (a tick), with parameter fields. Picks belong to the tab.
//!
//! The panel only edits the picks. Expanding them into NFD is core's job
//! (`nerka_core::pack::expand`); the tab puts the result into the Filter
//! or Search box and runs it.

use eframe::egui::{self, Color32, RichText};
use nerka_core::pack::{Library, ParamValue, Params, Pick, Selection};
use std::collections::BTreeMap;

/// How a query takes part in the filter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Use {
    Include,
    Exclude,
}

/// What the tab picked from the library, plus parameter values.
#[derive(Debug, Clone, Default)]
pub struct Picks {
    /// Qualified ids in the order they were picked.
    pub filter: Vec<(String, Use)>,
    pub search: Vec<String>,
    /// Filter queries whose kept lines get a whole-line colour (off unless
    /// switched on).
    pub highlight: Vec<String>,
    /// Parameter values by name, with "insert as regex" (raw).
    pub params: BTreeMap<String, (String, bool)>,
    /// The last expansion error, shown in the panel.
    pub error: Option<String>,
}

impl Picks {
    pub fn filter_use(&self, id: &str) -> Option<Use> {
        self.filter.iter().find(|(q, _)| q == id).map(|(_, u)| *u)
    }

    pub fn set_filter(&mut self, id: &str, u: Option<Use>) {
        match (self.filter.iter().position(|(q, _)| q == id), u) {
            (Some(i), Some(u)) => self.filter[i].1 = u,
            (Some(i), None) => {
                self.filter.remove(i);
            }
            (None, Some(u)) => self.filter.push((id.to_owned(), u)),
            (None, None) => {}
        }
    }

    pub fn set_search(&mut self, id: &str, on: bool) {
        let at = self.search.iter().position(|q| q == id);
        match (at, on) {
            (None, true) => self.search.push(id.to_owned()),
            (Some(i), false) => {
                self.search.remove(i);
            }
            _ => {}
        }
    }

    pub fn toggle_highlight(&mut self, id: &str) {
        match self.highlight.iter().position(|q| q == id) {
            Some(i) => {
                self.highlight.remove(i);
            }
            None => self.highlight.push(id.to_owned()),
        }
    }

    pub fn is_highlighted(&self, id: &str) -> bool {
        self.highlight.iter().any(|q| q == id)
    }

    /// Position of `id` among the included filter queries (its palette
    /// slot, the same as in the expansion).
    pub fn include_index(&self, id: &str) -> Option<usize> {
        self.filter.iter().filter(|(_, u)| *u == Use::Include).position(|(q, _)| q == id)
    }

    pub fn filter_selection(&self) -> Selection {
        let pick = |u| self.filter.iter().filter(|(_, x)| *x == u).map(|(q, _)| Pick::Query(q.clone())).collect();
        Selection { include: pick(Use::Include), exclude: pick(Use::Exclude) }
    }

    pub fn search_selection(&self) -> Selection {
        Selection { include: self.search.iter().map(|q| Pick::Query(q.clone())).collect(), exclude: vec![] }
    }

    pub fn params(&self) -> Params {
        self.params.iter().map(|(k, (v, raw))| (k.clone(), ParamValue { value: v.clone(), raw: *raw })).collect()
    }
}

/// What the user asked for in the panel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    None,
    Close,
    Reload,
    ApplyFilter,
    ApplySearch,
}

/// Panel settings shared by all tabs.
#[derive(Debug, Default)]
pub struct LibraryView {
    pub by_pack: bool,
}

/// Colours for search queries without a `color:` in their pack.
const PALETTE: [Color32; 8] = [
    Color32::from_rgb(0, 160, 220),
    Color32::from_rgb(230, 90, 60),
    Color32::from_rgb(70, 180, 90),
    Color32::from_rgb(170, 100, 220),
    Color32::from_rgb(230, 160, 0),
    Color32::from_rgb(0, 170, 160),
    Color32::from_rgb(220, 80, 160),
    Color32::from_rgb(140, 120, 80),
];

/// A pack colour: `#rrggbb` or a common colour name.
pub fn parse_color(s: &str) -> Option<Color32> {
    if let Some(hex) = s.strip_prefix('#') {
        let v = u32::from_str_radix(hex, 16).ok().filter(|_| hex.len() == 6)?;
        return Some(Color32::from_rgb((v >> 16) as u8, (v >> 8) as u8, v as u8));
    }
    Some(match s {
        "red" => Color32::from_rgb(220, 50, 50),
        "orange" => Color32::from_rgb(240, 140, 30),
        "yellow" => Color32::from_rgb(220, 200, 0),
        "green" => Color32::from_rgb(60, 170, 70),
        "teal" => Color32::from_rgb(0, 160, 150),
        "cyan" => Color32::from_rgb(0, 180, 220),
        "blue" => Color32::from_rgb(50, 110, 230),
        "violet" | "purple" => Color32::from_rgb(150, 90, 220),
        "pink" | "magenta" => Color32::from_rgb(220, 70, 170),
        "brown" => Color32::from_rgb(150, 100, 60),
        "gray" | "grey" => Color32::from_rgb(140, 140, 140),
        _ => return None,
    })
}

/// Colour of a query: its pack colour, else the palette entry for `index`.
pub fn query_color(lib: &Library, id: &str, index: usize) -> Color32 {
    lib.get(id).and_then(|(_, q)| q.color.as_deref()).and_then(parse_color).unwrap_or(PALETTE[index % PALETTE.len()])
}

/// The colour of the `index`-th search query when it isn't from a pack.
pub fn palette(index: usize) -> Color32 {
    PALETTE[index % PALETTE.len()]
}

/// A small coloured name tag; `strong` fills it with the colour.
pub fn chip(ui: &mut egui::Ui, text: &str, color: Color32, strong: bool) -> egui::Response {
    egui::Frame::new()
        .fill(color.gamma_multiply(if strong { 0.8 } else { 0.25 }))
        .stroke(egui::Stroke::new(1.0, color))
        .corner_radius(6.0)
        .inner_margin(egui::Margin::symmetric(5, 0))
        .show(ui, |ui| ui.label(RichText::new(text).small()))
        .response
}

pub fn panel(
    ui: &mut egui::Ui,
    lib: &Library,
    load_errors: &[String],
    picks: &mut Picks,
    view: &mut LibraryView,
) -> Action {
    let mut action = Action::None;
    ui.horizontal(|ui| {
        ui.heading("Library");
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if crate::close_button(ui).on_hover_text("Close panel").clicked() {
                action = Action::Close;
            }
            if ui.button("⟳").on_hover_text("Reload the pack files").clicked() {
                action = Action::Reload;
            }
        });
    });
    ui.horizontal(|ui| {
        ui.weak("Group by");
        ui.selectable_value(&mut view.by_pack, false, "Label");
        ui.selectable_value(&mut view.by_pack, true, "Pack");
    });
    for e in load_errors {
        ui.colored_label(ui.visuals().error_fg_color, e);
    }
    ui.separator();
    if lib.packs().is_empty() {
        ui.weak("No packs loaded. Put *.nerka-pack files into the pack folder, or start with --pack <dir|file>.");
        if let Some(d) = nerka_core::pack::default_dir() {
            ui.weak(format!("Pack folder: {}", d.display()));
        }
        return action;
    }

    // Bottom part first, so the list gets the space that is left.
    egui::Panel::bottom("library-actions").show(ui, |ui| {
        params_ui(ui, lib, picks);
        if let Some(e) = &picks.error {
            ui.colored_label(ui.visuals().error_fg_color, e);
        }
        ui.horizontal(|ui| {
            let n_filter = picks.filter.len();
            let n_search = picks.search.len();
            if ui
                .add_enabled(n_filter > 0, egui::Button::new(format!("Filter ({n_filter})")))
                .on_hover_text("Put the picked filter queries into the Filter box and run it")
                .clicked()
            {
                action = Action::ApplyFilter;
            }
            if ui
                .add_enabled(n_search > 0, egui::Button::new(format!("Search ({n_search})")))
                .on_hover_text("Put the ticked search queries into the Search box and run it")
                .clicked()
            {
                action = Action::ApplySearch;
            }
            if ui.add_enabled(n_filter + n_search > 0, egui::Button::new("Clear")).clicked() {
                picks.filter.clear();
                picks.search.clear();
                picks.error = None;
            }
        });
        ui.weak("+ include · − exclude (Filter) · ☑ Search · ■ colour lines");
    });

    egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
        if view.by_pack {
            for p in lib.packs() {
                let ids: Vec<String> = p.queries.iter().map(|q| format!("{}.{}", p.name, q.id)).collect();
                let header = egui::CollapsingHeader::new(RichText::new(&p.name).strong())
                    .id_salt(("pack", &p.name))
                    .default_open(true)
                    .show(ui, |ui| group_ui(ui, lib, &ids, picks));
                header.header_response.on_hover_text(&p.file);
            }
        } else {
            for label in lib.labels() {
                let ids = lib.with_label(label);
                egui::CollapsingHeader::new(RichText::new(label).strong())
                    .id_salt(("label", label))
                    .default_open(true)
                    .show(ui, |ui| group_ui(ui, lib, &ids, picks));
            }
            let unlabelled: Vec<String> = lib
                .packs()
                .iter()
                .flat_map(|p| p.queries.iter().filter(|q| q.labels.is_empty()).map(|q| format!("{}.{}", p.name, q.id)))
                .collect();
            if !unlabelled.is_empty() {
                egui::CollapsingHeader::new(RichText::new("(no label)").weak())
                    .id_salt("unlabelled")
                    .default_open(true)
                    .show(ui, |ui| group_ui(ui, lib, &unlabelled, picks));
            }
        }
    });
    action
}

/// A label's or pack's queries, with buttons that pick all of them at
/// once. A label is resolved now: queries labelled later don't join.
fn group_ui(ui: &mut egui::Ui, lib: &Library, ids: &[String], picks: &mut Picks) {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 2.0;
        if ui.small_button("+ all").on_hover_text("Include all in the filter").clicked() {
            ids.iter().for_each(|id| picks.set_filter(id, Some(Use::Include)));
        }
        if ui.small_button("− all").on_hover_text("Exclude all from the filter").clicked() {
            ids.iter().for_each(|id| picks.set_filter(id, Some(Use::Exclude)));
        }
        if ui.small_button("☑ all").on_hover_text("Search for all").clicked() {
            ids.iter().for_each(|id| picks.set_search(id, true));
        }
        if ui.small_button("none").on_hover_text("Unpick all").clicked() {
            for id in ids {
                picks.set_filter(id, None);
                picks.set_search(id, false);
            }
        }
    });
    for id in ids {
        query_row(ui, lib, id, picks);
    }
}

fn query_row(ui: &mut egui::Ui, lib: &Library, id: &str, picks: &mut Picks) {
    let Some((pack, q)) = lib.get(id) else { return };
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        let state = picks.filter_use(id);
        let (sym, tip) = match state {
            None => ("·", "Not in the filter: click to include"),
            Some(Use::Include) => ("+", "Included in the filter (OR): click to exclude"),
            Some(Use::Exclude) => ("−", "Excluded from the filter (AND NOT): click to drop"),
        };
        let b = egui::Button::new(RichText::new(sym).monospace().strong())
            .selected(state.is_some())
            .min_size(egui::vec2(20.0, 0.0));
        if ui.add(b).on_hover_text(tip).clicked() {
            let next = match state {
                None => Some(Use::Include),
                Some(Use::Include) => Some(Use::Exclude),
                Some(Use::Exclude) => None,
            };
            picks.set_filter(id, next);
        }
        let mut on = picks.search.iter().any(|s| s == id);
        if ui.checkbox(&mut on, "").on_hover_text("Search for it").changed() {
            picks.set_search(id, on);
        }
        // The swatch switches the line colour of this query's filter lines.
        let color = query_color(lib, id, picks.include_index(id).unwrap_or(0));
        let lit = picks.is_highlighted(id);
        let (rect, r) = ui.allocate_exact_size(egui::vec2(12.0, 14.0), egui::Sense::click());
        let sw = egui::Rect::from_center_size(rect.center(), egui::vec2(10.0, 12.0));
        if lit {
            ui.painter().rect_filled(sw, 2.0, color);
        } else {
            ui.painter().rect_stroke(sw, 2.0, egui::Stroke::new(1.5, color), egui::StrokeKind::Inside);
        }
        let tip = if lit {
            "Its filter lines are coloured: click to stop"
        } else {
            "Colour the lines this query keeps in the filter (later picks win where lines overlap)"
        };
        if r.on_hover_text(tip).clicked() {
            picks.toggle_highlight(id);
        }
        let mut name = q.id.clone();
        if !q.params.is_empty() {
            name += &format!("({})", q.params.join(", "));
        }
        let mut tip = format!("{id}\n\n{}", q.text);
        if let Some(parent) = pack.parent_of(q) {
            tip += &format!("\n\nrefines {parent}");
        }
        if !q.labels.is_empty() {
            tip += &format!("\nlabels: {}", q.labels.join(", "));
        }
        ui.label(name).on_hover_text(tip);
        ui.weak(&pack.name);
    });
}

/// Fields for the parameters the picked queries need.
fn params_ui(ui: &mut egui::Ui, lib: &Library, picks: &mut Picks) {
    let ids = picks.filter.iter().map(|(q, _)| q.as_str()).chain(picks.search.iter().map(String::as_str));
    let names = lib.params_needed(ids);
    if names.is_empty() {
        return;
    }
    ui.label(RichText::new("Parameters").strong());
    for name in names {
        let (value, raw) = picks.params.entry(name.clone()).or_default();
        ui.horizontal(|ui| {
            ui.monospace(format!("${name}"));
            // Right to left: the field takes the width the checkbox leaves.
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.checkbox(raw, "regex").on_hover_text(
                    "Insert the value as regex syntax inside regexes (normally it is matched literally)",
                );
                ui.add(egui::TextEdit::singleline(value).desired_width(ui.available_width()));
            });
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colours() {
        assert_eq!(parse_color("#ff8000"), Some(Color32::from_rgb(255, 128, 0)));
        assert_eq!(parse_color("#ff80"), None);
        assert_eq!(parse_color("violet"), parse_color("purple"));
        assert_eq!(parse_color("plaid"), None);
    }

    #[test]
    fn picks_keep_order_and_cycle() {
        let mut p = Picks::default();
        p.set_filter("a.x", Some(Use::Include));
        p.set_filter("a.y", Some(Use::Exclude));
        p.set_filter("a.z", Some(Use::Include));
        p.set_filter("a.x", Some(Use::Exclude));
        let s = p.filter_selection();
        assert_eq!(s.include, [Pick::Query("a.z".into())]);
        assert_eq!(s.exclude, [Pick::Query("a.x".into()), Pick::Query("a.y".into())]);
        p.set_filter("a.y", None);
        assert_eq!(p.filter.len(), 2);
        p.set_search("a.y", true);
        p.set_search("a.y", true);
        assert_eq!(p.search_selection().include.len(), 1);
        p.params.insert("cam".into(), ("c(1)".into(), false));
        assert_eq!(p.params()["cam"], ParamValue::literal("c(1)"));
        assert_eq!(p.include_index("a.z"), Some(0));
        assert_eq!(p.include_index("a.x"), None);
        p.toggle_highlight("a.z");
        assert!(p.is_highlighted("a.z"));
        p.toggle_highlight("a.z");
        assert!(!p.is_highlighted("a.z"));
    }
}
