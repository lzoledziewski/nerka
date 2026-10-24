# nerka

A fast, lightweight log viewer and analyser for big files: plain-text logs
and automotive **DLT**, also when it was recorded inside **pcap / pcapng**
network captures or **MF4** bus-logging files.

Nerka started as "klogg, but on our own terms": klogg-class speed on
multi-gigabyte files, plus a small filter language, reusable query packs and
decoded DLT headers you can filter on. It runs locally, with no backend, as a
desktop viewer (`nerka-gui`) and a command-line tool (`nerka`) built on one
UI-free engine (`nerka-core`).

> Status: **alpha** (`0.1.0-alpha.4`). Windows is the main platform; Linux
> builds come from CI. File formats and the GUI still change.

## Features

- **Big files:** the file is memory-mapped and never modified or copied; a
  1 GiB log indexes in ~0.1 s and a typical filter runs in ~30 ms (release
  build, Windows).
- **Formats, detected by content:** plain text (UTF-8); DLT files with
  storage headers; DLT over UDP inside pcap / pcapng (VLAN, IPv4 / IPv6
  fragment reassembly) and MF4 (Ethernet bus events). DLT is shown as a table
  with resizable columns. Several files of one kind can be opened as one
  view, one after another.
- **Filter and Search:** the Filter decides which lines the main view shows;
  the Search finds lines among those, lists the hits in a bottom pane and
  colours them per query. Each box has an **Auto** mode (plain words are a
  phrase, filter syntax is a query, they mix), a **Text** mode (literal or
  regex) and a strict **NFD** mode, with history on ↑ / ↓.
- **NFD, the filter language:**
  `level >= ERROR AND raw ~ /SSL|CERT/i AND NOT /healthcheck/`, or on DLT
  `apid == NAV AND level >= WARN AND payload ~ "timeout"`.
- **Query packs:** reusable queries in plain-text `.nerka-pack` files, with
  parent → child refinement, labels, parameters (`$cam`) and colours. Pick
  them in the library pane; they expand into plain NFD. Selections can be
  saved as **presets**, and each picked query can colour the lines it keeps.
- **Bookmarks with notes** (`m`, `b` / `B`), stored next to the log as
  `<log>.nerka-marks`.
- **Tabs and windows:** one tab per file; opening a file from the shell hands
  it to the nerka window focused last; windows can be named.

## Install

Pre-release builds for Windows (`.zip`) and Linux (`.tar.gz`, glibc 2.35+)
are on the [Releases](https://github.com/lzoledziewski/nerka/releases) page.
They are unsigned, so Windows SmartScreen may warn on first start.

To build from source you need a stable Rust toolchain
([rustup.rs](https://rustup.rs)); on Windows, the MSVC toolchain with the
"Desktop development with C++" build tools.

```
cargo build --release --workspace
```

The binaries are `target/release/nerka-gui` and `target/release/nerka`.

## Usage

### Desktop viewer

```
nerka-gui app.log                              # open a file (several: one view)
nerka-gui trace.pcapng --query 'apid == NAV'   # open and filter
nerka-gui app.log --new-window --name triage   # a separate, named window
nerka-gui app.log --signal 'lat=/latency_us=(?<lat>\d+)/' --query '@lat > 9000'   # a signal
nerka-gui app.log --signal 't:time=/^(?<t>\S+ \S+)/' --signal 'lat=/latency_us=(?<lat>\d+)/' --plot lat   # plot it over time
```

| Keys | |
|---|---|
| `Ctrl+O` | open files (or drop them on the window) |
| `Ctrl+F`, `Enter` | focus the Filter box, run it |
| `n` / `N` | next / previous search hit |
| `Ctrl+G` | go to line |
| `Enter` on a line | show the whole line (DLT: header fields and message) |
| `Ctrl+C` | copy the selection, or the current line |
| `Ctrl+click`, `Ctrl+Space` | pick single lines to copy |
| `m`, `b` / `B` | bookmark, next / previous bookmark |
| `Ctrl+Shift+L` | library and selection panes (query packs) |
| `Ctrl+Tab`, `Ctrl+W` | next tab, close tab |
| `Ctrl+,`, `F1` | settings, about |

### Command line

```
nerka filter 'level >= ERROR AND raw ~ /SSL|CERT/' app.log
nerka search -i 'timeout|refused' app.log
nerka search --in 'level >= WARN' -c timeout app.log     # search inside a filter
nerka filter 'apid == NAV AND level >= WARN' trace.dlt
nerka lines app.log 100 120
nerka --pack packs/ expand -l ota -p cam=CAM1            # pack picks -> NFD
nerka --signal 'gear:enum=/gear=(?<gear>\w+)/' filter '@gear == D' app.log   # signals
nerka --fibex ecu1.xml filter 'apid == NAV' trace.dlt     # DLT non-verbose messages decoded
```

`nerka help <command>` lists every option.

### NFD at a glance

| | |
|---|---|
| Comparisons | `== != >= <= > <`; levels compare by severity (`level >= WARN`) |
| Regex | `field ~ /re/`, `field !~ /re/`, `/re/i` ignores case |
| Text | bare `/re/` and `"text"` match the whole line (`raw`) |
| Logic | `AND OR NOT`, parentheses |
| Fields | `raw`, `level`; DLT adds `ecu apid ctid type mcnt tmsp seid mid mode payload`; several files add `file` |
| Signals | `@name`: a value captured by a named regex group — of a picked query (include or the `~` signal role), or a signal you define (📈 Signals panel, `--signal`; saved). `@gear == D` is the state (held until the next event), `@gear.event == D` a line that carries it, `@gear.changed` a change. Kinds `num`, `enum`, `time`; DLT has `@time` (receive time): `@time >= "2025-10-18 10:00:00"` |

## Documentation

- [doc/status.md](doc/status.md): current state, decisions and next steps.
- [doc/architecture-analysis.md](doc/architecture-analysis.md): architecture, decisions and deviations from the
  [original spec](doc/nerka_architecture_implementation_spec_v_0.md).
- [doc/search-manager.md](doc/search-manager.md): query packs, expansion, library and selection.
- [doc/search-modes.md](doc/search-modes.md): Auto, Text and NFD modes.
- [doc/sources-and-signals.md](doc/sources-and-signals.md): input formats and signals (what a signal is, what's next).
- [CLAUDE.md](CLAUDE.md): guide for contributors and AI agents (commands, rules).

## License

Licensed under either of [Apache License 2.0](LICENSE-APACHE) or
[MIT license](LICENSE-MIT), at your option.
