---
title: "Getting started"
description: "Install nerka on Windows or Linux, open a log, a DLT file or a capture, and run a first filter and search in the desktop viewer or on the command line."
---

# Getting started

## Install

Download the archive for your system from the
[releases page](https://github.com/lzoledziewski/nerka/releases/latest):

- **Windows:** `nerka-…-windows-x86_64.zip`. Unpack it anywhere; there is
  no installer. The binaries are unsigned, so SmartScreen may warn the first
  time.
- **Linux:** `nerka-…-linux-x86_64.tar.gz` (glibc 2.35 or newer, e.g.
  Ubuntu 22.04).
- **macOS (Apple Silicon):** `nerka-…-macos-aarch64.tar.gz`. The binaries
  are unsigned and not an app bundle: after unpacking, clear the download
  mark once (`xattr -dr com.apple.quarantine nerka-…-macos-aarch64`), then
  start `./nerka-gui` from Terminal. macOS builds are tested automatically
  but not tried by hand.

Each archive holds `nerka-gui` (the desktop viewer), `nerka` (the command
line tool), the README, the licences, `examples/` (a demo trace, sample
queries for the Library and protocol definitions) and this guide (`docs/`).
`SHA256SUMS` next to the archives on the release page checks them:
`sha256sum -c SHA256SUMS`. To build nerka from source, see
[CONTRIBUTING.md](https://github.com/lzoledziewski/nerka/blob/main/CONTRIBUTING.md).

## Open a file

Start `nerka-gui` and drop a file on the window, or use **Open…**
(`Ctrl+O`), or start it with the file:

```
nerka-gui app.log
nerka-gui trace.dlt trace.pcapng          # several files: one view
```

nerka recognises a file by its content: plain text, JSON, DLT, pcap,
pcapng or MF4 ([Files and formats](formats.md)). The file is mapped, never
modified, and indexed once; the status line says how long that took.

## Filter, then search

The **Filter** box at the top decides which lines the view shows. Type
words to find them, or a query:

```
level >= ERROR
timeout AND level >= WARN
apid == NAV AND payload ~ /speed=\d+/
```

Press `Enter`. The line under the box says how it was read: as text or as a
query ([Queries](queries.md)).

The **Search** box at the bottom finds lines among those the filter kept,
lists them in the pane below and colours them in the view; `n` / `N` jump
between hits.

## Try the demo

The archive's `examples/demo.dlt` is a made-up 20-minute drive: three
devices, one of them booting three times (and crashing once), a speed, a
gear, a temperature that makes a chip throttle, a battery voltage, an update
in progress, and a few bookmarks. `examples/packs/demo.nerka-pack` holds
queries for it. Open both, with the speed and temperature plotted:

```
nerka-gui examples/demo.dlt --pack examples/packs/demo.nerka-pack --library --signal-pick demo.speed --signal-pick demo.temperature --plot speed --plot temp
```

Then try the [Library](library.md#try-it) and the [plot](signals.md#plots):
click a point in the plot to jump to its line, right-click to place time
cursors. (In a source checkout, make the trace first:
`cargo run -p nerka-core --example gen_demo -- examples/demo.dlt`.)

## The same from the command line

```
nerka filter 'level >= ERROR AND raw ~ /SSL|CERT/' app.log
nerka search -i 'timeout|refused' app.log
nerka open trace.pcap                      # what the file is: lines, fields, protocols
nerka lines app.log 100 120
```

See [the command line](cli.md) for every command.

## Next

- Keep queries you use again in the [Library](library.md).
- Plot values from your lines: [signals and plots](signals.md).
- Follow a device's boots and check them: [lifecycles](lifecycles.md),
  [checks](checks.md).
