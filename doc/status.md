# Status & next steps

Snapshot of where the project stands. Update this file whenever the state changes.

_Last update: 2026-10-19 (GitHub issues #3–#6 on branch `claude/issues-3-6`: unnamed selection entries, pack names with spaces, search history with ↑ / ↓, long lines; #2 closed as invalid by the owner)._

## State

- Builds and runs on **Windows** (MSVC, debug and release). Clippy `-D warnings`, fmt and all tests pass (75 core, 32 GUI). The 1 GiB benchmarks have been run on Windows (numbers below and in the commit messages).
- Committed on `main`, **not pushed** since `5b632c3` (don't push until the owner says so).
- Only built for Windows so far. Linux/macOS builds and releases are open questions (see "Decisions pending").
- **CI and releases (owner, 2026-10-18):** `.github/workflows/ci.yml` runs fmt, clippy and the tests on Linux and Windows for pushes to `main` and every PR (macOS left out: its runner minutes count 10x on a private repo). `release.yml` builds `nerka` + `nerka-gui` for Linux (Ubuntu 22.04, glibc 2.35+, `.tar.gz`) and Windows (`.zip`) on a `v*` tag and publishes them as a GitHub pre-release (tags with `-`, e.g. `v0.1.0-alpha.1`); run by hand it only builds artifacts. Version is `0.1.0-alpha.3`. `.vscode/extensions.json` is still not on disk (appendix).

## GUI as of now (`ui/`)

Owner feedback is driving this; these are deliberate choices, keep them:

- **Filter and Search (owner, 2026-10-18):** the **Filter** decides what the **main view** shows (only the kept lines; every line without a filter). The **Search** looks only among those lines; its hits are listed in the **bottom pane** (clicking one moves the main view there) and coloured per query in both views; n / N jump between hits. This replaces the klogg layout (whole file on top, filter result below). Going to a line that the filter hides lands on the next kept line. The filter's matches are no longer tinted: every row in the main view matches it. The **Filter bar sits at the top**, under the tab bar and across the whole window; the Search box and its hits pane are at the bottom.
- **Line colours (owner, 2026-10-18):** each included library filter query can colour the whole lines it keeps in the main view. Off by default; switched on per query by clicking its swatch in the selection or its chip under the Filter box (filled = on). Where a line matches several, the later query in the selection wins. The cursor and selection are drawn over the colour. The search hits pane shows them too (owner, 2026-10-19). Colours follow the **selection**, not the Filter box text (owner, 2026-10-19): editing the box by hand keeps them (each lit node is searched among the lines the current filter keeps); removing a node from the selection removes its colour. No library-level default: a pack colour only picks the colour, it never switches highlighting on (owner: many packs must not cost conflicts or time).
- **Presets (owner, 2026-10-19):** a bar above the Selection pane (**Preset** list + ⋮: New from this tab… → Keep (session only, `•`) or Save; Update '<name>' from this tab (overwrites the file of a saved one); Save to disk; Delete). A preset holds the selection in order (role, line colour, without-parents), copies of edited and new queries (an internal `_preset` pack), parameter values, and the boxes' typed text (the Filter box as "the selection" when it holds its expansion). Not stored: search history, the Search box's library picks. Using one replaces the tab's selection and runs both boxes. Files: `%APPDATA%\nerka\presets\<name>.nerka-preset` (format and parser: `core/src/preset.rs`); a library query that isn't loaded any more is reported, not dropped.
- **Windows as sessions (owner, 2026-10-20):** `nerka-gui FILE…` hands its files to the nerka window **focused last** (it opens them as tabs and comes to the front) and exits; with `--new-window`, other startup options, or no files, a new window starts. Each window listens on a loopback port registered with a random token in `<config>/nerka/instances/<pid>` (rewritten on focus; stale entries are dropped), std only (`ui/src/instance.rs`). A tab's menu has **Move to new window** (its filter, search and selection go along as a temporary preset). **Settings** (⚙ or Ctrl+,; a window, not a menu: a menu closes on any click, so its fields couldn't be typed in) → **Name** names the window: the title becomes `<name> · <file> — nerka`, so Alt+Tab tells sessions apart (`--name` at startup).
- **Several files as one view (owner, 2026-10-20):** files opened together (Open dialog multi-select, dropping several, `nerka-gui a.dlt b.dlt`, a hand-off) become **one tab, one after another** in the order given, if they are of **one kind** (same columns and fields: plain text with plain text; DLT with DLT, captures included); mixed kinds open as separate tabs, saying why. Core: `input::concat::Concat` (`nerka_core::open_many`) adds a `file` field (`file == b.dlt`); the engine searches part by part (`LogSource::parts`), so each file keeps its fast path (single-file numbers unchanged: text `search ERROR` ~31 ms, DLT `apid == NAV AND level >= WARN` ~29 ms per GiB). Bookmarks stay in each file's own sidecar (`Bookmarks::load_for_parts` / `save_for_parts`). The tab is titled `a.dlt + 2` (hover lists the files); its menu can open the files as separate tabs. **Merging by time is not done**: what "time" means across files has to be defined first (owner).
- **Tabs**, one per open file (klogg-style). Each tab has its own filter, view positions and bookmarks. Opening an already-open file switches to its tab. Ctrl+Tab / Ctrl+Shift+Tab (or Ctrl+PgDn/PgUp) switch tabs; Ctrl+W, ✖, middle-click or the context menu close them.
- **Filter box:** a mode list (owner, 2026-10-19; `search-modes.md`): **Auto** (default: plain words are an exact phrase, NFD syntax a query, they mix, and a box that isn't a valid query is literal text; Ignore case and Invert), **Text** (literal by default; a **Regex** checkbox switches to regex; also Ignore case and Invert) and **NFD** (strict; on an error, **Search as text instead**). A preview line under the box says how it will be read (`→ text: …` / `→ query: …`). Library picks put the box into NFD mode; the library is not affected by Auto. Its button is **Filter** (it was labelled Search, which was confusing). The row is laid out right to left so it never gets wider than the window. If it did, the result view's scrollbar would end up off-screen (that was a real bug).
- **When a search runs (owner's decision):** only on Enter or the Search button. Ticking Regex / Ignore case / Invert, switching the mode or picking a history entry only changes the box's settings or contents; nothing runs until Enter. The last mode + checkbox combination is saved to `%APPDATA%\nerka\prefs` (or `$XDG_CONFIG_HOME/nerka/prefs`) and becomes the default for new tabs.
- **Search history:** the 🕘 button lists recent filters; picking one fills the box. They are stored in `%APPDATA%\nerka\history` (or `$XDG_CONFIG_HOME/nerka/history`) as `text|regex|query<TAB>pattern`, using `core::SearchHistory`.
- **Search box history (owner, issue #5):** the Search box has its own history, kept apart from the filters in `search-history` next to `history` (same format), with its own 🕘 button. Searches from the box (Enter / Find) and from a library 🔍 are recorded. In both boxes ↑ / ↓ step through the history shell-style (↓ past the newest entry gives back what was typed; typing stops browsing); like the 🕘 menu, this only fills the box, nothing runs until Enter.
- **Text selection:** drag, Shift+click, double-click (word), Shift+arrows (lines), Ctrl+A, Esc. Ctrl+C copies the selection, or the current line if nothing is selected (at most 100k lines). The right-click menu has Copy, Select all, Show whole line and Toggle bookmark. **Picked lines** (issue #12, 2026-10-20): Ctrl+click (or Ctrl+Space on the cursor line) picks or unpicks single lines anywhere, marked across the full width; Ctrl+C or the menu copies the picked lines whole in file order; a plain click or Esc clears them; picks are file lines, so they stay when the filter changes.
- **Long lines (owner, issue #6):** a horizontal scrollbar appears under a view when the visible lines are wider than it (Shift+wheel and ← / → still scroll too). **Enter** (or the right-click menu's **Show whole line…**) opens a line window: for a table (DLT) the header columns as a name / value grid, then the message with its own line breaks and tabs as the logger wrote them (`LogSource::line_detail` in core; the table row shows them as spaces), wrapped or not, selectable, with **Copy message** and **Copy line** (columns tab-separated, the message's line breaks kept); plain text shows the whole line (not cut at 4 KiB) with **Copy**. The window shows at most 1 MiB; Copy copies everything.
- **Go to line:** Ctrl+G / Ctrl+L opens a dialog. There is no toolbar field (the owner asked for it to be removed).
- **Look:** no tint on matching lines in the main view (the owner disliked the blue); the cursor line is neutral grey. Scrollbar thumbs use the text colour at 50% and are 14 px wide.
- **Scrolling:** the views read raw mouse-wheel events (3 rows per notch), not egui's smoothed delta; the smoothing made text look smeared. Rows and text origins snap to physical pixels. A headless test (`view::tests::wheel_scrolls_hovered_view_by_whole_lines`) runs the app's panel layout and injects wheel events. Use that pattern for other GUI input tests.
- **Icon:** a kidney ("nerka") with messy log lines going in and clean lines coming out, drawn in code (`ui/src/icon_art.rs`, no image file). It shows on the window and taskbar, and since 2026-10-20 (owner) also as the **`.exe` file icon**: `ui/build.rs` renders the same drawing at 16–256 px into an `.ico` and embeds it with `winresource` (build dependency, Windows only, MIT, one small dependency of its own; also writes the version info from `Cargo.toml`). The resource compiler comes with the Windows SDK; without it the build warns and goes on without the file icon.
- **About:** an "ℹ About" button at the right of the toolbar (or F1) opens a modal with the icon, version, author (© 2026 Łukasz Żołędziewski, email as a mailto link), licence and repository link. All of these come from `Cargo.toml` (`env!`), so keep `authors` / `repository` `.workspace = true` in each crate.
- **Search manager (POC):** 📚 (Ctrl+Shift+L) opens two panes: the **Library** (a tree: refining queries under their parent; `+` or drag adds to the selection, `🔍` searches, `⋮` edit / refine / exclude, `+ New query`) and the **Selection** (ordered filter queries; drag to reorder, `+` / `−` role, `✎` edit, save to a pack; owner, 2026-10-18: no checkboxes). **Apply** puts the selection into the Filter bar. Searching is an action: `🔍` → **Search for it** (replace) or **Add to search (OR)** (typed text becomes a query first). The Search box looks only inside the filter result and colours hits per query (n / N jump between search hits). Query names may contain spaces (quoted in pack files).
- **Bookmarks:** these work (`m`, `b` / `B`, notes in the side panel, `<log>.nerka-marks` sidecar; the panel shows each line wrapped to two rows, ▸ / ▾ or a click unfolds the whole line), but the owner wasn't sure about them yet. Hovering the 🔖 button explains them. Ask before investing more in them.

## Checking the GUI visually

`tools/screenshot.ps1` launches the debug build, captures its window to a PNG and closes it again. Read the PNG to check layout and rendering. This is how the off-screen scrollbar was found. `-Keys` sends keystrokes before the capture (SendKeys syntax), e.g. `-Keys '{F1}'` for the About dialog. Don't close or kill nerka-gui windows the owner has open; if the exe is locked by one of them, build with `--target-dir target/<name>` and pass `-Exe`.

## Next steps

1. Owner runs `cargo run --release -p nerka-core --example probe_capture -- <files>` on real pcap / pcapng / MF4 captures and shares the report (structure only, no payload or addresses); it decides the pcap dependencies (own reader + `etherparse` proposed) and the MF4 approach. Plan agreed: DLT is extracted into a temporary `.dlt` (cleaned up on tab close and at startup), shown under the capture's name.
2. Next GUI items (owner, 2026-10-19): single instance (new files open in the last focused window), move a tab to a new window, named sessions in the title. Later: several source files in one view (first: concatenated). Backsync from the box text into the selection is parked (needs a more interactive design).
3. Owner tries the DLT table on real use; settle the open DLT questions below.
4. **Search manager** POC: all five steps are built; the owner tries it and answers the GUI choices 7–12 in `search-manager.md` → "POC status".
5. Releases: `v0.1.0-alpha.1` (2026-10-18) and `v0.1.0-alpha.2` (2026-10-20: Auto / Text / NFD modes, presets, line colours following the selection, chip row, long-line window, search history) are tagged and published.

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

Done (step 4, 2026-10-20): **DLT in pcap / pcapng captures**, read in place (no temporary `.dlt`): `core/src/input/capture/` (container `pcap.rs`: pcap µs/ns both byte orders, pcapng sections / interfaces / `if_tsresol` / `if_tsoffset`; transport `net.rs`: Ethernet, stacked VLAN tags, Linux SLL / SLL2, raw IP, loopback, IPv4 / IPv6 with **fragment reassembly** in any order (30 s capture-time timeout, at most 4096 pending); router: UDP destination ports, default **3490** only (the DLT protocol's port; owner, 2026-10-20), configurable: CLI `--dlt-port` (repeatable), GUI ⚙ → UDP ports (saved in `prefs` as `dlt_ports`, used for captures opened afterwards), core `OpenOptions` / `CaptureOptions`). The DLT reader was generalised: a message takes its storage time and ECU from its index entry (`Framed`, 16 B: offset with a side-buffer bit and a 15-bit capture channel, time), so a `.dlt` file is the simplest case; reassembled datagrams live in a side buffer. A message gets the time of the frame that completed its datagram; its ECU is its own (WEID). `nerka_core::open` recognises captures by content; the GUI's Open dialog lists `.pcap` / `.pcapng`. No new dependency. Benchmark: `big.dlt` → `big.pcap` with `examples/dlt_to_pcap` (870 MB, 12.5 M messages, VLAN, several messages per datagram, every 400th datagram fragmented): index ~300 ms (`.dlt`: ~255 ms), the same match counts and search times as the `.dlt`; the `.dlt` numbers are unchanged by the refactor. **MF4** (2026-10-20, `capture/mf4.rs`, own reader, no dependency): block tree over the mapped file; unsorted (1/2/4/8-byte record ids) and sorted data; DT / DL / HL (records spanning data blocks are copied to the side buffer); VLSD channel groups (paired with their frame group by order) and SD blocks (by offset); DZ refused for now. Groups by channel names (`ETH_Frame.*` → Ethernet frames into the same transport and port router as pcap; `DLT_Frame.*` → DLT directly, its `ECU_ID` the fallback ECU), else by shape (an Ethernet bus-event group, as the owner's X2E files). Time = header start + master channel through its linear conversion (none: the raw value in seconds, as the spec says). Not yet tried on the owner's real files. Design and next steps (pluggable layers): `capture-import.md` (local draft).

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
- **Licence files** (owner, 2026-10-18): `LICENSE-MIT` (copyright line "Copyright (c) 2026 Łukasz Żołędziewski", as in the About dialog) and `LICENSE-APACHE` (the canonical Apache 2.0 text from apache.org) are in the repo root and ship in the release archives.

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
5. **Releases / downloads:** `release.yml` (Linux + Windows pre-releases on `v*` tags, archives with the README and both licence files) is in place; open: macOS builds (unsigned binaries warn; 10x runner minutes).
6. **Linux builds from this Windows PC:** no WSL, Docker or zig installed. Options: cross-compile with `cargo zigbuild --target x86_64-unknown-linux-gnu.2.17` (user-level installs; build only, untested), WSL (can also run the GUI), or GitHub Actions. `Cargo.lock` has no GTK (the file dialog uses D-Bus via `zbus`), so a cross build is expected to link; not verified.
7. **Copying multi-line messages (owner, 2026-10-19: action item, rethink later):** a decoded message's own line breaks and tabs show as spaces in the views and are kept only in the line window (Enter). Ctrl+C / Copy in the views copy the display text (formatting dropped); the line window's Copy message / Copy line keep it. The owner isn't fully happy with this, but neither single rule is good: always copying with the original formatting breaks the one-row-per-line, tab-separated paste (spreadsheets, grep), while always copying the display text loses stack traces and JSON. Revisit (e.g. a copy mode, or a second shortcut).

## Known limitations

- The line index uses 8 bytes per line (≈94 MB per 12 M lines); a block-compressed index is planned.
- No follow/tail mode: if a file is truncated while it is mapped, reads may fault.
- Only UTF-8/ASCII text is handled; invalid bytes are shown as U+FFFD.
- The GUI binary is ~19 MB on Linux. The size on Windows hasn't been measured.
- DLT: only files with storage headers; no FIBEX (non-verbose payloads stay hex); arrays/structs/fixed-point arguments shown as hex; time in UTC only; the message index is built single-threaded (~280 ms per GiB).

## Appendix: files to recreate

### `.vscode/extensions.json`

```json
{
  "recommendations": ["rust-lang.rust-analyzer", "tamasfe.even-better-toml", "vadimcn.vscode-lldb"]
}
```
