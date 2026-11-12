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
files, turn protocols on or off per capture. **Merge by time** in the
menu interleaves lines by their time, for files that know it; a tab of one
such file has **Sort by time** instead. A field `file` tells the files
apart in queries. See [Files and formats](formats.md).

A tab's menu → **Reload** reads its files again from disk, keeping
the filter, search, selection and the line you are on.

## Panels and windows

| Button | Opens |
|---|---|
| 📚 Library (`Ctrl+Shift+L`) | the library of query packs and this tab's selection ([Query packs](packs.md)) |
| 📈 Signals | signals and lifecycles of this tab ([Signals](signals.md), [Lifecycles](lifecycles.md)) |
| ✔ Checks | checks and their results ([Checks](checks.md)) |
| 📈 Plot | the plot of this tab's signals |
| 📷 (`Ctrl+Shift+S`) | copies a screenshot of the window to the clipboard |
| ⚙ (`Ctrl+,`) | settings ([Settings](settings.md)) |
| ℹ About (`F1`) | version, licence, the repository |

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
| `--pick PACK.QUERY`, `--exclude PACK.QUERY`, `--signal-pick PACK.QUERY` | put a library query in the selection (Include, Exclude, Signal) |
| `--search-pick PACK.QUERY` | put a library query in the Search box |
| `--param NAME=VALUE` | a library parameter's value |
| `--highlight NFD` | colour the lines a query matches |
| `--library` | show the library |
| `--signal SPEC`, `--lifecycle SPEC`, `--check SPEC` | a signal, lifecycle or check for this session (as the CLI's; not saved unless edited) |
| `--plot NAME`, `--plot-window` | plot a signal (in a window of its own) |
| `--marker NAME=VALUE[:LABEL]`, `--cursor TIME` | a line in a plotted lane, a time cursor |
| `--fibex FILE` | a FIBEX file for DLT non-verbose messages |
| `--new-window`, `--name NAME` | a window of its own, its name |
| `-h, --help`, `-V, --version` | |

A `--signal`, `--lifecycle` or `--check` that doesn't parse is listed in an
**Options not used** window. The CLI's options for opening captures
(protocols, ports, records, merging) are settings in the GUI: ⚙ Settings,
and a tab's **Records…** and **Files…**.
