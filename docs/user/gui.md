---
title: "The desktop viewer"
description: "The nerka desktop viewer: tabs, the Filter and Search boxes, the line window, bookmarks with notes, several files as one view, keyboard shortcuts and startup options."
---

# The desktop viewer

`nerka-gui` shows one **tab** per file, or per set of files opened as one
view. Each tab has its own filter, search, positions, bookmarks, signals
switched on and checks switched on.

## The two boxes

- **Filter** (at the top) decides which lines the **main view** shows:
  only the lines it keeps, or every line with no filter. **Clear** shows
  every line again (↑ in the box brings the filter back).
- **Search** (at the bottom) looks only among the lines the filter kept.
  Its hits are listed in the pane below it (click one to go there) and
  coloured in the view; `n` / `N` jump to the next / previous hit.

Each box has a **mode**:

| Mode | Reads the box as |
|---|---|
| **Auto** (default) | plain words are an exact phrase, filter syntax is a query, and they mix (`timeout AND level >= WARN`); a box that isn't a valid query is text |
| **Text** | literal text, or a regex with **Regex** ticked |
| **NFD** | a query only; on an error, **Search as text instead** |

**Ignore case** and **Invert** (the Filter box) apply to text and phrases.
A line under the box previews how it will be read (`→ text: …`,
`→ query: …`). Nothing runs until `Enter` or the button: ticking a box or
picking a mode only changes the box. The last mode picked is the default
for new tabs. 🕘 lists each box's history; picking an entry restores its
mode. See [Queries](queries.md) for the language and for how Auto reads
words.

## Lines

- **Enter** on a line (or the right-click menu's **Show whole line…**)
  opens the line window: for DLT and captures the header fields as a grid,
  then the message with its own line breaks and tabs; **Copy message** /
  **Copy line**. Long lines also scroll sideways (Shift+wheel, ← / →).
- **Selecting:** drag, Shift+click, double-click (a word), Shift+arrows,
  `Ctrl+A`. `Ctrl+C` copies the selection, or the current line.
- **Picked lines:** `Ctrl+click` (or `Ctrl+Space`) picks single lines
  anywhere; `Ctrl+C` copies them in file order. They stay when the filter
  changes.
- **Go to line:** `Ctrl+G`. A line the filter hides lands on the next kept
  one.

## Bookmarks

`m` toggles a bookmark on the current line; `b` / `B` go to the next /
previous one. The 🔖 Bookmarks panel lists them with their names (`B1`,
`B2`, … renamable) and **notes**; notes can show in the views (on a line of
their own above the line, or at its end). Bookmarks are saved next to the
log as `<log>.nerka-marks` and are anchored to their lines' content, so
they find their lines again in a changed file (one that can't is marked
`?`). In queries, `bookmark` is a field: `level >= ERROR OR bookmark`,
`bookmark ~ /fan/`.

## Several files

Files opened together (multi-select in **Open…**, dropping several,
`nerka-gui a.dlt b.pcap`) become **one tab**, one after another in the
order given. The tab's menu → **Files…** lists them: move, remove or add
files, turn protocols on or off per capture, pick a text file's
[profile](formats.md#profiles), move a file's times by an
[offset](formats.md#a-files-offset); below the list, how each file's time
was read. A text file whose lines have times shares a tab with DLT files
and captures. **Sort by time** in the
menu sorts all their lines by time (files that know it; lines of one time
in the order of the files, then of each file), for one file too. Files
that can't be sorted (one with lines but no times) leave the box
unticked, and the status bar names them; a file without lines doesn't
count. A field `file` tells the files apart in queries. See
[Files and formats](formats.md).

A tab's menu → **Reload** reads its files again from disk, keeping
the filter, search, selection and the line you are on.

## Panels and windows

| Button | Opens |
|---|---|
| 📚 Library (`Ctrl+Shift+L`) | the Library of queries and this tab's selection ([The Library](library.md)) |
| 📈 Signals | signals and lifecycles of this tab ([Signals](signals.md), [Lifecycles](lifecycles.md)) |
| ✔ Checks | checks and their results ([Checks](checks.md)) |
| 🧩 Patterns | this tab's lines grouped by message, in a window of its own ([Patterns](#patterns)) |
| 📈 Plot | the plot of this tab's signals |
| 📷 (`Ctrl+Shift+S`) | copies a screenshot of the window to the clipboard |
| ⚠ after the file's size in the top bar | what wasn't read, one by one, with an export for a bug report ([What wasn't read](formats.md#what-wasnt-read)) |
| ⚙ (`Ctrl+,`) | settings ([Settings](settings.md)) |
| ℹ About (`F1`) | version, licence, the repository |

## Patterns

*Experimental.* **🧩 Patterns** opens a window of the tab's lines grouped
by the message they print: values masked, the parts where messages differ
as slots, and states with their values (`gear={D|P|N}`), as
[`nerka patterns`](cli.md#patterns-experimental) does. Nothing is grouped
until **▶ Run**, and a result stays until the next run; it says **Out of
date** when the file was read again, other lines or another field are
chosen, or the [pattern configuration](cli.md#the-pattern-configuration)
or the signals changed.

- **Lines:** all lines, the filter result, one lifecycle (`@ecu1` and its
  number), or the lines of a filter you type (NFD). **Field:** the text
  grouped (default: a DLT message's `payload`, else the line).
- **Each row:** its share and count of lines, the fields that tell it
  apart (`apid=NAV`) and the template, state values in colour. A click
  goes to its first line. **▸** unfolds its slots (each value with its
  count and first and last line, a click goes there) and its lines.
- **▾ Filter** puts its query in the Filter box and runs it, **🔍**
  searches for its lines among the filtered ones, **📋** copies the
  query. Sort by most lines, rarest first, or first line.

**⚖ Compare** (in the same window) groups two sets of lines into patterns
together and lists what differs: patterns and slot values **new in B**,
**gone from B**, then those **changed** (more or less often, surest first;
the score is G², 10.8 or more by default), as
[`nerka compare`](cli.md#compare-experimental) does.

- **A and B:** all lines, the filter result, the lines of a filter,
  lifecycles of `@name` by number (`2`, or a group: `2, 5-7`) or every one
  of an outcome (every `crash` against every `planned`), or another tab.
  With lifecycles, a row also says in how many of a side's lifecycles the
  pattern has lines (`A 3/3 · B 0/5`): a constant symptom, not a one-off.
- **Only** and **Ignore** (NFD) choose the lines of both sides first:
  `ecu == ECU1`, or leave out heartbeats that differ every boot.
- **Each row:** the lines in A and B, the change, the score, the pattern
  (for a value: its slot and value), and its first line on its side (a
  click goes there, in its tab). **⋮**: *Ignore this pattern / value*
  (hidden from the results; *Show ignored* brings it back), *Save as
  ignore rule* (added to the [pattern
  configuration](cli.md#the-pattern-configuration), so patterns and
  compares leave its lines out), *Filter to its lines*, *Copy its query*.
- In the Signals panel, **⚖** on a lifecycle compares it with the one
  before.

## Windows

`nerka-gui FILE…` started while nerka is open hands the files to the window
focused last, which opens them as tabs. `--new-window` starts a window of
its own; `--name NAME` names it. Windows share one configuration: when
another window changed packs, presets, plots, checks or signals, a bar
says what, with **Reload** and **Not now**; nothing is changed under you.
A tab's menu → **Move to window** moves it, with its filter, search and
selection: to a **New window**, or to one of the other open windows, listed
by name and active tab. Name windows (⚙ Settings → Name) to tell them
apart.

## Keys

| Keys | |
|---|---|
| `Ctrl+O` | open files (or drop them on the window) |
| `Ctrl+F`, `Enter` | focus the Filter box, run it |
| `n` / `N` | next / previous search hit |
| `Ctrl+G` | go to line |
| `Enter` on a line | show the whole line |
| `Ctrl+C` | copy the selection, or the current line |
| `Ctrl+click`, `Ctrl+Space` | pick single lines to copy |
| `m`, `b` / `B` | bookmark, next / previous bookmark |
| `Ctrl+Shift+L` | library and selection panes |
| `Ctrl+Tab`, `Ctrl+Shift+Tab` | next / previous tab |
| `Ctrl+W` | close the tab |
| `Ctrl+Shift+S` | screenshot to the clipboard |
| `Ctrl+,`, `F1` | settings, about |

## Command line options

```
nerka-gui [OPTIONS] [FILE]...
```

| Option | |
|---|---|
| `-q, --query NFD` / `-r, --regex RE` / `-t, --text TEXT` | filter the first tab |
| `--pack PATH` | load a query pack (file or folder) |
| `--pick PACK.QUERY`, `--exclude PACK.QUERY`, `--signal-pick PACK.QUERY` | put a library query in the selection (Include, Exclude, Signal); `PACK.QUERY(NAME=VALUE, …)` adds an instance of it (repeat for several) |
| `--search-pick PACK.QUERY` | put a library query in the Search box |
| `--param NAME=VALUE` | a library parameter's value |
| `--highlight NFD` | colour the lines a query matches |
| `--library` | show the library |
| `--signal SPEC`, `--lifecycle SPEC`, `--check SPEC` | a signal, lifecycle or check for this session (as the CLI's; not saved unless edited) |
| `--plot NAME`, `--plot-window` | plot a signal (in a window of its own) |
| `--marker NAME=VALUE[:LABEL]`, `--cursor TIME` | a line in a plotted lane, a time cursor |
| `--fibex FILE` | a FIBEX file for DLT non-verbose messages |
| `--new-window`, `--name NAME` | a window of its own, its name |
| `--screenshot PATH` | once the files are loaded, the signals collected and the plot drawn, save the window as a PNG and close it (for checks without a person at the screen); a plot in its own window is saved too, as `PATH-plot.png` (`shot.png` → `shot-plot.png`) |
| `-h, --help`, `-V, --version` | |

A `--signal`, `--lifecycle` or `--check` that doesn't parse is listed in an
**Options not used** window. The CLI's options for opening captures
(protocols, ports, records, merging) are settings in the GUI: ⚙ Settings,
and a tab's **Records…** and **Files…**.
