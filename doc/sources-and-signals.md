# Sources & Signals — design

_Status: agreed direction. Written 2026-10-17. Updated 2026-10-18: **DLT from `.dlt` files is implemented** (source-supplied fields, DSL field predicates, own reader, table view); everything else here is still design. Current state and DLT decisions: `status.md`._

## Goal

Two new capabilities beyond plain-text logs:

1. **DLT**: DLT messages shown as log rows. They can come directly from a `.dlt` file, or embedded in **MF4** (bus-logging channels) or **pcap/pcapng** (DLT over UDP/TCP).
2. **Signals**: time series monitored alongside the log.
   - Initially from **SOME/IP**, decoded with a **FIBEX or ARXML** database. The user **selects the signals to monitor**; we don't decode everything. Browsing the full catalog is tentative.
   - Later from other producers: embedded state machines, regex extractors, other plugins.

## Decisions taken with the owner

| Topic | Decision |
|---|---|
| Signal value types | **Numeric and enum only** for now. No structs, arrays or strings. |
| Live monitoring (UDP/TCP capture) | **Not now.** Recorded files only, so stores may stay immutable for v1. Keep the traits append-friendly so this can be added later. |
| Defining derived signals | **In the DSL.** The owner isn't fully convinced; keep the syntax small and revisit it once it's in use. |
| Plugins | In-tree crates behind cargo features first. External plugins (WASM or a C ABI) later. |
| DLT parser (2026-10-18) | **Own zero-copy reader, no crate** (not `dlt-core` / `dlt_parse`). Lives in `core/src/input/dlt/` for now; becomes `nerka-dlt` when the `Container` / `Decoder` traits exist. |
| DLT display (2026-10-18) | A table with aligned, resizable columns in nerka's own view; no column sorting. |

## Layered model

```
 Containers / transports       Protocol decoders            Stores           Derivations
 .dlt file ──────┐
 .pcap/.pcapng ──┤  frames     DLT decoder ──── events ──▶ EventStore ──┬─▶ regex extractor ──┐
 .mf4 (bus/eth) ─┼─────────▶                                            ├─▶ state machine ────┤
 (live: later) ──┤             SOME/IP decoder ─ signals ─▶ SignalStore ◀┴─────────────────────┘
 plain text ─────┘                  ▲                         ▲
                             SignalDb ── FIBEX / ARXML        └── subscribed signals only
```

1. **`Container`** unpacks a file into **frames**: `(Timestamp, channel/bus id, protocol hint, payload bytes, source offset)`.
   - `.dlt`: one frame per storage-header message.
   - pcap: UDP/TCP payloads, filtered by port and protocol rule (`etherparse`).
   - MF4: Ethernet or CAN bus-logging records.
   - Plain text stays a special case and keeps its fast mmap path.
2. **`Decoder`** turns frames into **events**, **signal samples**, or both. A single pass over a container feeds every decoder that is registered for it (one pcap pass can drive both DLT and SOME/IP).
3. **Stores**
   - `EventStore` is the evolution of `input::LogSource`: rows addressed by `LineNo`, with `raw` (the rendered text) plus **typed fields that the source supplies**.
   - `SignalStore` holds columnar samples per signal, `Vec<(Timestamp, Value)>` with `Value = F64 | Enum(u32)`, plus metadata: name, unit, value table and origin.
4. **`Extractor`** derives signals from events (regex capture, state machine, counter or rate). It runs in the engine with `&Progress` and checks `is_cancelled()` per chunk. Its output goes into the same `SignalStore`, so the UI doesn't distinguish where a signal came from.

## Core changes required

- **Source-supplied fields.** A DLT event already knows its ECU, APID, CTID, level, message type, timestamps and counter, so the source supplies these instead of `detect_level` guessing from the text. The DSL gets field predicates such as `ecu == "ECU1" AND apid == "NAV"`. `raw` is never lost.
- **Fast path for decoded sources.** DLT has no contiguous text buffer, so search uses the per-line path. To keep that fast:
  - Index the frame offset of every message.
  - Answer header-only filters (`apid`, `level`, `ecu`) from the index without decoding.
  - Decode and render payloads lazily: only when a regex or the view needs them.
  - The CLAUDE.md rule still applies: every fast path is tested against the naive per-line `run_filter`.
- **Time model.** `Timestamp { ns: i64, clock: ClockId }`, with one offset per clock domain. DLT has both the ECU uptime and the logger storage time, and pcap and MF4 have their own capture clocks. This is needed to merge several sources into one timeline and to sync the log cursor with signal plots. It also covers the existing roadmap item "`timestamp` field + time-range filters".
- **One `SignalDb` for both protocols.** FIBEX describes SOME/IP services *and* DLT non-verbose messages. Build one database (service/method/event id → payload layout → signals) that both decoders query.
  - ARXML: `autosar-data`.
  - FIBEX: hand-written on `roxmltree`. No maintained crate exists.
- **Subscription model.** Browsing the catalog is cheap. Decoding everything is not. The user subscribes to signals, the SOME/IP decoder builds a `(service, method/event) → layouts` lookup from the subscription, and every other payload is skipped after one hash lookup.

## DSL sketch for derived signals (tentative)

```
signal speed   = /speed=(\d+)/ as f64            # regex capture → numeric
signal state   = states {                         # state machine → enum
    IDLE    on /engine stopped/
    RUNNING on /engine started/
    ERROR   on level >= ERROR AND apid == "ENG"
}
signal errrate = rate(level >= ERROR, 1s)        # events per window
```

Constraints: the grammar comment in `dsl/mod.rs` and parser tests with error positions apply (CLAUDE.md). Keep signal definitions separate from filter expressions, so that a filter remains a pure predicate.

## Plugin surface

The traits `Container`, `Decoder`, `SignalDbLoader` and `Extractor`. They exchange **batches** (of frames, events or samples), never single items, so a future WASM or C-ABI boundary stays cheap. Planned crates:

| Crate | Contents |
|---|---|
| `nerka-dlt` | DLT container and decoder; non-verbose messages via `SignalDb` |
| `nerka-pcap` | pcap/pcapng container |
| `nerka-mf4` | MF4 container (bus-logging channels) |
| `nerka-someip` | SOME/IP decoder, FIBEX/ARXML loaders |

## Candidate dependencies

All checked on crates.io on 2026-10-17. All MIT and/or Apache-2.0.

| Purpose | Crate | Notes |
|---|---|---|
| DLT | ~~`dlt-core` 0.20~~, ~~`dlt_parse` 0.10~~ | **Not used**: own reader (owner, 2026-10-18). |
| pcap/pcapng | `pcap-parser` 0.17 | |
| Ethernet/IP/UDP | `etherparse` 0.21 | |
| SOME/IP headers | `someip_parse` 0.7 | Doesn't interpret payloads: that is `SignalDb`'s job. |
| ARXML | `autosar-data` 0.23 | |
| FIBEX | `roxmltree` 0.21 + own mapping | |
| MF4 | **open**: `mdf4-rs` 0.6, `mf4-rs` 3.6, `falcon_mdf` 0.7, `mdflib` (C++ bindings), `mdfr` (GitHub only, license unchecked) | All are young. Evaluate them on real files, especially their support for bus-logging channels. |
| Signal plots (GUI) | `egui_plot` 0.37 | Matches the current egui version. |

Each one must be justified in its PR (spec §3.1).

## UI

- Signal panel: `egui_plot` lanes, one per subscribed signal, with a shared time cursor. Selecting a log row moves the cursor and clicking the plot jumps to the nearest row. Enum signals are drawn as step or state bands.
- Signal picker: search the `SignalDb` catalog by name, service or PDU and subscribe. A full tree browser is tentative.
- Per tab, like the filter and bookmarks: subscriptions and derived-signal definitions belong to the tab.

## Suggested order

1. Time model and source-supplied fields in `Event`, with DSL field predicates. Use plain-text timestamps as the first user. — **Fields and predicates done** (`LogSource::schema` / `field` / `supplied_level`, `Query::bind`); the `Timestamp` / clock model is **not** done (DLT exposes `tmsp` as a number, `time` is display-only).
2. DLT from `.dlt` files, with the header-only index fast path. — **Done** in `core/src/input/dlt/` (index = message offsets; headers read in place).
3. `nerka-pcap`: DLT over UDP.
4. `SignalStore`, `egui_plot` panel, regex extractor in the DSL (the first signal producer, with no database needed).
5. `SignalDb`: ARXML, then FIBEX. `nerka-someip` with subscriptions.
6. `nerka-mf4` once a crate has been chosen. State machines in the DSL.
7. Later: live capture, external plugin ABI.
