//! The search manager in the GUI: a **Library** pane to browse and edit
//! the packs' queries, and a **Selection** pane with the filter queries in
//! use, in order.
//!
//! Queries get into the selection by dragging them from the library or
//! with their `+`; `−` removes them, the `⠿` handle reorders them. Each
//! node includes in or excludes from the Filter. Its text can be edited in
//! place, new nodes can be written from scratch, and either can be saved
//! into a pack file. Library queries are edited (renamed, deleted, added)
//! in the library pane itself.
//!
//! Searching is an action, not a role: a query's 🔍 menu either replaces
//! the Search box or adds the query to it (OR), so typing in the Search box
//! never leaves stale picks behind.
//!
//! Order matters: included filter queries are OR'ed in this order, and
//! where line colours overlap the later node wins.
//!
//! The panes only edit the selection. Expanding it into NFD is core's job
//! (`nerka_core::pack::expand`): local (new or edited) nodes are put into a
//! temporary pack so they expand exactly like library queries.

use eframe::egui::{self, Color32, RichText};
use nerka_core::pack::{
    self, Expansion, Library, Pack, PackQuery, ParamValue, Params, Pick, Role, Selection, format_query, is_query_name,
    params_in, parse_pack, upsert_query,
};
use std::collections::BTreeMap;

/// How a node takes part in the filter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Use {
    /// Lines it matches are kept (OR'ed with the other includes).
    Include,
    /// Lines it matches are dropped (AND NOT).
    Exclude,
}

impl Use {
    fn symbol(self) -> &'static str {
        match self {
            Use::Include => "+",
            Use::Exclude => "−",
        }
    }

    fn describe(self) -> &'static str {
        match self {
            Use::Include => "+  include in the filter",
            Use::Exclude => "−  exclude from the filter",
        }
    }
}

/// Name of the temporary pack that holds local nodes while expanding.
const LOCAL_PACK: &str = "_sel";

/// One query in the selection, or the query being edited in the library.
#[derive(Debug, Clone)]
pub struct Node {
    /// Stable id in the UI.
    pub key: u64,
    /// The library query it came from (`pack.query`), if any.
    pub source: Option<String>,
    pub name: String,
    /// NFD with `$params`.
    pub text: String,
    /// Qualified parent, kept from the library query.
    pub parent: Option<String>,
    /// Labels as typed: comma-separated, `/` for hierarchy.
    pub labels: String,
    /// A colour name or `#rrggbb`; empty for none.
    pub color: String,
    pub role: Use,
    /// Colour the lines it keeps (included filter queries only).
    pub lit: bool,
    /// Text or name differ from the library query (or there is none).
    pub edited: bool,
    /// Editor open.
    pub open: bool,
    /// Last save error.
    pub error: Option<String>,
}

impl Node {
    fn blank(key: u64, role: Use) -> Node {
        Node {
            key,
            source: None,
            name: format!("new {key}"),
            text: String::new(),
            parent: None,
            labels: String::new(),
            color: String::new(),
            role,
            lit: false,
            edited: true,
            open: true,
            error: None,
        }
    }

    /// A node holding library query `id`.
    fn of_library(key: u64, lib: &Library, id: &str, role: Use) -> Option<Node> {
        let (p, q) = lib.get(id)?;
        Some(Node {
            source: Some(id.to_owned()),
            name: q.id.clone(),
            text: q.text.clone(),
            parent: p.parent_of(q),
            labels: q.labels.join(", "),
            color: q.color.clone().unwrap_or_default(),
            edited: false,
            open: false,
            ..Node::blank(key, role)
        })
    }

    /// The expansion id: the library query, unless edited.
    fn expand_id(&self) -> String {
        match &self.source {
            Some(s) if !self.edited => s.clone(),
            _ => format!("{LOCAL_PACK}.n{}", self.key),
        }
    }

    fn as_query(&self, id: &str) -> PackQuery {
        PackQuery {
            id: id.to_owned(),
            parent: self.parent.clone(),
            labels: self.labels.split(',').map(str::trim).filter(|l| !l.is_empty()).map(str::to_owned).collect(),
            params: params_in(&self.text),
            color: Some(self.color.trim().to_owned()).filter(|c| !c.is_empty()),
            text: self.text.clone(),
            line: 0,
            end_line: 0,
        }
    }

    /// Check the name, labels, colour and text as the pack parser would.
    fn validate(&self) -> Result<(), String> {
        let name = self.name.trim();
        if !is_query_name(name) {
            return Err("Name: anything but . \" # : (spaces are fine)".into());
        }
        upsert_query(name, &format!("pack {LOCAL_PACK}\n"), &self.as_query(name)).map(|_| ()).map_err(|e| e.msg)
    }
}

/// An expansion with, per part, the node it came from, a display name and
/// a colour.
#[derive(Debug, Clone)]
pub struct Applied {
    pub exp: Expansion,
    pub keys: Vec<u64>,
    pub names: Vec<String>,
    pub colors: Vec<Color32>,
}

/// One query of the Search box: its name, NFD and colour.
#[derive(Debug, Clone, PartialEq)]
pub struct Term {
    pub name: String,
    pub nfd: String,
    pub color: Color32,
}

/// A query to search for: from the library, or a selection node.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    Library(String),
    Node(u64),
}

/// What the tab selected, in order, plus parameter values.
#[derive(Debug, Clone, Default)]
pub struct Picks {
    pub nodes: Vec<Node>,
    next_key: u64,
    /// Parameter values by name, with "insert as regex" (raw).
    pub params: BTreeMap<String, (String, bool)>,
    /// The last expansion error, shown in the selection pane.
    pub error: Option<String>,
    /// Pack chosen in the editor's "Save to".
    pub save_pack: String,
    /// Name typed for a new pack file.
    pub new_pack: String,
}

impl Picks {
    fn next_key(&mut self) -> u64 {
        self.next_key += 1;
        self.next_key
    }

    /// Add library query `id` at `at` (the end when `None`). A query
    /// already selected in the same role isn't added twice.
    pub fn add(&mut self, lib: &Library, id: &str, role: Use, at: Option<usize>) -> bool {
        if self.nodes.iter().any(|n| n.source.as_deref() == Some(id) && !n.edited && n.role == role) {
            return false;
        }
        let key = self.next_key();
        let Some(n) = Node::of_library(key, lib, id, role) else { return false };
        let at = at.unwrap_or(self.nodes.len()).min(self.nodes.len());
        self.nodes.insert(at, n);
        true
    }

    /// A new, empty node at the end, with its editor open.
    pub fn add_new(&mut self, role: Use) -> u64 {
        let key = self.next_key();
        self.nodes.push(Node::blank(key, role));
        key
    }

    pub fn remove(&mut self, key: u64) {
        self.nodes.retain(|n| n.key != key);
    }

    /// Move node `key` so it ends up before the node now at `to`
    /// (`to == len` moves it to the end).
    pub fn move_node(&mut self, key: u64, to: usize) {
        let Some(from) = self.nodes.iter().position(|n| n.key == key) else { return };
        let n = self.nodes.remove(from);
        let to = if to > from { to - 1 } else { to };
        self.nodes.insert(to.min(self.nodes.len()), n);
    }

    pub fn node_mut(&mut self, key: u64) -> Option<&mut Node> {
        self.nodes.iter_mut().find(|n| n.key == key)
    }

    pub fn is_lit(&self, key: u64) -> bool {
        self.nodes.iter().any(|n| n.key == key && n.lit)
    }

    pub fn toggle_lit(&mut self, key: u64) {
        if let Some(n) = self.node_mut(key) {
            n.lit = !n.lit;
        }
    }

    /// Switch on the line colour of the node that came from `id`.
    pub fn light_source(&mut self, id: &str) {
        if let Some(n) = self.nodes.iter_mut().find(|n| n.source.as_deref() == Some(id)) {
            n.lit = true;
        }
    }

    /// A library query was renamed: nodes referring to it follow.
    pub fn renamed(&mut self, old: &str, new: &str) {
        for n in &mut self.nodes {
            if n.source.as_deref() == Some(old) {
                n.source = Some(new.to_owned());
                if !n.edited {
                    n.name = new.split_once('.').map_or(new, |(_, q)| q).to_owned();
                }
            }
        }
    }

    /// After the packs changed: unedited nodes take the library's current
    /// text; nodes whose query disappeared become local copies.
    pub fn refresh(&mut self, lib: &Library) {
        for n in &mut self.nodes {
            let Some(id) = n.source.clone() else { continue };
            match lib.get(&id) {
                Some((p, q)) if !n.edited => {
                    n.text = q.text.clone();
                    n.parent = p.parent_of(q);
                    n.labels = q.labels.join(", ");
                    n.color = q.color.clone().unwrap_or_default();
                }
                Some(_) => {}
                None => {
                    n.source = None;
                    n.edited = true;
                }
            }
        }
    }

    pub fn params(&self) -> Params {
        self.params.iter().map(|(k, (v, raw))| (k.clone(), ParamValue { value: v.clone(), raw: *raw })).collect()
    }

    /// The library plus a temporary pack with the local nodes.
    fn with_locals(&self, lib: &Library) -> Result<Library, String> {
        let mut text = format!("pack {LOCAL_PACK}\n");
        for n in self.nodes.iter().filter(|n| n.edited || n.source.is_none()) {
            if let Err(e) = n.validate() {
                return Err(format!("'{}': {e}", n.name));
            }
            text += &format_query(&n.as_query(&format!("n{}", n.key)));
            text.push('\n');
        }
        let local: Pack = parse_pack("selection", &text).map_err(|e| e.to_string())?;
        let mut lib = lib.clone();
        lib.add(local).map_err(|e| e.to_string())?;
        Ok(lib)
    }

    /// Expand the filter nodes.
    pub fn expand(&self, lib: &Library) -> Result<Applied, String> {
        let full = self.with_locals(lib)?;
        let mut sel = Selection::default();
        let mut by_id: Vec<(String, &Node)> = Vec::new();
        for n in &self.nodes {
            let id = n.expand_id();
            match n.role {
                Use::Include => sel.include.push(Pick::Query(id.clone())),
                Use::Exclude => sel.exclude.push(Pick::Query(id.clone())),
            }
            by_id.push((id, n));
        }
        let exp = pack::expand(&sel, &full, &self.params()).map_err(|e| e.to_string())?;
        let (mut keys, mut names, mut colors) = (Vec::new(), Vec::new(), Vec::new());
        let mut includes = 0;
        for p in &exp.parts {
            let n = by_id.iter().find(|(id, _)| *id == p.query).map(|(_, n)| *n).expect("expanded from these nodes");
            keys.push(n.key);
            names.push(n.name.clone());
            let slot = if p.role == Role::Include { includes } else { 0 };
            colors.push(node_color(n, slot));
            if p.role == Role::Include {
                includes += 1;
            }
        }
        Ok(Applied { exp, keys, names, colors })
    }

    /// A query to search for, expanded (parent chain and parameters), with
    /// its colour or the palette's `slot`.
    pub fn search_term(&self, lib: &Library, target: &Target, slot: usize) -> Result<Term, String> {
        let full = self.with_locals(lib)?;
        let (id, name, color) = match target {
            Target::Library(id) => {
                let (_, q) = lib.get(id).ok_or_else(|| format!("unknown query '{id}'"))?;
                (id.clone(), q.id.clone(), q.color.as_deref().and_then(parse_color).unwrap_or(palette(slot)))
            }
            Target::Node(key) => {
                let n = self.nodes.iter().find(|n| n.key == *key).ok_or("the query was removed")?;
                (n.expand_id(), n.name.clone(), node_color(n, slot))
            }
        };
        let sel = Selection { include: vec![Pick::Query(id)], exclude: vec![] };
        let exp = pack::expand(&sel, &full, &self.params()).map_err(|e| e.to_string())?;
        Ok(Term { name, nfd: exp.nfd, color })
    }

    /// Parameter names the selected nodes need, parent chains included.
    fn params_needed(&self, lib: &Library) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for n in &self.nodes {
            let from_parent = n.parent.as_deref().map(|p| lib.params_needed([p])).unwrap_or_default();
            for p in from_parent.into_iter().chain(params_in(&n.text)) {
                if !out.contains(&p) {
                    out.push(p);
                }
            }
        }
        out
    }
}

/// What the user asked for in a pane.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    None,
    Close,
    Reload,
    /// Put the selection into the Filter bar and run it.
    Apply,
    /// Search for a query: replace the Search box, or add to it (OR).
    Search {
        target: Target,
        add: bool,
    },
    /// The library was edited; `renamed` is `(old, new)` qualified ids.
    Edited {
        renamed: Option<(String, String)>,
    },
}

/// A library query being edited (or a new one).
#[derive(Debug, Clone)]
pub struct LibEdit {
    pack: String,
    /// The query's name before editing; `None` for a new query.
    old: Option<String>,
    node: Node,
    /// Delete was clicked once and asks for confirmation.
    confirm_delete: bool,
}

/// Library pane state shared by all tabs.
#[derive(Debug, Default)]
pub struct LibraryView {
    pub by_pack: bool,
    pub editing: Option<LibEdit>,
}

/// Dragged between the panes.
#[derive(Debug, Clone)]
enum Drag {
    Library(String),
    Node(u64),
}

/// Colours for queries without a `color:` in their pack.
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

/// The colour of the `index`-th query when it has none of its own.
pub fn palette(index: usize) -> Color32 {
    PALETTE[index % PALETTE.len()]
}

/// A node's colour: its own, else the palette entry for `slot`.
fn node_color(n: &Node, slot: usize) -> Color32 {
    parse_color(n.color.trim()).unwrap_or(palette(slot))
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

/// The 🔍 menu of a query: replace the search, or add to it.
fn search_menu(ui: &mut egui::Ui, target: Target, action: &mut Action) {
    ui.menu_button("🔍", |ui| {
        if ui.button("Search for it").on_hover_text("Replace what the Search box holds").clicked() {
            *action = Action::Search { target: target.clone(), add: false };
            ui.close();
        }
        if ui
            .button("Add to search (OR)")
            .on_hover_text("Keep what the Search box holds (typed text becomes a query) and add this, OR'ed")
            .clicked()
        {
            *action = Action::Search { target, add: true };
            ui.close();
        }
    })
    .response
    .on_hover_text("Search");
}

/// The Library pane: browse and edit the packs; `+` or drag adds a query
/// to the selection, 🔍 searches for it.
pub fn library_panel(
    ui: &mut egui::Ui,
    lib: &mut Library,
    load_errors: &[String],
    picks: &mut Picks,
    view: &mut LibraryView,
) -> Action {
    let mut action = Action::None;
    ui.horizontal(|ui| {
        ui.heading("Library");
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if crate::close_button(ui).on_hover_text("Close the library and the selection").clicked() {
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
        if let Some(d) = pack::default_dir() {
            ui.weak(format!("Pack folder: {}", d.display()));
        }
        return action;
    }

    // The query being edited, above the list.
    if let Some(edit) = &mut view.editing {
        match library_editor(ui, lib, edit) {
            EditOutcome::Open => {}
            EditOutcome::Closed => view.editing = None,
            EditOutcome::Saved(renamed) => {
                view.editing = None;
                action = Action::Edited { renamed };
            }
        }
        ui.separator();
    }

    ui.weak("+ adds to the selection (or drag it) · 🔍 searches · ✎ edits");
    let lib_ro: &Library = lib;
    let mut edit_request: Option<EditRequest> = None;
    egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
        if view.by_pack {
            for p in lib_ro.packs() {
                let ids: Vec<String> = p.queries.iter().map(|q| format!("{}.{}", p.name, q.id)).collect();
                let header = egui::CollapsingHeader::new(RichText::new(&p.name).strong())
                    .id_salt(("pack", &p.name))
                    .default_open(true)
                    .show(ui, |ui| {
                        if ui
                            .small_button("+ New query")
                            .on_hover_text(format!("Write a new query in {}", p.name))
                            .clicked()
                        {
                            edit_request = Some(EditRequest { pack: p.name.clone(), old: None, parent: None });
                        }
                        group_ui(ui, lib_ro, &ids, picks, &mut action, &mut edit_request);
                    });
                header.header_response.on_hover_text(&p.file);
            }
        } else {
            for label in lib_ro.labels() {
                let ids = lib_ro.with_label(label);
                egui::CollapsingHeader::new(RichText::new(label).strong())
                    .id_salt(("label", label))
                    .default_open(true)
                    .show(ui, |ui| group_ui(ui, lib_ro, &ids, picks, &mut action, &mut edit_request));
            }
            let unlabelled: Vec<String> = lib_ro
                .packs()
                .iter()
                .flat_map(|p| p.queries.iter().filter(|q| q.labels.is_empty()).map(|q| format!("{}.{}", p.name, q.id)))
                .collect();
            if !unlabelled.is_empty() {
                egui::CollapsingHeader::new(RichText::new("(no label)").weak())
                    .id_salt("unlabelled")
                    .default_open(true)
                    .show(ui, |ui| group_ui(ui, lib_ro, &unlabelled, picks, &mut action, &mut edit_request));
            }
            ui.add_space(4.0);
            ui.weak("New queries: ↳ on a query refines it; or group by Pack, then + New query.");
        }
    });
    if let Some(EditRequest { pack, old, parent }) = edit_request {
        let mut node = match &old {
            Some(id) => Node::of_library(0, lib_ro, &format!("{pack}.{id}"), Use::Include).expect("listed query"),
            None => Node { name: String::new(), parent, ..Node::blank(0, Use::Include) },
        };
        node.open = true;
        view.editing = Some(LibEdit { pack, old, node, confirm_delete: false });
    }
    action
}

/// Open the library editor: on query `old` of `pack`, or on a new query
/// (refining `parent`, if given).
struct EditRequest {
    pack: String,
    old: Option<String>,
    parent: Option<String>,
}

/// Queries a query may refine: every library query except `own` (the
/// query being edited) and the queries that refine it, so no chain loops.
pub fn parent_choices(lib: &Library, own: Option<&str>) -> Vec<String> {
    lib.packs()
        .iter()
        .flat_map(|p| p.queries.iter().map(move |q| format!("{}.{}", p.name, q.id)))
        .filter(|id| own.is_none_or(|own| !lib.chain(id).iter().any(|c| c == own)))
        .collect()
}

/// A label's or pack's queries. "+ all" adds them now: a query labelled
/// later doesn't join.
fn group_ui(
    ui: &mut egui::Ui,
    lib: &Library,
    ids: &[String],
    picks: &mut Picks,
    action: &mut Action,
    edit: &mut Option<EditRequest>,
) {
    if ui.small_button("+ all").on_hover_text("Add all of them to the selection").clicked() {
        for id in ids {
            picks.add(lib, id, Use::Include, None);
        }
    }
    for id in ids {
        let Some((pack, q)) = lib.get(id) else { continue };
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            if ui.small_button("+").on_hover_text("Add to the selection (include in the filter)").clicked() {
                picks.add(lib, id, Use::Include, None);
            }
            search_menu(ui, Target::Library(id.clone()), action);
            let color = q.color.as_deref().and_then(parse_color);
            let (rect, _) = ui.allocate_exact_size(egui::vec2(8.0, 12.0), egui::Sense::hover());
            if let Some(c) = color {
                ui.painter().rect_filled(rect, 2.0, c);
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
            ui.dnd_drag_source(ui.scope_id().with(("lib-drag", id)), Drag::Library(id.clone()), |ui| {
                ui.label(name);
            })
            .response
            .on_hover_text(tip);
            ui.weak(&pack.name);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.small_button("✎").on_hover_text("Edit this query in its pack").clicked() {
                    *edit = Some(EditRequest { pack: pack.name.clone(), old: Some(q.id.clone()), parent: None });
                }
                if ui
                    .small_button("↳")
                    .on_hover_text("New query that refines this one (its parent): its meaning is this AND the new text")
                    .clicked()
                {
                    *edit = Some(EditRequest { pack: pack.name.clone(), old: None, parent: Some(id.clone()) });
                }
            });
        });
    }
}

enum EditOutcome {
    Open,
    Closed,
    /// Saved; `(old, new)` qualified ids if it was renamed.
    Saved(Option<(String, String)>),
}

/// Edit a library query in place: Save writes its pack file.
fn library_editor(ui: &mut egui::Ui, lib: &mut Library, e: &mut LibEdit) -> EditOutcome {
    let mut outcome = EditOutcome::Open;
    let title = match &e.old {
        Some(old) => format!("Edit {}.{old}", e.pack),
        None => format!("New query in {}", e.pack),
    };
    ui.label(RichText::new(title).strong());
    let own = e.old.as_ref().map(|o| format!("{}.{o}", e.pack));
    let parents = parent_choices(lib, own.as_deref());
    let valid = fields_ui(ui, &mut e.node, &parents);
    ui.horizontal(|ui| {
        if ui
            .add_enabled(valid.is_ok(), egui::Button::new("Save"))
            .on_hover_text("Write it into the pack file")
            .clicked()
        {
            let name = e.node.name.trim().to_owned();
            let mut q = e.node.as_query(&name);
            if let Some(short) = q.parent.as_deref().and_then(|p| p.strip_prefix(&format!("{}.", e.pack))) {
                q.parent = Some(short.to_owned());
            }
            let res = match &e.old {
                Some(old) => lib.rename_query(&e.pack, old, &q),
                None if lib.get(&format!("{}.{name}", e.pack)).is_some() => {
                    Err(pack::PackError { file: e.pack.clone(), pos: None, msg: format!("'{name}' already exists") })
                }
                None => lib.save_query(&e.pack, &q),
            };
            match res {
                Ok(()) => {
                    let renamed = e
                        .old
                        .as_ref()
                        .filter(|old| **old != name)
                        .map(|old| (format!("{}.{old}", e.pack), format!("{}.{name}", e.pack)));
                    outcome = EditOutcome::Saved(renamed);
                }
                Err(err) => e.node.error = Some(err.to_string()),
            }
        }
        if ui.button("Cancel").clicked() {
            outcome = EditOutcome::Closed;
        }
        if let Some(old) = e.old.clone() {
            let label = if e.confirm_delete { "Really delete?" } else { "Delete" };
            if ui.button(label).on_hover_text("Remove it from the pack file").clicked() {
                if e.confirm_delete {
                    match lib.remove_query(&e.pack, &old) {
                        Ok(()) => outcome = EditOutcome::Saved(None),
                        Err(err) => e.node.error = Some(err.to_string()),
                    }
                } else {
                    e.confirm_delete = true;
                }
            }
        }
    });
    outcome
}

/// Name, labels, colour, parent and NFD text of a query, with validation.
/// `parents` are the queries it may refine.
fn fields_ui(ui: &mut egui::Ui, n: &mut Node, parents: &[String]) -> Result<(), String> {
    egui::Grid::new(("fields", n.key)).num_columns(2).show(ui, |ui| {
        ui.label("Name");
        if ui.add(egui::TextEdit::singleline(&mut n.name).desired_width(f32::INFINITY)).changed() {
            n.edited = true;
        }
        ui.end_row();
        ui.label("Labels");
        if ui
            .add(
                egui::TextEdit::singleline(&mut n.labels)
                    .hint_text("e.g. system/vision, perf")
                    .desired_width(f32::INFINITY),
            )
            .changed()
        {
            n.edited = true;
        }
        ui.end_row();
        ui.label("Colour");
        if ui
            .add(egui::TextEdit::singleline(&mut n.color).hint_text("violet, #ff8000").desired_width(f32::INFINITY))
            .changed()
        {
            n.edited = true;
        }
        ui.end_row();
        ui.label("Refines").on_hover_text("Its parent: the query means the parent AND its own text");
        let before = n.parent.clone();
        egui::ComboBox::from_id_salt(("parent", n.key))
            .width(ui.available_width().max(120.0))
            .selected_text(n.parent.as_deref().unwrap_or("(nothing)"))
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut n.parent, None, "(nothing)");
                for p in parents {
                    ui.selectable_value(&mut n.parent, Some(p.clone()), p);
                }
            });
        if n.parent != before {
            n.edited = true;
        }
        ui.end_row();
    });
    let r = ui.add(
        egui::TextEdit::multiline(&mut n.text)
            .font(egui::TextStyle::Monospace)
            .desired_rows(2)
            .desired_width(f32::INFINITY)
            .hint_text("NFD, e.g. level >= ERROR AND /timeout/   ($name for a parameter)"),
    );
    if r.changed() {
        n.edited = true;
        n.error = None;
    }
    let valid = n.validate();
    match (&n.error, &valid) {
        (Some(e), _) | (None, Err(e)) => {
            ui.colored_label(ui.visuals().error_fg_color, e);
        }
        (None, Ok(())) => {
            ui.weak("✓ valid");
        }
    }
    valid
}

/// The Selection pane: the filter queries in use, in order.
pub fn selection_panel(ui: &mut egui::Ui, lib: &mut Library, picks: &mut Picks) -> Action {
    let mut action = Action::None;
    ui.horizontal(|ui| {
        ui.heading("Selection");
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui.button("+ New").on_hover_text("Write a new query").clicked() {
                picks.add_new(Use::Include);
            }
        });
    });
    ui.weak("Filter queries, in order: ⠿ drags · − removes · ✎ edits · 🔍 searches");
    ui.separator();

    // Bottom part first, so the list gets the space that is left.
    egui::Panel::bottom("selection-actions").show(ui, |ui| {
        params_ui(ui, lib, picks);
        if let Some(e) = &picks.error {
            ui.colored_label(ui.visuals().error_fg_color, e);
        }
        ui.horizontal(|ui| {
            let any = !picks.nodes.is_empty();
            if ui
                .add_enabled(any, egui::Button::new(RichText::new("Apply").strong()))
                .on_hover_text("Put the selection into the Filter bar and run it")
                .clicked()
            {
                action = Action::Apply;
            }
            if ui.add_enabled(any, egui::Button::new("Clear")).on_hover_text("Remove every query").clicked() {
                picks.nodes.clear();
                picks.error = None;
            }
        });
    });

    let mut row_rects: Vec<egui::Rect> = Vec::new();
    let mut remove = None;
    let mut save: Option<u64> = None;
    let mut create_pack = false;
    // The drop zone fills the pane, so a drop anywhere below the list counts.
    let pane_height = ui.available_height();
    let (zone, dropped) = egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            ui.dnd_drop_zone::<Drag, ()>(egui::Frame::new().inner_margin(2.0), |ui| {
                ui.set_min_width(ui.available_width());
                ui.set_min_height(pane_height - 8.0);
                if picks.nodes.is_empty() {
                    ui.weak("Drag queries here from the library, press + there, or + New.");
                }
                let packs: Vec<String> = lib.packs().iter().map(|p| p.name.clone()).collect();
                let (save_pack, new_pack) = (&mut picks.save_pack, &mut picks.new_pack);
                for n in picks.nodes.iter_mut() {
                    row_rects.push(node_row(ui, n, &mut remove, &mut action));
                    if n.open {
                        let parents = parent_choices(lib, n.source.as_deref());
                        match node_editor(ui, n, &packs, &parents, save_pack, new_pack) {
                            Some(EditorAction::Save) => save = Some(n.key),
                            Some(EditorAction::CreatePack) => create_pack = true,
                            None => {}
                        }
                    }
                }
                // Room to drop at the end.
                ui.allocate_space(egui::vec2(ui.available_width(), 28.0));
            })
        })
        .inner;

    // Where a drop would go: before the first row below the pointer.
    let pointer = ui.ctx().pointer_interact_pos();
    let insert_at = pointer.map_or(picks.nodes.len(), |p| row_rects.iter().filter(|r| r.center().y < p.y).count());
    if zone.response.dnd_hover_payload::<Drag>().is_some() {
        let y = row_rects.get(insert_at).map_or_else(
            || row_rects.last().map_or(zone.response.rect.top() + 4.0, |r| r.bottom() + 2.0),
            |r| r.top() - 2.0,
        );
        ui.painter().hline(
            zone.response.rect.x_range(),
            y,
            egui::Stroke::new(2.0, ui.visuals().selection.stroke.color),
        );
    }
    if let Some(d) = dropped {
        match &*d {
            Drag::Library(id) => {
                picks.add(lib, id, Use::Include, Some(insert_at));
            }
            Drag::Node(key) => picks.move_node(*key, insert_at),
        }
    }
    if let Some(key) = remove {
        picks.remove(key);
    }
    if let Some(key) = save {
        if let Some(renamed) = save_node(lib, picks, key) {
            action = Action::Edited { renamed };
        }
    }
    if create_pack {
        let name = picks.new_pack.trim().to_owned();
        let res = pack::default_dir()
            .ok_or_else(|| "no pack folder on this system".to_owned())
            .and_then(|dir| pack::create_pack_file(&dir, &name).map_err(|e| e.to_string()))
            .and_then(|path| lib.load(&path).map_err(|e| e.to_string()));
        match res {
            Ok(()) => {
                picks.save_pack = name;
                picks.new_pack.clear();
                picks.error = None;
            }
            Err(e) => picks.error = Some(e),
        }
    }
    action
}

/// One node: handle, role, colour, name, search, edit and remove. Returns
/// its rect.
fn node_row(ui: &mut egui::Ui, n: &mut Node, remove: &mut Option<u64>, action: &mut Action) -> egui::Rect {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        ui.dnd_drag_source(ui.scope_id().with(("node-drag", n.key)), Drag::Node(n.key), |ui| {
            ui.label(RichText::new("⠿").strong());
        })
        .response
        .on_hover_text("Drag to reorder");
        egui::ComboBox::from_id_salt(("role", n.key)).width(28.0).selected_text(n.role.symbol()).show_ui(ui, |ui| {
            for u in [Use::Include, Use::Exclude] {
                ui.selectable_value(&mut n.role, u, u.describe());
            }
        });
        // Swatch: switches the line colour of an included filter query.
        let color = node_color(n, 0);
        let (rect, r) = ui.allocate_exact_size(egui::vec2(12.0, 14.0), egui::Sense::click());
        let sw = egui::Rect::from_center_size(rect.center(), egui::vec2(10.0, 12.0));
        if n.role == Use::Include {
            if n.lit {
                ui.painter().rect_filled(sw, 2.0, color);
            } else {
                ui.painter().rect_stroke(sw, 2.0, egui::Stroke::new(1.5, color), egui::StrokeKind::Inside);
            }
            let tip = if n.lit {
                "Its lines are coloured: click to stop"
            } else {
                "Colour the lines it keeps (later queries win where lines overlap)"
            };
            if r.on_hover_text(tip).clicked() {
                n.lit = !n.lit;
            }
        }
        let mut name = RichText::new(&n.name);
        if n.edited {
            name = name.italics();
        }
        let tip = match &n.source {
            Some(s) if n.edited => format!("edited copy of {s}\n\n{}", n.text),
            Some(s) => format!("{s}\n\n{}", n.text),
            None => format!("new query (not in a pack)\n\n{}", n.text),
        };
        ui.label(name).on_hover_text(tip);
        if n.edited {
            ui.weak("•").on_hover_text("Not saved in a pack");
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui.small_button("−").on_hover_text("Remove from the selection").clicked() {
                *remove = Some(n.key);
            }
            if ui.selectable_label(n.open, "✎").on_hover_text("Edit").clicked() {
                n.open = !n.open;
            }
            search_menu(ui, Target::Node(n.key), action);
        });
    })
    .response
    .rect
}

enum EditorAction {
    Save,
    CreatePack,
}

/// A selection node's fields and Save to a pack.
fn node_editor(
    ui: &mut egui::Ui,
    n: &mut Node,
    packs: &[String],
    parents: &[String],
    save_pack: &mut String,
    new_pack: &mut String,
) -> Option<EditorAction> {
    let mut action = None;
    egui::Frame::group(ui.style()).show(ui, |ui| {
        let valid = fields_ui(ui, n, parents);
        if save_pack.is_empty()
            && let Some((p, _)) = n.source.as_deref().and_then(|s| s.split_once('.'))
        {
            *save_pack = p.to_owned();
        }
        ui.horizontal(|ui| {
            ui.label("Save to");
            egui::ComboBox::from_id_salt(("save-pack", n.key))
                .selected_text(if save_pack.is_empty() { "pack…" } else { save_pack.as_str() })
                .show_ui(ui, |ui| {
                    for p in packs {
                        ui.selectable_value(save_pack, p.clone(), p);
                    }
                });
            let can = valid.is_ok() && packs.contains(save_pack);
            let tip = format!(
                "Write it into the {save_pack} pack file as '{}' (renames the query it came from if it is in that pack)",
                n.name.trim()
            );
            if ui.add_enabled(can, egui::Button::new("Save")).on_hover_text(tip).clicked() {
                action = Some(EditorAction::Save);
            }
        });
        ui.horizontal(|ui| {
            ui.weak("or a new pack");
            ui.add(egui::TextEdit::singleline(new_pack).hint_text("name").desired_width(90.0));
            if ui
                .add_enabled(!new_pack.trim().is_empty(), egui::Button::new("Create"))
                .on_hover_text("Create <name>.nerka-pack in the pack folder")
                .clicked()
            {
                action = Some(EditorAction::CreatePack);
            }
        });
    });
    action
}

/// Write node `key` into the chosen pack; it then refers to that query. A
/// node from a query of the same pack renames that query; otherwise the
/// query is added (or replaces one with that name). Returns the rename on
/// success, `None` if saving failed.
fn save_node(lib: &mut Library, picks: &mut Picks, key: u64) -> Option<Option<(String, String)>> {
    let pack_name = picks.save_pack.clone();
    let n = picks.node_mut(key)?;
    let name = n.name.trim().to_owned();
    let mut q = n.as_query(&name);
    // A parent in the same pack is written without the pack name.
    if let Some(short) = q.parent.as_deref().and_then(|p| p.strip_prefix(&format!("{pack_name}."))) {
        q.parent = Some(short.to_owned());
    }
    let old = n
        .source
        .as_deref()
        .and_then(|s| s.split_once('.'))
        .filter(|(p, _)| *p == pack_name)
        .map(|(_, q)| q.to_owned())
        .filter(|old| lib.get(&format!("{pack_name}.{old}")).is_some());
    let res = match &old {
        Some(old) => lib.rename_query(&pack_name, old, &q),
        None => lib.save_query(&pack_name, &q),
    };
    match res {
        Ok(()) => {
            n.source = Some(format!("{pack_name}.{name}"));
            n.name = name.clone();
            n.edited = false;
            n.error = None;
            n.open = false;
            Some(old.filter(|o| *o != name).map(|o| (format!("{pack_name}.{o}"), format!("{pack_name}.{name}"))))
        }
        Err(e) => {
            n.error = Some(e.to_string());
            None
        }
    }
}

/// Fields for the parameters the selected queries need.
fn params_ui(ui: &mut egui::Ui, lib: &Library, picks: &mut Picks) {
    let names = picks.params_needed(lib);
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

    fn lib() -> Library {
        let mut lib = Library::new();
        let text = "pack v\nquery scope(ecu)\n  ecu == $ecu\nquery drops  parent: scope  color: violet\n  /drop/\nquery slow\n  /slow/\n";
        lib.add(parse_pack("v.nerka-pack", text).unwrap()).unwrap();
        lib
    }

    /// One frame: the library on the right, the selection in the centre.
    fn frame(ctx: &egui::Context, t: f64, events: Vec<egui::Event>, lib: &mut Library, picks: &mut Picks) {
        let screen = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(1000.0, 600.0));
        let input = egui::RawInput { screen_rect: Some(screen), time: Some(t), events, ..Default::default() };
        let mut out = ctx.run_ui(input, |ui| {
            egui::Panel::right("library").exact_size(300.0).show(ui, |ui| {
                library_panel(ui, &mut lib.clone(), &[], picks, &mut LibraryView::default());
            });
            egui::CentralPanel::default().show(ui, |ui| {
                selection_panel(ui, lib, picks);
            });
        });
        out.textures_delta.clear();
    }

    fn button(pos: egui::Pos2, pressed: bool) -> egui::Event {
        egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        }
    }

    #[test]
    fn drag_from_the_library_into_the_selection_and_reorder() {
        let ctx = egui::Context::default();
        let (mut lib, mut picks) = (lib(), Picks::default());
        let mut t = 0.0;
        let mut step = |events, lib: &mut Library, picks: &mut Picks| {
            t += 1.0 / 60.0;
            frame(&ctx, t, events, lib, picks);
        };
        let drag = |from: egui::Pos2, to: egui::Pos2| {
            [
                vec![egui::Event::PointerMoved(from), button(from, true)],
                vec![egui::Event::PointerMoved(from + egui::vec2(12.0, 0.0))],
                vec![egui::Event::PointerMoved(to)],
                vec![egui::Event::PointerMoved(to)],
                vec![button(to, false)],
                vec![],
            ]
        };
        step(vec![], &mut lib, &mut picks);
        // Find a library query's name by probing down the library pane: a
        // press and a small move there starts a drag.
        let mut found = None;
        'probe: for x in [770, 790, 810] {
            for y in (40..400).step_by(3) {
                let p = egui::pos2(x as f32, y as f32);
                step(vec![egui::Event::PointerMoved(p), button(p, true)], &mut lib, &mut picks);
                step(vec![egui::Event::PointerMoved(p + egui::vec2(12.0, 0.0))], &mut lib, &mut picks);
                let dragging = egui::DragAndDrop::has_any_payload(&ctx);
                step(vec![button(p, false)], &mut lib, &mut picks);
                step(vec![], &mut lib, &mut picks);
                if dragging {
                    found = Some(p);
                    break 'probe;
                }
            }
        }
        let from = found.expect("a draggable library query");
        assert!(picks.nodes.is_empty(), "releasing in the library adds nothing");
        for events in drag(from, egui::pos2(300.0, 150.0)) {
            step(events, &mut lib, &mut picks);
        }
        assert_eq!(picks.nodes.len(), 1, "dropped into the selection");
        let first = picks.nodes[0].name.clone();
        // Add another, then drag the second node above the first by its
        // handle.
        let other = if first == "slow" { "v.drops" } else { "v.slow" };
        picks.add(&lib, other, Use::Include, None);
        step(vec![], &mut lib, &mut picks);
        let mut moved = false;
        for y in (60..300).step_by(3) {
            let p = egui::pos2(12.0, y as f32);
            for events in drag(p, egui::pos2(12.0, 70.0)) {
                step(events, &mut lib, &mut picks);
            }
            if picks.nodes[0].name != first {
                moved = true;
                break;
            }
        }
        assert!(moved, "the second node was dragged above the first");
        assert_eq!(picks.nodes.len(), 2);
    }

    #[test]
    fn names_labels_and_colours_are_checked_and_saved() {
        let mut p = Picks::default();
        let k = p.add_new(Use::Include);
        let n = p.node_mut(k).unwrap();
        n.text = "apid == \"xyz\"".into();
        n.name = "xyz.logs".into();
        assert!(n.validate().unwrap_err().contains("anything but"));
        n.name = " xyz logs ".into();
        n.labels = "aurix, system/ecu ,".into();
        n.color = "Teal".into();
        assert!(n.validate().unwrap_err().contains("invalid colour"));
        n.color = "teal".into();
        n.labels = "a b".into();
        assert!(n.validate().unwrap_err().contains("separated by commas"));
        n.labels = "aurix, system/ecu ,".into();
        n.validate().unwrap();

        let dir = std::env::temp_dir().join(format!("nerka-gui-labels-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("a.nerka-pack");
        std::fs::write(&file, "pack a\n").unwrap();
        let mut lib = Library::new();
        lib.load(&file).unwrap();
        p.save_pack = "a".into();
        assert_eq!(save_node(&mut lib, &mut p, k), Some(None));
        assert_eq!(p.nodes[0].error, None);
        assert_eq!(
            std::fs::read_to_string(&file).unwrap(),
            "pack a\n\nquery \"xyz logs\"  labels: aurix, system/ecu  color: teal\n    apid == \"xyz\"\n"
        );
        assert_eq!(lib.with_label("system"), ["a.xyz logs"]);
        // Saving it again under another name renames it in the pack.
        p.nodes[0].name = "xyz all".into();
        p.nodes[0].edited = true;
        assert_eq!(save_node(&mut lib, &mut p, k), Some(Some(("a.xyz logs".into(), "a.xyz all".into()))));
        let ids: Vec<&str> = lib.packs()[0].queries.iter().map(|q| q.id.as_str()).collect();
        assert_eq!(ids, ["xyz all"]);
        // A library node shows its labels and colour as text.
        let mut q = Picks::default();
        q.add(&lib, "a.xyz all", Use::Include, None);
        assert_eq!((q.nodes[0].labels.as_str(), q.nodes[0].color.as_str()), ("aurix, system/ecu", "teal"));
        q.renamed("a.xyz all", "a.renamed");
        assert_eq!((q.nodes[0].source.as_deref(), q.nodes[0].name.as_str()), (Some("a.renamed"), "renamed"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn library_editor_edits_renames_and_deletes() {
        let dir = std::env::temp_dir().join(format!("nerka-gui-libedit-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("v.nerka-pack");
        std::fs::write(&file, "pack v\n\nquery slow\n  /slow/\n\nquery fast\n  /fast/\n").unwrap();
        let mut lib = Library::new();
        lib.load(&file).unwrap();
        let ctx = egui::Context::default();
        // Render the editor, find a button by its text, click it; return
        // what the editor reported on the release frame.
        let run = |e: &mut LibEdit, lib: &mut Library, click: &str| {
            let frame = |events: Vec<egui::Event>, e: &mut LibEdit, lib: &mut Library| {
                let mut out = EditOutcome::Open;
                let input = egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(400.0, 400.0))),
                    events,
                    ..Default::default()
                };
                let mut full = ctx.run_ui(input, |ui| {
                    egui::CentralPanel::default().show(ui, |ui| out = library_editor(ui, lib, e));
                });
                full.textures_delta.clear();
                (out, full.shapes)
            };
            let (_, shapes) = frame(vec![], e, lib);
            let p = find_text(&shapes, click).unwrap_or_else(|| panic!("no '{click}' button"));
            frame(vec![egui::Event::PointerMoved(p), button(p, true)], e, lib);
            frame(vec![button(p, false)], e, lib).0
        };
        let mut e = LibEdit {
            pack: "v".into(),
            old: Some("slow".into()),
            node: Node::of_library(0, &lib, "v.slow", Use::Include).unwrap(),
            confirm_delete: false,
        };
        e.node.name = "very slow".into();
        e.node.text = "/very slow/".into();
        assert!(
            matches!(run(&mut e, &mut lib, "Save"), EditOutcome::Saved(Some((ref o, ref n))) if o == "v.slow" && n == "v.very slow")
        );
        assert_eq!(
            std::fs::read_to_string(&file).unwrap(),
            "pack v\n\nquery \"very slow\"\n    /very slow/\n\nquery fast\n  /fast/\n"
        );
        // A new query can't take an existing name.
        let mut e = LibEdit { pack: "v".into(), old: None, node: Node::blank(0, Use::Include), confirm_delete: false };
        e.node.name = "fast".into();
        e.node.text = "/x/".into();
        assert!(matches!(run(&mut e, &mut lib, "Save"), EditOutcome::Open));
        assert!(e.node.error.as_deref().unwrap().contains("already exists"));
        // Delete asks first.
        let mut e = LibEdit {
            pack: "v".into(),
            old: Some("fast".into()),
            node: Node::of_library(0, &lib, "v.fast", Use::Include).unwrap(),
            confirm_delete: false,
        };
        assert!(matches!(run(&mut e, &mut lib, "Delete"), EditOutcome::Open));
        assert!(e.confirm_delete);
        assert!(matches!(run(&mut e, &mut lib, "Really delete?"), EditOutcome::Saved(None)));
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "pack v\n\nquery \"very slow\"\n    /very slow/\n");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Centre of the first text shape that reads `label`.
    fn find_text(shapes: &[egui::epaint::ClippedShape], label: &str) -> Option<egui::Pos2> {
        fn walk(shape: &egui::Shape, label: &str) -> Option<egui::Pos2> {
            match shape {
                egui::Shape::Text(t) if t.galley.text() == label => Some(t.visual_bounding_rect().center()),
                egui::Shape::Vec(v) => v.iter().find_map(|s| walk(s, label)),
                _ => None,
            }
        }
        shapes.iter().find_map(|c| walk(&c.shape, label))
    }

    #[test]
    fn refining_a_query_from_the_library() {
        let lib = lib();
        // No loops: a query can't refine itself or a query refining it.
        let all = parent_choices(&lib, None);
        assert_eq!(all, ["v.scope", "v.drops", "v.slow"]);
        assert_eq!(parent_choices(&lib, Some("v.scope")), ["v.slow"]);
        assert_eq!(parent_choices(&lib, Some("v.drops")), ["v.scope", "v.slow"]);

        // A new child of v.slow, saved from the library editor: the parent
        // is written short (same pack), and expanding it ANDs the chain.
        let dir = std::env::temp_dir().join(format!("nerka-gui-refine-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("v.nerka-pack");
        std::fs::write(&file, "pack v\nquery slow\n  /slow/\n").unwrap();
        let mut lib = Library::new();
        lib.load(&file).unwrap();
        let ctx = egui::Context::default();
        let mut e = LibEdit {
            pack: "v".into(),
            old: None,
            node: Node { name: "very slow".into(), parent: Some("v.slow".into()), ..Node::blank(0, Use::Include) },
            confirm_delete: false,
        };
        e.node.text = "level >= WARN".into();
        let frame = |events: Vec<egui::Event>, e: &mut LibEdit, lib: &mut Library| {
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(400.0, 400.0))),
                events,
                ..Default::default()
            };
            let mut out = EditOutcome::Open;
            let mut full = ctx.run_ui(input, |ui| {
                egui::CentralPanel::default().show(ui, |ui| out = library_editor(ui, lib, e));
            });
            full.textures_delta.clear();
            (out, full.shapes)
        };
        let (_, shapes) = frame(vec![], &mut e, &mut lib);
        let p = find_text(&shapes, "Save").unwrap();
        frame(vec![egui::Event::PointerMoved(p), button(p, true)], &mut e, &mut lib);
        assert!(matches!(frame(vec![button(p, false)], &mut e, &mut lib).0, EditOutcome::Saved(None)));
        assert_eq!(
            std::fs::read_to_string(&file).unwrap(),
            "pack v\nquery slow\n  /slow/\n\nquery \"very slow\"  parent: slow\n    level >= WARN\n"
        );
        let mut p = Picks::default();
        p.add(&lib, "v.very slow", Use::Include, None);
        assert_eq!(p.expand(&lib).unwrap().exp.nfd, "(/slow/) AND (level >= WARN)");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn colours() {
        assert_eq!(parse_color("#ff8000"), Some(Color32::from_rgb(255, 128, 0)));
        assert_eq!(parse_color("#ff80"), None);
        assert_eq!(parse_color("violet"), parse_color("purple"));
        assert_eq!(parse_color("plaid"), None);
    }

    #[test]
    fn order_add_move_remove() {
        let lib = lib();
        let mut p = Picks::default();
        assert!(p.add(&lib, "v.drops", Use::Include, None));
        assert!(p.add(&lib, "v.slow", Use::Include, None));
        assert!(!p.add(&lib, "v.slow", Use::Include, None), "not twice in one role");
        assert!(p.add(&lib, "v.slow", Use::Exclude, Some(0)));
        assert!(!p.add(&lib, "v.nope", Use::Include, None));
        let names = |p: &Picks| p.nodes.iter().map(|n| (n.name.clone(), n.role)).collect::<Vec<_>>();
        assert_eq!(names(&p)[0], ("slow".into(), Use::Exclude));
        let first = p.nodes[0].key;
        p.move_node(first, 3);
        assert_eq!(names(&p)[2], ("slow".into(), Use::Exclude));
        p.move_node(first, 0);
        assert_eq!(p.nodes[0].key, first);
        p.remove(first);
        assert_eq!(p.nodes.len(), 2);
    }

    #[test]
    fn expansion_follows_the_order_and_local_nodes_expand_too() {
        let lib = lib();
        let mut p = Picks::default();
        p.add(&lib, "v.slow", Use::Include, None);
        p.add(&lib, "v.drops", Use::Include, None);
        p.params.insert("ecu".into(), ("E1".into(), false));
        let a = p.expand(&lib).unwrap();
        assert_eq!(a.exp.nfd, r#"(/slow/) OR (ecu == "E1") AND (/drop/)"#);
        assert_eq!(a.names, ["slow", "drops"]);
        assert_eq!(a.colors, [palette(0), parse_color("violet").unwrap()]);
        // Reordered: the expansion follows.
        let k = p.nodes[1].key;
        p.move_node(k, 0);
        assert_eq!(p.expand(&lib).unwrap().exp.nfd, r#"(ecu == "E1") AND (/drop/) OR (/slow/)"#);

        // An edited library node and a new one, excluded.
        p.nodes[0].text = "/dropped $what/".into();
        p.nodes[0].edited = true;
        let new = p.add_new(Use::Exclude);
        p.node_mut(new).unwrap().text = "level <= DEBUG".into();
        p.params.insert("what".into(), ("a.b".into(), false));
        let a = p.expand(&lib).unwrap();
        assert_eq!(a.exp.nfd, r#"((ecu == "E1") AND (/dropped a\.b/) OR (/slow/)) AND NOT (level <= DEBUG)"#);
        assert_eq!(a.keys.len(), 3);
        assert_eq!(p.params_needed(&lib), ["ecu", "what"]);

        // Search terms: a library query (parent chain, parameters) or a node.
        let t = p.search_term(&lib, &Target::Library("v.drops".into()), 3).unwrap();
        assert_eq!(
            t,
            Term {
                name: "drops".into(),
                nfd: r#"(ecu == "E1") AND (/drop/)"#.into(),
                color: parse_color("violet").unwrap()
            }
        );
        let t = p.search_term(&lib, &Target::Node(new), 2).unwrap();
        assert_eq!((t.nfd.as_str(), t.color), ("(level <= DEBUG)", palette(2)));

        // Invalid local text is reported with the node's name.
        p.node_mut(new).unwrap().text = "level >= LOUD".into();
        let e = p.expand(&lib).unwrap_err();
        assert!(e.contains("unknown level"), "{e}");
    }

    #[test]
    fn refreshing_follows_the_library() {
        let dir = std::env::temp_dir().join(format!("nerka-gui-save-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("v.nerka-pack");
        std::fs::write(&file, "pack v\nquery fast\n  /fast/\nquery slow\n  /slow/\n").unwrap();
        let mut lib = Library::new();
        lib.load(&file).unwrap();
        let mut p = Picks::default();
        p.add(&lib, "v.fast", Use::Include, None);
        p.add(&lib, "v.slow", Use::Include, None);
        p.nodes[1].text = "/mine/".into();
        p.nodes[1].edited = true;
        std::fs::write(&file, "pack v\nquery fast\n  /quick/\n").unwrap();
        let mut lib = Library::new();
        lib.load(&file).unwrap();
        p.refresh(&lib);
        assert_eq!(p.nodes[0].text, "/quick/");
        assert_eq!(p.nodes[1].text, "/mine/");
        // v.slow is gone: the edited node stays, as a local query.
        assert_eq!(p.nodes[1].source, None);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
