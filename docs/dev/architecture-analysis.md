# Nerka — Architecture Analysis & Decisions (v0.1 scaffold)

Companion to `nerka_architecture_implementation_spec_v_0.md`. This records
what the scaffold implements, the decisions taken on the spec's open
questions, where it deliberately deviates, and what comes next.

Date: 2026-10-17 (layout and roadmap updated 2026-10-22)

---

## 1. Requirements recap

From the README and spec, ranked by what shapes the architecture:

| # | Requirement | Consequence |
|---|---|---|
| R1 | klogg-level speed on Windows first (regex, big files) | Native code, memory-mapped I/O, parallel SIMD scanning, a virtual list view |
| R2 | Lightweight, not a monolith; usable as just a viewer | Single small binaries; UI-free core library; CLI usable on its own |
| R3 | Multiple input formats (text, PCAP, DLT…) | Format-neutral `LogSource` trait; the engine never sees file formats |
| R4 | Multi-platform: shell, Windows, Linux, macOS | Rust + a UI toolkit with no platform-specific code |
| R5 | DSL filtering (NFD) | Small parser in core; the UI only sends query strings |
| R6 | Deployable as light cloud nodes; collaboration later | Core must embed in a headless server; annotations need a shareable model |

## 2. UI framework

**egui / eframe** (owner, 2026-11-11: settled, the frontend stays egui).
It paints only the visible rows, straight from the memory map, with no IPC
or serialization in between; it is one native binary, and it compiles to
WebAssembly for a later browser UI. The spec's rules still hold: the UI does
no filtering and no parsing and only calls core APIs; spec §3.6 (Tauri
commands) are plain Rust calls.

## 3. Indexing strategy (spec §13 open question)

The spec says "streaming-first". For an interactive viewer the scaffold uses
**memory-map + line-offset index**:

- `TextFile::open` maps the file read-only. `LineIndex::build` splits it into
  8 MiB chunks and finds `\n` with SIMD (`memchr`) in parallel (rayon).
- Random access to any line is O(1), which jump-to-line, scrolling and
  "filtered view → main view" navigation all need. Pure streaming can't do
  this without re-reading the file.
- The cost is 8 bytes per line (≈ 94 MB for 12 M lines). Roadmap: block-compressed
  offsets (u32 deltas per 64-line block) cut this ~4×, which is how klogg does it.

Streaming still applies where it matters: search runs over chunks in
parallel with progress reporting and cancellation, and stdin/follow mode
will append to the index incrementally (see roadmap).

## 4. Filter engine

Two filter kinds share one engine (`engine::search`):

- `Filter::Regex` — the klogg search box (ignore case, literal, invert).
- `Filter::Query` — NFD, the spec's filter DSL (§3.4).

**Fast path.** If the filter has a *prefilter regex* — any plain regex, or a
query with a positive `raw ~ /…/` in its top-level AND — the regex scans the
raw mapped bytes chunk by chunk. Only candidate lines are fully evaluated.
Lines with no hit cost only the SIMD-accelerated scan.

**Slow path.** Per-line evaluation in parallel, used for inverted regexes,
`level >= ERROR` alone, and future decoded formats.

Correctness details:

- `^`/`$` anchor per line, including CRLF lines.
- Matches that spill across a line break are re-verified on the single line.
- Every test cross-checks the fast path against the naive per-line path.

### DSL grammar (spec §13 open question)

**Handwritten recursive descent** (`core/src/dsl/parser.rs`, ~300 lines)
rather than a PEG generator:

- Zero dependencies, which matches spec §3.1 "no heavy dependencies".
- Errors carry an exact column (the CLI prints a caret under it).
- The grammar is tiny and easy to extend.

Supported beyond the spec's v1 list: `OR`, parentheses, `!=`, `<`, `<=`,
`>`, `!~`, `/re/i`, plus the bare shorthands `/re/` and `"text"`.

Spec §3.5 "cheap checks first, regex last" is implemented: AND/OR operands
are flattened and sorted by estimated cost at compile time.

### Event model (spec §3.3)

`raw` is always the untouched line. Fields are computed lazily, only when a
query references them. `level` comes from a hand-rolled scanner that looks
for upper-case level tokens in the first 160 bytes. It replaced a regex and
made level-only queries 5× faster.

Since 2026-10-18 sources can supply **typed fields** and levels themselves
(`LogSource::schema` / `field` / `supplied_level`; DLT supplies `ecu`,
`apid`, `ctid`, … and the header level). The parser accepts any field name;
`Query::bind` resolves it against the source's schema and reports unknown
fields or wrong operators with their position. `Event` is lazy over
`(source, line)`, so header-only filters never fetch or render the line.

## 5. Measured performance

Measured for the scaffold (2026-10-17) on a 2-core cloud VM. Current numbers
on Windows, for text and DLT, are in `history.md` ("Next steps as of 2026-11-09") and the commit messages.

1 GiB synthetic log, 12.0 M lines, 2-core cloud VM, release build, warm page cache:

| Operation | Time | Throughput |
|---|---|---|
| Open + index | 0.22 s | ~4.4 GB/s |
| Regex `ERROR` (2.0 M hits) | 0.29 s | ~3.5 GB/s |
| Query `level >= ERROR AND raw ~ /…/` (prefilter path) | 0.08–0.3 s | 3–13 GB/s |
| Query `level >= WARN` (per-line path, 4.0 M hits) | 0.36 s | ~2.9 GB/s |
| Query with OR/NOT/level, no prefilter | 0.72 s | ~1.4 GB/s |

Spec §12 DoD: "open 1 GB", "UI responsive" (all work runs on background
threads with progress and cancel), and "<100 ms per chunk" (a 4 MiB chunk
takes ~1–3 ms) are met. "Works on Windows" is covered by the CI matrix
below.

## 6. Storage

| What | Where (now) | Later |
|---|---|---|
| Log data | Read-only memory map; never copied or modified. DLT lines are rendered on demand from the mapped messages (no materialized text) | Revisit only if regex search over decoded formats proves too slow |
| Line index | In memory | Optional on-disk index cache for multi-GB files |
| Bookmarks + notes | Sidecar `<log>.nerka-marks`, plain text `line<TAB>note` | Same model synced via `nerka-server` for collaboration |
| Search histories, prefs, query packs, presets | Own plain-text files in `%APPDATA%\nerka` / XDG config dir (not eframe persistence) | Packs stored with a project (open question) |

The sidecar is human-readable and diff-able, so people can already share
annotations by sending it alongside the log.

## 7. Repository layout (spec §10)

As of 2026-11-11 (review 2026-11-07 R18):

```
nerka/
  Cargo.toml            workspace, shared deps, lints, release profile
  core/                 nerka-core  — UI-free engine (library)
    src/input/          LogSource trait, open() by content, SourceInfo / query_warnings, schema (field merge)
      text.rs, records.rs, json.rs   plain text, multi-line records, JSON (records split per file)
      dlt/              DLT reader + writer, FIBEX (non-verbose)
      proto/            Protocol trait; DLT, SOME/IP, syslog; ksy/ (protocols from .ksy definitions)
      capture/          pcap, pcapng, MF4 (+ unpack cache), network layers, UDP port routes
      concat.rs, merged.rs   several files as one view; interleaved by time (--merge)
    src/engine/         LineIndex, search/Filter/Highlighter, within (search inside a filter result)
    src/dsl/            NFD parser + evaluator, auto (Auto mode: plain words + NFD)
    src/pack/           query packs: parser, Library, expand, write
    src/signal.rs       signals (@name) from captures, queries and lifecycles; src/plot.rs: kept plots
    src/lifecycle.rs    lifecycles (boots per device); src/check.rs: checks over them
    src/session.rs      a tab's state and jobs, shared by the GUI (filter, search, colours)
    src/bundle.rs       settings bundles (export / import, format migrations)
    src/time.rs         times: parsing, zones, local / UTC
    src/event.rs        lazy fields (level, source-supplied fields)
    src/bookmarks.rs    bookmarks + sidecar persistence
    src/history.rs      search history; src/preset.rs: presets
    examples/           test data: gen_log, gen_dlt, gen_multiline_dlt, gen_captures, dlt_to_pcap, dlt_to_mf4;
                        tools: probe_capture, ksy_cells, mutate_open (the gate's fuzz run)
  cli/                  nerka       — open | search | filter | lines | marks | expand | packs | check | lifecycles
                                      | export | import (several files as one view, --merge)
  ui/                   nerka-gui   — egui desktop viewer (view, library/selection, presets, prefs, signals,
                                      plots, checks, protocols, bundles, instances, config sync)
  examples/             protocols/ (.ksy definitions: dlt, sensor), packs/ (sample packs for the docs)
  tools/                screenshot.ps1 (GUI screenshot, Windows), review-gate.sh, bench.sh
  docs/dev/                  spec, this analysis, status / features / GUI decisions / history, designs, review/
  .github/workflows/    ci.yml: fmt, clippy -D warnings, tests on Linux + Windows, cargo deny, DCO;
                        release.yml: Linux + Windows archives, pre-release on a v* tag (actions pinned by commit)
```

## 8. Roadmap

**Phase 1 — MVP (spec §7), done in this scaffold:** open file, line
indexing, regex and DSL filtering, highlighting, bookmarks with notes, CLI.

**Phase 1.5 — klogg parity** (owner is happy with the base as of 2026-10-18; remaining items are on hold)
- Follow/tail mode: remap on growth and extend the index incrementally.
- Highlight rules and saved filters: **done differently** — query packs with colours, line colours per selected query, and presets (`search-manager.md`), instead of klogg's lists.
- Encoding detection (UTF-16, Latin-1).
- Block-compressed index (§3).
- Selection and copy of ranges; search history: **done**.
- Rotated logs (`app.log`, `app.log.1`, …) presented as one source: several files can be opened as one view, one after another (2026-10-20); recognising rotation by name isn't done.

**Phase 2 — formats, normalization & signals** (design: `sources-and-signals.md`)
- DLT from `.dlt` files: **done** (2026-10-18), shown as a table.
- DLT from pcap / pcapng and MF4: **done** (2026-10-20, own readers).
- Signals: **done** (2026-10-24): per-tab signals from explicit regex captures, events and states (`@x`, `@x.event`, `@x.changed`) in NFD (`core/src/signal.rs`), time as a signal (`core/src/time.rs`, DLT `@time`), the plot (`ui/src/plot.rs`). FIBEX for DLT non-verbose messages: **done** (2026-10-24, `core/src/input/dlt/fibex.rs`). Next: hits on the plot, typed FIBEX arguments as signals, the `nerka-db` format.
- `timestamp` / `service` fields, with time-range filters (`timestamp >= 10:42:00`).
- stdin source (`tail -f x | nerka-gui -`).

**Phase 3 — deployment & collaboration**
- `nerka-server` (axum): the same core behind HTTP/WebSocket, as a lightweight node.
- egui UI compiled to wasm, or a thin web UI, against the server.
- Shared bookmarks/notes/markers, with the sidecar model as the wire format.

## 9. Open questions

Kept in one place: `status.md` → "Decisions pending with the owner".
