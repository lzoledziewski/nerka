//! What a tab's files couldn't be read, one by one: the ⚠ in the top bar
//! opens this list (the counts alone said how much, not where). A row
//! jumps to the line after it; **Export…** reads the files again and
//! writes every skip, not only those listed, with what a bug report needs
//! (`nerka_core::input::export_skips`).

use crate::job::{Job, Poll};
use eframe::egui::{self, RichText};
use nerka_core::input::{Skip, Skips};
use nerka_core::{LineNo, SourceInfo};
use std::path::PathBuf;

/// The list's state, per tab.
#[derive(Default)]
pub struct SkipsUi {
    pub open: bool,
    selected: Option<usize>,
    /// An export running: where it writes.
    export: Option<(Job<u64>, PathBuf)>,
}

/// What the list asks of its tab.
pub enum Asked {
    None,
    Goto(LineNo),
    /// Export to this file.
    Export(PathBuf),
}

impl SkipsUi {
    /// An export started.
    pub fn exporting(&mut self, job: Job<u64>, to: PathBuf) {
        self.export = Some((job, to));
    }

    /// An export that finished: what to say.
    pub fn poll(&mut self) -> Option<String> {
        let (job, to) = self.export.as_ref()?;
        let Poll::Done(r, took) = job.poll() else { return None };
        let said = match r {
            Ok(n) => format!("{n} skips exported to {} in {:.1} s", to.display(), took.as_secs_f64()),
            Err(nerka_core::Error::Cancelled) => "Export of the skips cancelled".into(),
            Err(e) => format!("Export of the skips to {} failed: {e}", to.display()),
        };
        self.export = None;
        Some(said)
    }
}

/// One row: kind, line, time, frame, bytes, detail (monospace, aligned).
fn row_text(s: &Skip) -> String {
    let opt = |v: Option<u64>| v.map_or_else(|| "–".into(), |v| v.to_string());
    let time = s.time.map_or_else(|| "–".into(), |t| nerka_core::time::format_time(t, 6));
    let file = if s.file.is_empty() { String::new() } else { format!("{}: ", s.file) };
    format!(
        "{:<18} {:>9} {:<26} {:>8} {:>7}  {file}{}",
        s.kind.text(),
        opt(s.line.map(|l| l + 1)),
        time,
        opt(s.frame),
        s.bytes,
        s.detail
    )
}

fn tip(s: &Skip) -> String {
    let mut t = format!("{}, {} bytes", s.kind.text(), s.bytes);
    if let Some(o) = s.offset {
        t += &format!("\nat byte {o} of the file");
    }
    if let Some(f) = s.frame {
        t += &format!("\nframe {f}: in Wireshark, frame.number == {f}");
    }
    if !s.head.is_empty() {
        let hex: Vec<String> = s.head.iter().map(|b| format!("{b:02x}")).collect();
        t += &format!("\nstarts with: {}", hex.join(" "));
    }
    if !s.detail.is_empty() {
        t += &format!("\n{}", s.detail);
    }
    t += match s.line {
        Some(_) => "\nClick: go to the line after it",
        None => "\nAfter the last line",
    };
    t
}

/// The window, for a tab titled `title` whose files say `info`.
pub fn window(ctx: &egui::Context, state: &mut SkipsUi, title: &str, info: &SourceInfo, file_name: &str) -> Asked {
    let mut asked = Asked::None;
    if !state.open {
        return asked;
    }
    let mut open = true;
    egui::Window::new(format!("Not read · {title}"))
        .id(egui::Id::unique("skips-window"))
        .open(&mut open)
        .default_width(820.0)
        .default_height(420.0)
        .show(ctx, |ui| {
            for n in &info.notes {
                ui.label(format!("⚠ {n}"));
            }
            if info.notes.is_empty() {
                ui.label("Everything in these files was read.");
            }
            // Whether the capture itself dropped packets: lost before it, or by it.
            for a in &info.about {
                ui.weak(a);
            }
            let listed = info.skips.len();
            let mut said = format!("{listed} listed");
            if info.skips_more > 0 {
                said += &format!(
                    "; {} more not listed (each file lists its first {}): Export writes every one",
                    info.skips_more,
                    Skips::MAX
                );
            }
            ui.horizontal(|ui| {
                ui.weak(said);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| match &state.export {
                    Some((job, _)) => {
                        if ui.button("Cancel").clicked() {
                            job.cancel();
                        }
                        ui.add(egui::ProgressBar::new(job.progress.fraction()).desired_width(140.0).show_percentage());
                        ui.weak("exporting");
                    }
                    None => {
                        let tip = "Every skip (not only those listed) as a tab-separated file: the files are \
                                       read again. With nerka's version, the files' names and kinds and the \
                                       capture settings at the top, a summary at the end, and each skip's first \
                                       16 bytes: enough to attach to a bug report without the file itself.";
                        if ui
                            .add_enabled(!info.notes.is_empty(), egui::Button::new("Export…"))
                            .on_hover_text(tip)
                            .clicked()
                            && let Some(to) = rfd::FileDialog::new()
                                .set_file_name(format!("{file_name}-not-read.tsv"))
                                .add_filter("Tab-separated", &["tsv", "txt"])
                                .save_file()
                        {
                            asked = Asked::Export(to);
                        }
                    }
                });
            });
            ui.separator();
            let head = format!(
                "{:<18} {:>9} {:<26} {:>8} {:>7}  {}",
                "kind", "line", "time", "frame", "bytes", "protocol, sender, why"
            );
            ui.label(RichText::new(head).monospace().weak());
            let row_h = ui.text_style_height(&egui::TextStyle::Monospace) + 4.0;
            egui::ScrollArea::both().auto_shrink([false, false]).show_rows(ui, row_h, listed, |ui, rows| {
                for i in rows {
                    let s = &info.skips[i];
                    let text = RichText::new(row_text(s)).monospace();
                    let r = ui.selectable_label(state.selected == Some(i), text).on_hover_text(tip(s));
                    if r.clicked() {
                        state.selected = Some(i);
                        if let Some(l) = s.line {
                            asked = Asked::Goto(l);
                        }
                    }
                }
            });
        });
    if !open {
        state.open = false;
    }
    asked
}

#[cfg(test)]
mod tests {
    use super::*;
    use nerka_core::input::SkipKind;

    #[test]
    fn a_row_says_where_and_what() {
        let s = Skip {
            kind: SkipKind::LostFragment,
            line: Some(41),
            time: None,
            frame: Some(1234),
            offset: Some(99),
            bytes: 304,
            detail: "10.0.0.1 → 10.0.0.2".into(),
            file: "b.pcap".into(),
            head: vec![0x3d, 0x01],
            cut: Some((1514, 9000)),
        };
        let r = row_text(&s);
        assert!(r.starts_with("lost a fragment           42 –"), "{r}");
        assert!(r.ends_with("1234     304  b.pcap: 10.0.0.1 → 10.0.0.2"), "{r}");
        let t = tip(&s);
        assert!(t.contains("frame.number == 1234") && t.contains("starts with: 3d 01") && t.contains("byte 99"), "{t}");
    }
}
