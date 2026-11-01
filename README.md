# nerka

A fast, lightweight log viewer and analyser for big files: plain-text logs
and automotive **DLT**, also when it was recorded inside **pcap / pcapng**
network captures or **MF4** bus-logging files.

Open a multi-gigabyte trace in a second, filter it with a small query
language, keep the queries you use in shareable packs, and plot the values
your logs carry. Everything runs locally, with no backend: a desktop viewer
(`nerka-gui`) and a command-line tool (`nerka`) on one UI-free engine
(`nerka-core`).

![nerka on a synthetic DLT drive: warnings filtered from a query pack, speed, temperature and battery voltage plotted with a throttling threshold, a text search (regex, ignoring case) for throttling messages in the bottom pane, and the library and selection panes on the right](doc/img/nerka.png)

> Status: **0.1.0-alpha.9**, a pre-release before 0.1.0. Windows is the main
> platform; Linux builds come from CI; macOS isn't built yet. While nerka is
> at 0.x, file formats, the filter language and the GUI can still change.

## Features

- **Big files:** the file is memory-mapped and never modified or copied; a
  1 GiB log indexes in ~0.1 s and a typical filter runs in ~30 ms (release
  build, Windows).
- **Formats, detected by content:** plain text (UTF-8); DLT files with
  storage headers; captures in pcap / pcapng (VLAN, IPv4 / IPv6 fragment
  reassembly) and MF4 (Ethernet bus events, compressed data too) with
  **DLT**, **SOME/IP** (headers) and **syslog** over UDP, picked by port.
  Messages are shown as a table with resizable columns; a capture with
  several protocols shares one table (`proto == SOMEIP AND service == 0x1234`). Several files of one kind
  can be opened as one view, one after another, and reordered.
- **Filter and Search:** the Filter decides which lines the main view shows;
  the Search finds lines among those, lists the hits in a bottom pane and
  colours them per query. Each box has an **Auto** mode (plain words are a
  phrase, filter syntax is a query, they mix), a **Text** mode (literal or
  regex) and a strict **NFD** mode, with history on ↑ / ↓.
- **NFD, the filter language:**
  `level >= ERROR AND raw ~ /SSL|CERT/i AND NOT /healthcheck/`, or on DLT
  `apid == NAV AND level >= WARN AND payload ~ "timeout"`.
- **Query packs and presets:** reusable queries in plain-text `.nerka-pack`
  files, with parent → child refinement, labels, parameters (`$cam`) and
  colours. Pick them in the library pane; they expand into plain NFD, and
  each picked query can colour the lines it keeps. A selection can be saved
  as a **preset**.
- **Signals and plots:** a value captured by a named regex group is a
  signal (`@speed > 100`, `@gear.changed`); plot signals over time in lanes,
  with time cursors (Δt), value markers and bookmarks, and keep plots by
  name to use them on other files.
- **FIBEX:** DLT non-verbose messages decoded from FIBEX files; their
  arguments can be picked as signals and plotted, from a searchable tree.
- **Bookmarks with notes** (`m`, `b` / `B`), stored next to the log as
  `<log>.nerka-marks`. Notes show in the views (on the line before, at the
  end of the line or as a column; copying takes them along as `// note`),
  and `bookmark` is a field in queries:
  `level >= ERROR OR bookmark` keeps your marked lines in a filter.
- **Tabs and windows:** one tab per file (or set of files); opening a file
  from the shell hands it to the nerka window focused last; windows can be
  named.

## Install

Builds for Windows (`.zip`) and Linux (`.tar.gz`, glibc 2.35+) are on the
[Releases](https://github.com/lzoledziewski/nerka/releases) page. They are
unsigned, so Windows SmartScreen may warn on first start.

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
nerka --someip-port 30501 filter 'proto == SOMEIP AND type == ERROR' trace.pcap   # SOME/IP, syslog in captures, by UDP port
nerka lines app.log 100 120
nerka --pack packs/ expand -l ota -p cam=CAM1            # pack picks -> NFD
nerka --signal 'gear:enum=/gear=(?<gear>\w+)/' filter '@gear == D' app.log   # signals
nerka --fibex ecu1.xml filter 'apid == NAV' trace.dlt     # DLT non-verbose messages decoded
nerka --local-time lines trace.dlt 1 20                  # local time (CLI: UTC by default; GUI: local, ⚙ Settings)
nerka --signal 'up1=ecu == ECU1 AND tmsp ~ /(?<up1>\d+)/' filter '@up1.changed AND @up1 < 10000' trace.dlt   # ECU1 resets
```

`nerka help <command>` lists every option.

### NFD at a glance

| | |
|---|---|
| Comparisons | `== != >= <= > <`; levels compare by severity (`level >= WARN`) |
| Regex | `field ~ /re/`, `field !~ /re/`, `/re/i` ignores case |
| Text | bare `/re/` and `"text"` match the whole line (`raw`) |
| Logic | `AND OR NOT`, parentheses |
| Fields | `raw`, `level`; DLT adds `ecu apid ctid type mcnt tmsp seid mid mode payload`, SOME/IP `service method client session iface type rc len payload`, syslog `facility host app pid msgid stamp payload`, captures `proto`; several files add `file` |
| Bookmarks | `bookmark` alone: a bookmarked line (`level >= ERROR OR bookmark` keeps your marked lines); `bookmark == B2` its name; `bookmark ~ /fan/` its name or note |
| Signals | `@name`: a value captured by a named regex group — of a picked query (include or the `~` signal role), or a signal you define (📈 Signals panel, `--signal`; saved). `@gear == D` is the state (held until the next event), `@gear.event == D` a line that carries it, `@gear.changed` a change. Kinds `num`, `enum`, `time`; DLT has `@time` (receive time): `@time >= "2025-10-18 10:00:00"` |

## Documentation

- [doc/status.md](doc/status.md): current state, decisions and next steps.
- [doc/architecture-analysis.md](doc/architecture-analysis.md): architecture, decisions and deviations from the
  [original spec](doc/nerka_architecture_implementation_spec_v_0.md).
- [doc/search-manager.md](doc/search-manager.md): query packs, expansion, library and selection.
- [doc/search-modes.md](doc/search-modes.md): Auto, Text and NFD modes.
- [doc/sources-and-signals.md](doc/sources-and-signals.md): input formats and signals (what a signal is, what's next).
- [CLAUDE.md](CLAUDE.md): guide for contributors and AI agents (commands, rules).

## Contributing

Issues and pull requests are welcome: see [CONTRIBUTING.md](CONTRIBUTING.md). Every commit needs a
DCO sign-off (`git commit -s`). Security problems: please report them privately, see
[SECURITY.md](SECURITY.md). Conduct: [CODE_OF_CONDUCT.md](CODE_OF_CONDUCT.md).

## License

Licensed under either of [Apache License 2.0](LICENSE-APACHE) or
[MIT license](LICENSE-MIT), at your option. Unless you state otherwise, any contribution you submit
is dual-licensed the same way, without additional terms.

The core and the apps stay open source under these licences. Extensions
(plugins) may also be offered separately, some possibly closed source; the
plugin interface, once it exists (it isn't built yet), is the boundary
between them.
