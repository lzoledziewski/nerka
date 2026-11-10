# Sources & Signals — design

_Status: agreed direction. Written 2026-10-17. **Signals** (2026-10-24): per-tab signals from explicit captures, events and states in NFD (`@x`, `@x.event`, `@x.changed`), time as a signal (`@time` on DLT, `time` captures), the signal plot; see "What a signal is" and `features.md` → "Signals". Implemented so far: **DLT from `.dlt` files** (2026-10-18: source-supplied fields, DSL field predicates, own reader, table view) and **DLT inside pcap / pcapng and MF4** (2026-10-20: own readers in `core/src/input/capture/`, no dependency). Signals, SOME/IP, the time model and the plugin crates are still design. Current state and decisions: `status.md`._

## Goal

Two new capabilities beyond plain-text logs:

1. **DLT**: DLT messages shown as log rows. They can come directly from a `.dlt` file, or embedded in **MF4** (bus-logging channels) or **pcap/pcapng** (DLT over UDP/TCP).
2. **Signals**: time series monitored alongside the log.
   - Initially from **SOME/IP**, decoded with a **FIBEX or ARXML** database. The user **selects the signals to monitor**; we don't decode everything. Browsing the full catalog is tentative.
   - Later from other producers: embedded state machines, regex extractors, other plugins.

## What a signal is (owner, 2026-10-24)

A **signal** is a named value that some rows of a log carry, defined **per tab**.

- **Where it comes from (owner, 2026-10-24):** three sources. (1) **Pack queries**: the named groups of the queries in the selection with role Include or the Signal role (a role that doesn't filter); picking the query is the explicit step. (2) **Signals the user defines** in the Signals panel, saved (`<config>/nerka/signals`) for every tab. (3) **Imports**: FIBEX (2026-10-24: every typed argument of a described DLT non-verbose message, listed as a tree in the Signals panel, picked or plotted without a regex); ARXML later. The file's own time is the built-in `@time`. Kinds of query signals are inferred from the values, a pack can pin them (`signals: t:time`), the panel can override them.
- **Scope (owner):** per signal, `all` lines or only the `filtered` ones (what the filter keeps; panel and plot). Only `all` signals can be used in a filter.
- **Definition:** name, kind, an NFD query, and the regex group to take the value from (by default the signal's name). A row carries the value when the query matches it and the group captured something.
- **Kinds:** `num` (the capture read as a number: decimal, float or `0x` hex) and `enum` (the captured text). A capture that doesn't read as a number gives the row no value.
- **First use: filtering.** In NFD a signal is written with `@`, so it can't be mistaken for a source field: `@speed > 100`, `@gear == D`, `@gear ~ /D|N/`. Like a field, a row without a value only satisfies `!=` and `!~`. A query that names a signal works only in a tab that defines it; elsewhere `@speed` is a bind error ("unknown signal"), the same way `apid` is on a plain-text file.
- **Events and states (owner, 2026-10-24).** A row that carries a value is an **event** (a data event: the value was received there). The **state** is the value the signal holds: each event updates it, and it holds until the next event. On the event row itself the state is already the new value. Both can be queried:

  | NFD | Meaning |
  |---|---|
  | `@gear == D` | the state (a bare `@gear` is the state) |
  | `@gear.state == D` | the same, spelled out |
  | `@gear.event == D` | an event with value D on this row; `@gear.event` alone: any event |
  | `@gear.changed` | an event that changed the state (the first event too); with a comparison, on the new value |

  Before the first event a row has no state. In a view of several files the state doesn't carry over from one file into the next. Engine: one pass per signal and file collects its events (row, value), cached with the signal's definition; the state at a row is the last event at or before it (binary search), so every condition stays per row and parallel.
- **Time** is a signal too (owner, 2026-10-24): on DLT the receive (storage) time is the built-in `@time`; on plain text the user captures it with a signal of kind `time`. The tab's clock is the source's time, else its first `time` signal's state at each line, else line numbers. Built: `status.md` → "Signals".
- **Plotting** is a separate feature built on top: the state over time (a step line, enum as bands), with a per-signal checkbox to show its events (when the value was received), and search and filter hits as markers on the same time axis. Built (2026-10-24) except the hit markers.
- **Not now:** export, comparing signals across files, derived signals (rates, state machines), signals defined from other signals.

## Decisions taken with the owner

| Topic | Decision |
|---|---|
| Signal value types | **Numeric and enum only** for now. No structs, arrays or strings. |
| Live monitoring (UDP/TCP capture) | **Not now.** Recorded files only, so stores may stay immutable for v1. Keep the traits append-friendly so this can be added later. |
| Defining derived signals | **In the DSL.** The owner isn't fully convinced; keep the syntax small and revisit it once it's in use. |
| Plugins | In-tree crates behind cargo features first. External plugins (WASM or a C ABI) later. |
| DLT parser (2026-10-18) | **Own zero-copy reader, no crate** (not `dlt-core` / `dlt_parse`). Lives in `core/src/input/dlt/` for now; becomes `nerka-dlt` when the `Container` / `Decoder` traits exist. |
| DLT display (2026-10-18) | A table with aligned, resizable columns in nerka's own view; no column sorting. |
| FIBEX (2026-10-24) | FIBEX first (SOME/IP, if it comes, from ARXML). `quick-xml` (MIT, needs only `memchr`) for FIBEX and later ARXML, not `roxmltree` / `autosar-data`. First step: decode DLT non-verbose messages from files given for the session; the `nerka-db` intermediate format (import once, library or session) after that. Built: `status.md` → "FIBEX". |

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

_Superseded for v1 by "What a signal is" above: signals are defined per tab outside NFD, and NFD only refers to them with `@name`. Kept as ideas for derived signals later._

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

**Built so far (2026-11-01):** the decoder side as `input::proto::Protocol` (DLT, SOME/IP headers, syslog), routed by UDP port, with one source type for every container (`input::proto::Records`); see `history.md` → "Next steps as of 2026-11-09". Containers are still functions of `input::capture` (pcap, pcapng, MF4) and the `.dlt` reader, not a trait yet.

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
| pcap/pcapng | ~~`pcap-parser` 0.17~~ | **Not used**: own reader (`capture/pcap.rs`, 2026-10-20). |
| Ethernet/IP/UDP | ~~`etherparse` 0.21~~ | **Not used**: own parsing with fragment reassembly (`capture/net.rs`). |
| SOME/IP headers | `someip_parse` 0.7 | Doesn't interpret payloads: that is `SignalDb`'s job. |
| ARXML | `autosar-data` 0.23 | |
| FIBEX | ~~`roxmltree` 0.21~~ `quick-xml` 0.42 + own mapping | **Used** (2026-10-24): streaming, needs only `memchr`. |
| MF4 | ~~`mdf4-rs`, `mf4-rs`, `falcon_mdf`, `mdflib`, `mdfr`~~ | **Not used**: own reader for bus-logging channels (`capture/mf4.rs`, 2026-10-20). |
| Deflate (MF4 DZ blocks) | `miniz_oxide` 0.9 | **Used** (2026-10-26): pure Rust, MIT / Zlib / Apache-2.0, no dependencies of its own that aren't already built (the GUI's image decoding uses it). Streaming inflate into the unpack cache (`capture/unpack.rs`). Later for `.gz` captures too. |
| Signal plots (GUI) | `egui_plot` 0.37 | Matches the current egui version. |

Each one must be justified in its PR (spec §3.1).

## UI

- Signal panel: `egui_plot` lanes, one per subscribed signal, with a shared time cursor. Selecting a log row moves the cursor and clicking the plot jumps to the nearest row. Enum signals are drawn as step or state bands.
- Signal picker: search the `SignalDb` catalog by name, service or PDU and subscribe. A full tree browser is tentative.
- Per tab, like the filter and bookmarks: subscriptions and derived-signal definitions belong to the tab.

## Suggested order

1. Time model and source-supplied fields in `Event`, with DSL field predicates. Use plain-text timestamps as the first user. — **Fields and predicates done** (`LogSource::schema` / `field` / `supplied_level`, `Query::bind`); the `Timestamp` / clock model is **not** done (DLT exposes `tmsp` as a number, `time` is display-only).
2. DLT from `.dlt` files, with the header-only index fast path. — **Done** in `core/src/input/dlt/` (index = message offsets; headers read in place).
3. DLT over UDP in pcap / pcapng. — **Done** in `core/src/input/capture/` (not a separate crate yet).
4. `SignalStore`, `egui_plot` panel, regex extractor in the DSL (the first signal producer, with no database needed). — **Regex captures done** as per-tab signal definitions with events and states in NFD (2026-10-24); the store is the per-file event list cached with each definition (`SignalDef::series`); no plot yet.
5. `SignalDb`: ARXML, then FIBEX. `nerka-someip` with subscriptions.
6. MF4 bus-logging channels. — **Done** for Ethernet and DLT channels with an own reader. State machines in the DSL: open.
7. Later: live capture, external plugin ABI.
