//! Embeds the app icon (and version information from Cargo) in the
//! Windows `.exe`, so Explorer and the taskbar show it for the file too.
//! The icon is rendered from the same code as the window icon
//! (`src/icon_art.rs`), so no image file is kept in the repo.

#[allow(dead_code)]
#[path = "src/icon_art.rs"]
mod icon_art;

fn main() {
    println!("cargo:rerun-if-changed=src/icon_art.rs");
    println!("cargo:rerun-if-changed=build.rs");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let out = std::path::PathBuf::from(std::env::var("OUT_DIR").expect("set by cargo")).join("nerka.ico");
    std::fs::write(&out, icon_art::ico(&[16, 20, 24, 32, 40, 48, 64, 128, 256])).expect("write the .ico");
    embed(&out);
}

/// The resource compiler (`rc.exe`, from the Windows SDK) runs on Windows
/// hosts; elsewhere the `.exe` is built without a file icon.
#[cfg(windows)]
fn embed(ico: &std::path::Path) {
    let mut res = winresource::WindowsResource::new();
    res.set_icon(&ico.display().to_string());
    if let Err(e) = res.compile() {
        println!("cargo:warning=building without the .exe icon: {e}");
    }
}

#[cfg(not(windows))]
fn embed(_: &std::path::Path) {
    println!("cargo:warning=building without the .exe icon (needs a Windows host)");
}
