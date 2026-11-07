//! Settings bundles in the GUI (owner, 2026-11-07; `doc/bundles.md`):
//! ⚙ Settings → Settings bundle → Export… lists the analysis setup of the
//! config folder with a checkbox each, then saves the ticked ones; Import…
//! reads a bundle and shows the plan, a row per item with its status and a
//! choice (Auto: add new ones, keep both where they differ), before
//! anything is written. The logic is `nerka_core::bundle`.

use eframe::egui::{self, RichText};
use nerka_core::bundle::{BUNDLE_EXTENSION, Bundle, Item, Kind, Plan, Status, Store};
use std::path::PathBuf;

/// The Export window: every item of the config folder, ticked or not.
pub struct ExportView {
    pub items: Vec<(Item, bool)>,
    /// Files that couldn't be read (not exported).
    pub errors: Vec<String>,
}

impl ExportView {
    /// The config folder's items, all ticked.
    pub fn of(store: &Store) -> ExportView {
        let (items, errors) = store.items();
        ExportView { items: items.into_iter().map(|s| (s.item, true)).collect(), errors }
    }
}

/// The Import window: a bundle's plan.
pub struct ImportView {
    pub file: PathBuf,
    pub nerka: Option<String>,
    pub plan: Plan,
}

impl ImportView {
    /// Ask for a bundle and plan its import into `store`; the error says why not.
    pub fn pick(store: &Store) -> Option<Result<ImportView, String>> {
        let file = rfd::FileDialog::new()
            .add_filter("Nerka settings bundles", &[BUNDLE_EXTENSION])
            .add_filter("All files", &["*"])
            .pick_file()?;
        Some(Self::read(store, file))
    }

    pub fn read(store: &Store, file: PathBuf) -> Result<ImportView, String> {
        let text = std::fs::read_to_string(&file).map_err(|e| format!("{}: {e}", file.display()))?;
        let bundle = Bundle::parse(&file.display().to_string(), &text).map_err(|e| e.to_string())?;
        Ok(ImportView { file, nerka: bundle.nerka.clone(), plan: store.plan(&bundle) })
    }
}

pub enum Outcome {
    Open,
    Close,
    /// Export: the ticked items. Import: the plan as chosen.
    Go,
}

/// The Export window.
pub fn export_window(ctx: &egui::Context, v: &mut ExportView) -> Outcome {
    let mut open = true;
    let mut out = Outcome::Open;
    egui::Window::new("Export settings")
        .id(egui::Id::unique("bundle-export"))
        .open(&mut open)
        .collapsible(false)
        .default_width(360.0)
        .show(ctx, |ui| {
            ui.label("The analysis setup to put into a settings bundle:");
            ui.horizontal(|ui| {
                if ui.small_button("All").clicked() {
                    v.items.iter_mut().for_each(|i| i.1 = true);
                }
                if ui.small_button("None").clicked() {
                    v.items.iter_mut().for_each(|i| i.1 = false);
                }
            });
            ui.separator();
            egui::ScrollArea::vertical().max_height(360.0).auto_shrink([false, true]).show(ui, |ui| {
                if v.items.is_empty() {
                    ui.weak("Nothing saved yet: no packs, presets, plots, checks or signals in the config folder.");
                }
                for kind in Kind::ALL {
                    let of_kind: Vec<usize> = (0..v.items.len()).filter(|&i| v.items[i].0.kind == kind).collect();
                    if of_kind.is_empty() {
                        continue;
                    }
                    let all = of_kind.iter().all(|&i| v.items[i].1);
                    let mut ticked = all;
                    let label = RichText::new(format!("{} ({})", capital(kind.plural()), of_kind.len())).strong();
                    if ui.checkbox(&mut ticked, label).changed() {
                        of_kind.iter().for_each(|&i| v.items[i].1 = ticked);
                    }
                    ui.indent(kind.as_str(), |ui| {
                        for &i in &of_kind {
                            let (item, on) = &mut v.items[i];
                            ui.checkbox(on, &item.name);
                        }
                    });
                }
            });
            for e in &v.errors {
                ui.colored_label(ui.visuals().warn_fg_color, format!("Not exported: {e}"));
            }
            ui.separator();
            let n = v.items.iter().filter(|i| i.1).count();
            ui.horizontal(|ui| {
                let save = ui.add_enabled(n > 0, egui::Button::new(format!("Save {n} item{}…", plural(n))));
                if save.on_hover_text("Choose where to save the bundle (.nerka-bundle)").clicked() {
                    out = Outcome::Go;
                }
                if ui.button("Cancel").clicked() {
                    out = Outcome::Close;
                }
            });
        });
    if !open { Outcome::Close } else { out }
}

/// Save the ticked items of `v` where the user picks; the status to show,
/// or none if the dialog was cancelled.
pub fn save_export(v: &ExportView) -> Option<String> {
    let path = rfd::FileDialog::new()
        .add_filter("Nerka settings bundles", &[BUNDLE_EXTENSION])
        .set_file_name(format!("nerka-settings.{BUNDLE_EXTENSION}"))
        .save_file()?;
    let items: Vec<Item> = v.items.iter().filter(|i| i.1).map(|i| i.0.clone()).collect();
    let n = items.len();
    Some(match nerka_core::write_atomic(&path, Bundle::new(items).to_text()) {
        Ok(()) => format!("Exported {n} item{} to {}", plural(n), path.display()),
        Err(e) => format!("Could not export to {}: {e}", path.display()),
    })
}

/// The Import window: the plan, a row per item, and a choice where there is one.
pub fn import_window(ctx: &egui::Context, v: &mut ImportView) -> Outcome {
    let mut open = true;
    let mut out = Outcome::Open;
    egui::Window::new("Import settings")
        .id(egui::Id::unique("bundle-import"))
        .open(&mut open)
        .collapsible(false)
        .default_width(520.0)
        .show(ctx, |ui| {
            let name = v.file.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            let from = v.nerka.as_ref().map(|n| format!(" (made by nerka {n})")).unwrap_or_default();
            ui.label(format!("{name}{from}")).on_hover_text(v.file.display().to_string());
            ui.separator();
            egui::ScrollArea::vertical().max_height(400.0).auto_shrink([false, true]).show(ui, |ui| {
                if v.plan.items.is_empty() {
                    ui.weak("The bundle has no items.");
                }
                egui::Grid::new("bundle-plan").num_columns(4).spacing([12.0, 4.0]).striped(true).show(ui, |ui| {
                    ui.label(RichText::new("Kind").strong());
                    ui.label(RichText::new("Name").strong());
                    ui.label(RichText::new("Status").strong());
                    ui.label(RichText::new("Import").strong());
                    ui.end_row();
                    for (i, p) in v.plan.items.iter_mut().enumerate() {
                        ui.label(p.kind.as_str());
                        ui.label(&p.name);
                        let status = RichText::new(p.status_text());
                        match &p.status {
                            Status::Invalid(why) => {
                                ui.label(status.color(ui.visuals().error_fg_color)).on_hover_text(why);
                            }
                            Status::Different => {
                                ui.label(status.color(ui.visuals().warn_fg_color))
                                    .on_hover_text("You have one of that name, with other contents");
                            }
                            Status::Same => {
                                ui.weak(p.status_text()).on_hover_text("You have it as it is");
                            }
                            Status::New => {
                                ui.label(status);
                            }
                        }
                        if let Status::Invalid(why) = &p.status {
                            ui.colored_label(ui.visuals().error_fg_color, why);
                        } else if p.choices().len() > 1 {
                            let mut c = p.choice;
                            egui::ComboBox::from_id_salt(("bundle-choice", i)).selected_text(p.choice_text(c)).show_ui(
                                ui,
                                |ui| {
                                    for &o in p.choices() {
                                        ui.selectable_value(&mut c, o, p.choice_text(o));
                                    }
                                },
                            );
                            p.choice = c;
                        } else {
                            ui.weak(p.choice_text(p.choice));
                        }
                        ui.end_row();
                    }
                });
            });
            for n in &v.plan.notes {
                ui.colored_label(ui.visuals().warn_fg_color, n);
            }
            ui.separator();
            let n = v.plan.changes();
            ui.horizontal(|ui| {
                let go = ui.add_enabled(n > 0, egui::Button::new(format!("Import {n} item{}", plural(n))));
                if go.on_hover_text("Write them into the config folder; what is replaced is backed up first").clicked()
                {
                    out = Outcome::Go;
                }
                let auto = ui.button("Auto").on_hover_text(
                    "Add the new ones, keep both where they differ (theirs renamed); nothing is overwritten",
                );
                if auto.clicked() {
                    v.plan.set_auto();
                }
                if ui.button("Cancel").clicked() {
                    out = Outcome::Close;
                }
            });
        });
    if !open { Outcome::Close } else { out }
}

fn plural(n: usize) -> &'static str {
    if n == 1 { "" } else { "s" }
}

fn capital(s: &str) -> String {
    let mut c = s.chars();
    c.next().map(|f| f.to_uppercase().chain(c).collect()).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use nerka_core::bundle::Choice;

    fn texts(shapes: &[egui::epaint::ClippedShape]) -> Vec<(String, egui::Rect)> {
        fn walk(s: &egui::Shape, clip: egui::Rect, out: &mut Vec<(String, egui::Rect)>) {
            match s {
                egui::Shape::Text(t) => {
                    let r = t.visual_bounding_rect().intersect(clip);
                    if r.is_positive() {
                        out.push((t.galley.text().to_owned(), r));
                    }
                }
                egui::Shape::Vec(v) => v.iter().for_each(|s| walk(s, clip, out)),
                _ => {}
            }
        }
        let mut out = Vec::new();
        shapes.iter().for_each(|c| walk(&c.shape, c.clip_rect, &mut out));
        out
    }

    /// A frame of the Import window: its outcome and texts.
    fn frame(
        ctx: &egui::Context,
        t: f64,
        events: Vec<egui::Event>,
        v: &mut ImportView,
    ) -> (bool, Vec<(String, egui::Rect)>) {
        let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1000.0, 700.0));
        let input = egui::RawInput { screen_rect: Some(screen), time: Some(t), events, ..Default::default() };
        let mut go = false;
        let mut out = ctx.run_ui(input, |ui| {
            go = matches!(import_window(ui.ctx(), v), Outcome::Go);
        });
        out.textures_delta.clear();
        (go, texts(&out.shapes))
    }

    fn click(at: egui::Pos2) -> Vec<Vec<egui::Event>> {
        let b = |pressed| egui::Event::PointerButton {
            pos: at,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };
        vec![vec![egui::Event::PointerMoved(at)], vec![b(true)], vec![b(false)]]
    }

    #[test]
    fn the_import_window_shows_the_plan_and_auto_resets_it() {
        let d = std::env::temp_dir().join(format!("nerka-gui-bundle-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let store = Store::new(d.join("nerka"));
        std::fs::create_dir_all(store.root()).unwrap();
        std::fs::write(store.root().join("checks"), "storm: lines level >= ERROR; exists\n").unwrap();
        let file = d.join("theirs.nerka-bundle");
        std::fs::write(
            &file,
            "nerka-bundle 1\nnerka 0.1.0\n@@ check storm format 1\nstorm: lines level >= WARN; exists\n\
             @@ pack vision format 1\npack vision\n@@ check bad format 1\nbad: lines level >= LOUD; exists\n",
        )
        .unwrap();
        let mut v = ImportView::read(&store, file).unwrap();
        v.plan.items[0].choice = Choice::Take;
        let ctx = egui::Context::default();
        let mut t = 0.0;
        let mut shown = Vec::new();
        for _ in 0..3 {
            t += 0.1;
            shown = frame(&ctx, t, vec![], &mut v).1;
        }
        fn at(shown: &[(String, egui::Rect)], s: &str) -> Option<egui::Pos2> {
            shown.iter().find(|(x, _)| x == s).map(|(_, r)| r.center())
        }
        for s in ["theirs.nerka-bundle (made by nerka 0.1.0)", "different", "take theirs", "new", "invalid", "add"] {
            assert!(at(&shown, s).is_some(), "'{s}' isn't shown: {:?}", shown.iter().map(|x| &x.0).collect::<Vec<_>>());
        }
        assert!(shown.iter().any(|(x, _)| x.contains("unknown level 'LOUD'")), "the reason shows");
        // Auto: keep both again.
        for e in click(at(&shown, "Auto").unwrap()) {
            t += 0.1;
            frame(&ctx, t, e, &mut v);
        }
        assert_eq!(v.plan.items[0].choice, Choice::Both);
        shown = frame(&ctx, t + 0.1, vec![], &mut v).1;
        assert!(at(&shown, "keep both (as storm_imported)").is_some());
        // Import 2 items.
        let mut go = false;
        for e in click(at(&shown, "Import 2 items").unwrap()) {
            t += 0.1;
            go |= frame(&ctx, t, e, &mut v).0;
        }
        assert!(go);
    }
}
