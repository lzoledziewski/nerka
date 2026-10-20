//! The app icon for egui (window, taskbar, About dialog); the drawing is in
//! `icon_art.rs`, which `build.rs` also uses for the `.exe` file icon.

use crate::icon_art;
use eframe::egui::IconData;

pub fn icon() -> IconData {
    IconData { rgba: icon_art::render(icon_art::SIZE), width: icon_art::SIZE, height: icon_art::SIZE }
}
