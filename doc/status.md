# Status & next steps

Snapshot of where the project stands. Update this file whenever the state changes.

_Last update: 2026-10-18 (DLT import started: source-supplied fields)._

## State

- Builds and runs on **Windows** (MSVC, debug build). Clippy `-D warnings`, fmt and all tests pass (26 core, 6 GUI). A release build and the 1 GiB benchmark have not been run on Windows yet.
- Committed on `main`, **not pushed** (don't push until the owner says so).
- `.github/workflows/ci.yml` and `.vscode/extensions.json` are still **not on disk**; their contents are in the appendix below.

## GUI as of now (`ui/`)

Owner feedback is driving this; these are deliberate choices, keep them:

- **Tabs**, one per open file (klogg-style). Each tab has its own filter, view positions and bookmarks. Opening an already-open file switches to its tab. Ctrl+Tab / Ctrl+Shift+Tab (or Ctrl+PgDn/PgUp) switch tabs; Ctrl+W, ✖, middle-click or the context menu close them.
- **Filter box:** modes **Text** (literal by default; a **Regex** checkbox switches to regex; also Ignore case and Invert) and **Query** (NFD). The row is laid out right to left so it never gets wider than the window. If it did, the result view's scrollbar would end up off-screen (that was a real bug).
- **When a search runs (owner's decision):** only on Enter or the Search button. Ticking Regex / Ignore case / Invert, switching Text/Query or picking a history entry only changes the box's settings or contents; nothing runs until Enter. The last mode + checkbox combination is saved to `%APPDATA%
erka\prefs` (or `$XDG_CONFIG_HOME/nerka/prefs`) and becomes the default for new tabs.
- **Search history:** the 🕘 button lists recent filters; picking one fills the box. They are stored in `%APPDATA%\nerka\history` (or `$XDG_CONFIG_HOME/nerka/history`) as `text|regex|query<TAB>pattern`, using `core::SearchHistory`.
- **Text selection:** drag, Shift+click, double-click (word), Shift+arrows (lines), Ctrl+A, Esc. Ctrl+C copies the selection, or the current line if nothing is selected (at most 100k lines). The right-click menu has Copy, Select all and Toggle bookmark.
- **Go to line:** Ctrl+G / Ctrl+L opens a dialog. There is no toolbar field (the owner asked for it to be removed).
- **Look:** no tint on matching lines in the main view (the owner disliked the blue); the cursor line is neutral grey. Scrollbar thumbs use the text colour at 50% and are 14 px wide.
- **Scrolling:** the views read raw mouse-wheel events (3 rows per notch), not egui's smoothed delta; the smoothing made text look smeared. Rows and text origins snap to physical pixels. A headless test (`view::tests::wheel_scrolls_hovered_view_by_whole_lines`) runs the app's panel layout and injects wheel events. Use that pattern for other GUI input tests.
- **Icon:** a kidney ("nerka") with messy log lines going in and clean lines coming out, drawn in code (`ui/src/icon.rs`). It shows on the window and taskbar only; the `.exe` file icon is still the default, because embedding it needs a build dependency the owner hasn't approved.
- **Bookmarks:** these work (`m`, `b` / `B`, notes in the side panel, `<log>.nerka-marks` sidecar), but the owner wasn't sure about them yet. Hovering the 🔖 button explains them. Ask before investing more in them.

## Checking the GUI visually

`tools/screenshot.ps1` launches the debug build, captures its window to a PNG and closes it again. Read the PNG to check layout and rendering. This is how the off-screen scrollbar was found. Don't close or kill nerka-gui windows the owner has open; if the exe is locked by one of them, build with `--target-dir target/<name>` and pass `-Exe`.

## Next steps

1. Release build plus the benchmark on Windows: `cargo run --release -p nerka-core --example gen_log -- big.log 1024`, then `target\release\nerka filter -c --stats "level >= ERROR" big.log` and `target\release\nerka-gui big.log`. Compare the numbers with `architecture-analysis.md` §5.
2. Recreate the two config files from the appendix (ask before adding CI).
3. Continue klogg parity / Phase 1.5 (`architecture-analysis.md` §8). Follow/tail mode and highlight rules come first.

## Current focus

**DLT import** (owner, 2026-10-18), then a search manager / highlighters of the owner's own design (not a klogg clone; don't start it yet). Design: `sources-and-signals.md`.

Decided for DLT: own zero-copy parser in `core/src/input/dlt/` (no crate, no dependency); synthetic test data only (a DLT writer + `gen_dlt` example); shown as a **table with aligned, resizable columns** in our own view (no column sorting; each row is still one tab-separated text line, so search, highlight and copy keep working).

Done (step 1): `LogSource::line_bytes` returns `Cow`; sources can supply a field schema, per-line field values and levels; `Event` is lazy over `(source, line)`, so header-only filters never render a line; the DSL accepts source fields (`apid == NAV`, `mcnt >= 0x10`), resolved by `Query::bind` / `compile_for`. Text-log benchmark unchanged (1 GiB: `search ERROR` ~30 ms, `level >= ERROR` ~31 ms).

Next: DLT writer + parser with the columnar header index, then the table view.

## Decisions pending with the owner

Don't settle these silently: ask.

1. **UI framework:** egui was chosen instead of the spec's Tauri, because the owner asked for "not heavy" (they originally considered Qt/C++). Not yet confirmed.
2. **Level detection:** only upper-case tokens (`ERROR`, `[WARN]`, `level=INFO`) in the first 160 bytes are recognised. This needs real log samples from the owner.
3. **Bookmark storage:** currently a sidecar `<log>.nerka-marks` next to the log. The alternative is a central app-data folder when the log's directory is read-only.

## Known limitations

- The line index uses 8 bytes per line (≈94 MB per 12 M lines); a block-compressed index is planned.
- No follow/tail mode: if a file is truncated while it is mapped, reads may fault.
- Only UTF-8/ASCII text is handled; invalid bytes are shown as U+FFFD.
- The GUI binary is ~19 MB on Linux. The size on Windows hasn't been measured.

## Appendix: files to recreate

### `.github/workflows/ci.yml`

```yaml
name: CI

on:
  push:
    branches: [main]
  pull_request:

env:
  CARGO_TERM_COLOR: always

jobs:
  check:
    name: ${{ matrix.os }}
    runs-on: ${{ matrix.os }}
    strategy:
      fail-fast: false
      matrix:
        os: [windows-latest, ubuntu-latest, macos-latest]
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
        with:
          components: rustfmt, clippy
      - uses: Swatinem/rust-cache@v2
      - name: Format
        run: cargo fmt --all --check
      - name: Clippy
        run: cargo clippy --workspace --all-targets -- -D warnings
      - name: Test
        run: cargo test --workspace
      - name: Build release
        run: cargo build --release --workspace
      - name: Upload binaries
        uses: actions/upload-artifact@v4
        with:
          name: nerka-${{ matrix.os }}
          path: |
            target/release/nerka
            target/release/nerka-gui
            target/release/nerka.exe
            target/release/nerka-gui.exe
          if-no-files-found: ignore
```

### `.vscode/extensions.json`

```json
{
  "recommendations": ["rust-lang.rust-analyzer", "tamasfe.even-better-toml", "vadimcn.vscode-lldb"]
}
```
