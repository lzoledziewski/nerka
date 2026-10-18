# Status & next steps

Snapshot of where the project stands. Update this file whenever the state changes.

_Last update: 2026-10-18 (search manager POC built on branch `claude/search-manager-poc-d451e1`: core, CLI and GUI; owner to try it)._

## State

- Builds and runs on **Windows** (MSVC, debug and release). Clippy `-D warnings`, fmt and all tests pass (63 core, 15 GUI). The 1 GiB benchmarks have been run on Windows (numbers below and in the commit messages).
- Committed on `main`, **not pushed** since `5b632c3` (don't push until the owner says so).
- Only built for Windows so far. Linux/macOS builds and releases are open questions (see "Decisions pending").
- `.github/workflows/ci.yml` and `.vscode/extensions.json` are still **not on disk**; their contents are in the appendix below.

## GUI as of now (`ui/`)

Owner feedback is driving this; these are deliberate choices, keep them:

- **Tabs**, one per open file (klogg-style). Each tab has its own filter, view positions and bookmarks. Opening an already-open file switches to its tab. Ctrl+Tab / Ctrl+Shift+Tab (or Ctrl+PgDn/PgUp) switch tabs; Ctrl+W, ✖, middle-click or the context menu close them.
- **Filter box:** modes **Text** (literal by default; a **Regex** checkbox switches to regex; also Ignore case and Invert) and **Query** (NFD). The row is laid out right to left so it never gets wider than the window. If it did, the result view's scrollbar would end up off-screen (that was a real bug).
- **When a search runs (owner's decision):** only on Enter or the Search button. Ticking Regex / Ignore case / Invert, switching Text/Query or picking a history entry only changes the box's settings or contents; nothing runs until Enter. The last mode + checkbox combination is saved to `%APPDATA%\nerka\prefs` (or `$XDG_CONFIG_HOME/nerka/prefs`) and becomes the default for new tabs.
- **Search history:** the 🕘 button lists recent filters; picking one fills the box. They are stored in `%APPDATA%\nerka\history` (or `$XDG_CONFIG_HOME/nerka/history`) as `text|regex|query<TAB>pattern`, using `core::SearchHistory`.
- **Text selection:** drag, Shift+click, double-click (word), Shift+arrows (lines), Ctrl+A, Esc. Ctrl+C copies the selection, or the current line if nothing is selected (at most 100k lines). The right-click menu has Copy, Select all and Toggle bookmark.
- **Go to line:** Ctrl+G / Ctrl+L opens a dialog. There is no toolbar field (the owner asked for it to be removed).
- **Look:** no tint on matching lines in the main view (the owner disliked the blue); the cursor line is neutral grey. Scrollbar thumbs use the text colour at 50% and are 14 px wide.
- **Scrolling:** the views read raw mouse-wheel events (3 rows per notch), not egui's smoothed delta; the smoothing made text look smeared. Rows and text origins snap to physical pixels. A headless test (`view::tests::wheel_scrolls_hovered_view_by_whole_lines`) runs the app's panel layout and injects wheel events. Use that pattern for other GUI input tests.
- **Icon:** a kidney ("nerka") with messy log lines going in and clean lines coming out, drawn in code (`ui/src/icon.rs`). It shows on the window and taskbar only; the `.exe` file icon is still the default, because embedding it needs a build dependency the owner hasn't approved.
- **About:** an "ℹ About" button at the right of the toolbar (or F1) opens a modal with the icon, version, author (© 2026 Łukasz Żołędziewski, email as a mailto link), licence and repository link. All of these come from `Cargo.toml` (`env!`), so keep `authors` / `repository` `.workspace = true` in each crate.
- **Search manager (POC):** a 📚 Library panel (Ctrl+Shift+L) with the queries of the packs; picks expand into the Filter box or into the new **Search box** under it, which searches only inside the filter result and colours hits per query (n / N jump between search hits). Details and the choices still to confirm: `search-manager.md` → "POC status".
- **Bookmarks:** these work (`m`, `b` / `B`, notes in the side panel, `<log>.nerka-marks` sidecar), but the owner wasn't sure about them yet. Hovering the 🔖 button explains them. Ask before investing more in them.

## Checking the GUI visually

`tools/screenshot.ps1` launches the debug build, captures its window to a PNG and closes it again. Read the PNG to check layout and rendering. This is how the off-screen scrollbar was found. `-Keys` sends keystrokes before the capture (SendKeys syntax), e.g. `-Keys '{F1}'` for the About dialog. Don't close or kill nerka-gui windows the owner has open; if the exe is locked by one of them, build with `--target-dir target/<name>` and pass `-Exe`.

## Next steps

1. Owner tries the DLT table on real use; settle the open DLT questions below.
2. **Search manager** POC: all five steps are built; the owner tries it and answers the GUI choices 7–12 in `search-manager.md` → "POC status".
3. Recreate the two config files from the appendix (ask before adding CI).

Text-log benchmark on Windows (1 GiB, 12M lines, release): index ~90 ms, `search ERROR` ~30 ms, `level >= ERROR` ~31 ms, `level >= WARN AND raw ~ /timeout/` ~30 ms, `search -v ERROR` ~100 ms.

## Current focus

**DLT import** (owner, 2026-10-18), then a search manager / highlighters of the owner's own design (not a klogg clone; don't start it yet). Design: `sources-and-signals.md`.

Decided for DLT: own zero-copy parser in `core/src/input/dlt/` (no crate, no dependency); synthetic test data only (a DLT writer + `gen_dlt` example); shown as a **table with aligned, resizable columns** in our own view (no column sorting; each row is still one tab-separated text line, so search, highlight and copy keep working).

Done (step 1): `LogSource::line_bytes` returns `Cow`; sources can supply a field schema, per-line field values and levels; `Event` is lazy over `(source, line)`, so header-only filters never render a line; the DSL accepts source fields (`apid == NAV`, `mcnt >= 0x10`), resolved by `Query::bind` / `compile_for`. Text-log benchmark unchanged (1 GiB: `search ERROR` ~30 ms, `level >= ERROR` ~31 ms).

Done (step 2): own DLT reader in `core/src/input/dlt/` (no new dependencies). `nerka_core::open` picks DLT or text by content; the CLI uses it, the **GUI still opens everything as text** (switching it comes with the table view).
- Index = one `u64` offset per message (~8 B/msg). Headers are read in place, so header-only filters never decode payloads.
- Fields: `ecu apid ctid type mcnt tmsp seid mid mode payload`; `payload` is the decoded payload column only (unlike `raw`, which spans every column), e.g. `payload ~ "timeout"`, and is checked after the header conditions; `mode` is `verbose` / `non-verbose` (`mid` stays empty on verbose messages, since 0 is a valid message id); `level` comes from the header (`VERBOSE` is accepted as an alias for `TRACE`).
- Line = tab-separated columns `time tmsp ecu apid ctid type payload` (time is the storage time in **UTC**; `type` is the level for log messages, e.g. `error`).
- Payload: verbose bool/int/float/string/raw/trace info, with VARI names and units; arrays, structs and fixed-point fall back to `<undecoded> hex`. Non-verbose: `[id] hex`. Control: service name, status, version string.
- Corrupt data: resync on the next `DLT\x01`, skipped bytes counted. Files without storage headers aren't supported.
- `writer` module + `examples/gen_dlt.rs` produce synthetic files.
- 1 GiB synthetic DLT (12.5M messages): index 280 ms (single-threaded), 95 MiB; `ecu == ECU1` 37 ms, `apid == NAV AND level >= WARN` 29 ms, `level >= ERROR` 32 ms, `raw ~ /timeout/` 288 ms (renders every line), `apid == NAV AND raw ~ /timeout/` 60 ms.

Done (step 3): the GUI opens files via `nerka_core::open`; sources with columns (`LogSource::columns`, DLT) are shown as a **table** in the existing view: a header row with the column names, cells aligned at column boundaries, faint dividers, and a divider drag in the header resizes the column (widths belong to the tab, shared by the main and filter views; not saved yet). Text that doesn't fit a cell is hidden and marked with "…". Selection, double-click, search highlighting and copy still work on the line text (copy keeps the tabs, so it pastes into a spreadsheet as columns). Plain text looks as before. The Open dialog lists `.dlt` files.

Open with the owner: save column widths? Sorting stays out (decided). Next after DLT: the search manager / highlighters (owner's own design).

### DLT decisions (owner, 2026-10-18)

- **Own parser, no crate** (`dlt-core` / `dlt_parse` rejected; spec §3.1 few dependencies). Zero-copy over the mmap.
- **Location:** `core/src/input/dlt/`, not a `nerka-dlt` crate yet. Extract it into a crate when the `Container` / `Decoder` traits of `sources-and-signals.md` arrive with pcap; that's where a crate boundary pays off.
- **Filtering** by ECU / APID / CTID / level comes from the headers (source-supplied fields + DSL field predicates), never guessed from text. Header-only filters must not decode payloads, and every fast path is tested against the naive per-line evaluator.
- **Display:** a real table (aligned, resizable columns), built into our own log view; **no column sorting** (it would break line numbers, bookmarks and next/previous match, and logs are read in time order). Each row is still one tab-separated text line underneath.
- **Test data:** synthetic only (`writer` module + `gen_dlt`); no real `.dlt` files in the repo.
- **Non-verbose messages:** shown as `[message id] hex` until FIBEX support exists. `mid` is that message id: the key into the FIBEX database that will later decode the payload.
- **Time:** `time` is the logger's storage time rendered in UTC, `tmsp` the ECU uptime (0.1 ms). The full `Timestamp { ns, clock }` model of `sources-and-signals.md` waits until sources are merged or signals arrive.

## Licensing (owner, 2026-10-18)

- **Stays `MIT OR Apache-2.0`** for the core and the apps.
- Goal: the core stays OSS; extensions (plugins / DLLs, planned WASM or C ABI) can be sold as closed source. A permissive licence allows that; it also allows others to sell closed forks, which the owner accepts for now.
- Considered: **MPL-2.0** (core changes must stay open, closed plugins still allowed), LGPL (awkward with Rust's static linking), GPL + plugin exception, AGPL + commercial dual licence (needs a CLA). Revisit before the first public release or before accepting outside contributions: relicensing later needs every contributor's consent, so decide on a CLA first.
- Code already published under `MIT OR Apache-2.0` stays available under it.
- The plugin interface is also the licensing boundary: keep it stable and documented.
- **Missing:** `LICENSE-MIT` and `LICENSE-APACHE` files in the repo root (a dual licence normally ships both).

## Search manager (POC built: core, CLI, GUI)

Design agreed with the owner on 2026-10-18 after mockups: **`search-manager.md`** (decisions, model, POC scope, open questions). In short:

- **No result tabs.** Two functions with the same inputs: **Filter** and **Search**. Search only looks inside the filter result.
- Queries live in shareable **packs** (files). Child queries refine a component's parent query. **Labels** group queries across packs and components. **Parameters** are substituted regex-escaped.
- **NFD is unchanged:** picking from the library expands (in core) into plain NFD, with provenance for search colours.
- Parked: scenarios / skill export, pack collaboration (dirty / clean, push / pull), the details view with replace / extract.
- Still not a copy of klogg's saved filters / highlighters lists.

## Where state lives

Nothing is stored in the cloud.

- **Nerka:** search history and prefs in `%APPDATA%\nerka\` (Linux: `~/.config/nerka/`); bookmarks as `<log>.nerka-marks` next to the log. Copy those files to move them to another machine.
- **Agent sessions** (Claude Code) are local to the machine they ran on. The project's decisions and state belong in this repo (`CLAUDE.md`, this file, `sources-and-signals.md`, `architecture-analysis.md`), so a new session on any machine starts from the repo.

## Decisions pending with the owner

Don't settle these silently: ask.

1. **UI framework:** egui was chosen instead of the spec's Tauri, because the owner asked for "not heavy" (they originally considered Qt/C++). Not yet confirmed.
2. **Level detection:** only upper-case tokens (`ERROR`, `[WARN]`, `level=INFO`) in the first 160 bytes are recognised. This needs real log samples from the owner.
3. **Bookmark storage:** currently a sidecar `<log>.nerka-marks` next to the log. The alternative is a central app-data folder when the log's directory is read-only.
4. **DLT column widths:** resizable per tab but not saved. Save them (globally or per file)?
5. **Releases / downloads:** not set up. Options discussed: `gh release create` by hand, or a `.github/workflows/release.yml` that builds Windows/Linux/macOS on a `v*` tag and attaches zips (`softprops/action-gh-release`). Needs pushing, and adds the first CI file (ask first). Unsigned macOS binaries will warn; the `.exe` icon needs an unapproved build dependency.
6. **Linux builds from this Windows PC:** no WSL, Docker or zig installed. Options: cross-compile with `cargo zigbuild --target x86_64-unknown-linux-gnu.2.17` (user-level installs; build only, untested), WSL (can also run the GUI), or GitHub Actions. `Cargo.lock` has no GTK (the file dialog uses D-Bus via `zbus`), so a cross build is expected to link; not verified.
7. **Licence files:** add `LICENSE-MIT` and `LICENSE-APACHE`, and with which copyright line?

## Known limitations

- The line index uses 8 bytes per line (≈94 MB per 12 M lines); a block-compressed index is planned.
- No follow/tail mode: if a file is truncated while it is mapped, reads may fault.
- Only UTF-8/ASCII text is handled; invalid bytes are shown as U+FFFD.
- The GUI binary is ~19 MB on Linux. The size on Windows hasn't been measured.
- DLT: only files with storage headers; no FIBEX (non-verbose payloads stay hex); arrays/structs/fixed-point arguments shown as hex; time in UTC only; the message index is built single-threaded (~280 ms per GiB).

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
