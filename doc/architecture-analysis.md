# Nerka — Architecture Analysis & Decisions (v0.1 scaffold)

Companion to `nerka_architecture_implementation_spec_v_0.md`. This records
what the scaffold implements, the decisions taken on the spec's open
questions, where it deliberately deviates, and what comes next.

Date: 2026-10-17

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

## 2. UI framework (spec §13 open question)

You considered Qt/C++. Options compared against R1, R2 and R4:

| Option | Footprint | Huge-list rendering | Rust integration | Cost / risk |
|---|---|---|---|---|
| **Qt 6 / C++** | 30–60 MB of DLLs to ship | Excellent (`QAbstractItemModel`) — what klogg uses | FFI boundary (cxx-qt): two languages, two build systems | LGPL dynamic-linking duties or a commercial licence; heavy CMake/vcpkg setup on Windows |
| **Tauri** (spec default) | ~5–10 MB + system WebView2 | Needs a JS virtual list; every visible line crosses IPC as JSON | Good (commands/events) | Two stacks (Rust + TS/npm); IPC latency on fast scrolling |
| **egui / eframe** ✅ | One ~10–20 MB binary, no runtime | Immediate mode: paint only the visible rows directly from mmap, zero copies | Native Rust, same process | Less "native" look; you write custom widgets (done for the log view) |
| Slint | Small | OK | Native Rust | GPL/royalty licensing for commercial use; weaker for custom big views |
| Iced | Small | Fair | Native Rust | API still churning; less mature text rendering |

**Decision: egui for the desktop viewer.** It is the lightest option, which
was your explicit constraint, and the only one that reads lines straight
from the memory map into the GPU with no IPC or serialization in between.
That is what a klogg-class viewer needs. It also compiles to WebAssembly, so
a browser UI for the server/collaboration phase can reuse the same code.

**Deviation from spec §2/§4:** the spec names Tauri. The spec's *rules*
still hold: the UI does no filtering and no parsing, and only calls core
APIs. Spec §3.6 (Tauri commands) becomes plain Rust calls. If you prefer
Tauri, only `ui/` changes; `core/` and `cli/` are unaffected.

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
made level-only queries 5× faster. `timestamp`, `service` and `source` are
reserved: the parser rejects them with "not supported yet" until a
normalizer provides them.

## 5. Measured performance

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
| Log data | Read-only memory map; never copied or modified | Decoders (PCAP/DLT) materialize text lines into a temp file or arena |
| Line index | In memory | Optional on-disk index cache for multi-GB files |
| Bookmarks + notes | Sidecar `<log>.nerka-marks`, plain text `line<TAB>note` | Same model synced via `nerka-server` for collaboration |
| Saved filters, settings | — | `%APPDATA%\nerka` / XDG config dir (eframe persistence) |

The sidecar is human-readable and diff-able, so people can already share
annotations by sending it alongside the log.

## 7. Repository layout (spec §10)

```
nerka/
  Cargo.toml            workspace, shared deps, lints, release profile
  core/                 nerka-core  — UI-free engine (library)
    src/input/          LogSource trait, TextFile (mmap)
    src/engine/         LineIndex, search/Filter/Highlighter
    src/dsl/            NFD parser + evaluator
    src/event.rs        lazy field normalization (level)
    src/bookmarks.rs    bookmarks + sidecar persistence
    examples/gen_log.rs synthetic log generator for benchmarks
  cli/                  nerka       — open | search | filter | marks | lines
  ui/                   nerka-gui   — egui desktop viewer
  doc/                  spec + this analysis
  .github/workflows/    CI: fmt, clippy -D warnings, test, release build on Win/Linux/macOS
```

## 8. Roadmap

**Phase 1 — MVP (spec §7), done in this scaffold:** open file, line
indexing, regex and DSL filtering, highlighting, bookmarks with notes, CLI.

**Phase 1.5 — klogg parity**
- Follow/tail mode: remap on growth and extend the index incrementally.
- Multiple highlight rules with colours (klogg's "highlighters"), saved filters.
- Encoding detection (UTF-16, Latin-1).
- Block-compressed index (§3).
- Selection and copy of ranges; search history.
- Rotated logs (`app.log`, `app.log.1`, …) presented as one source.

**Phase 2 — formats & normalization**
- `input::dlt`, `input::pcap` adapters implementing `LogSource`.
- `timestamp` / `service` fields, with time-range filters (`timestamp >= 10:42:00`).
- stdin source (`tail -f x | nerka-gui -`).

**Phase 3 — deployment & collaboration**
- `nerka-server` (axum): the same core behind HTTP/WebSocket, as a lightweight node.
- egui UI compiled to wasm, or a thin web UI, against the server.
- Shared bookmarks/notes/markers, with the sidecar model as the wire format.

## 9. Open questions for you

1. Is egui instead of Tauri acceptable, or should the UI follow the spec literally?
2. Level detection only recognises upper-case tokens. Do your real logs use other conventions (`E/`, `<3>`, lower-case)? A few sample lines would let me tune the normalizer.
3. Should the sidecar live next to the log (current behaviour), or in a central app-data folder when the log directory is read-only?
