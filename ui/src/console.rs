//! Text for the terminal `nerka-gui` was started from (`--help`, an unknown
//! option). A release build on Windows is a GUI program without a console
//! of its own, so it attaches to its parent's first; started from Explorer
//! there is none, and the text goes nowhere.

use std::io::Write;

/// `text` on stdout.
pub fn print(text: &str) {
    attach();
    let _ = std::io::stdout().write_all(text.as_bytes());
}

/// `text` on stderr.
pub fn error(text: &str) {
    attach();
    let _ = std::io::stderr().write_all(text.as_bytes());
}

#[cfg(all(windows, not(debug_assertions)))]
fn attach() {
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn AttachConsole(process_id: u32) -> i32;
    }
    const ATTACH_PARENT_PROCESS: u32 = u32::MAX;
    // SAFETY: AttachConsole takes a plain integer and has no other
    // preconditions; when it fails (no parent console) nothing changes.
    unsafe {
        AttachConsole(ATTACH_PARENT_PROCESS);
    }
}

/// A debug build on Windows, and other systems, have the terminal already.
#[cfg(not(all(windows, not(debug_assertions))))]
fn attach() {}
