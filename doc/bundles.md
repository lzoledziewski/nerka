# Settings bundles: export, import, upgrades

_Owner, 2026-11-07: an import / export of the analysis setup, so it can be
shared with a colleague or moved to another computer, and so that a later
nerka with a breaking change in a file format adapts the data instead of
failing on it._

## What is in a bundle

The **analysis setup**, the user's own items in the per-user config folder
(`%APPDATA%\nerka`, `~/.config/nerka`):

| Kind     | Where it lives                    | One item is      | Named by               |
|----------|-----------------------------------|------------------|------------------------|
| `pack`   | `packs/*.nerka-pack`              | a pack file      | its `pack NAME` line   |
| `preset` | `presets/*.nerka-preset`          | a preset file    | its `preset NAME` line |
| `plot`   | `plots/*.nerka-plot`              | a plot file      | its `plot NAME` line   |
| `check`  | `checks` (a spec line each)       | one check line   | the text before `:`    |
| `signal` | `signals` (a spec line each)      | one signal or lifecycle line | its name   |

Not in it (owner, 2026-11-07): `prefs`, the filter and search histories,
`query-usage`, the `records` memo (absolute paths), window registrations
(`instances/`), the unpack cache, bookmark sidecars (they live next to the
logs). Packs loaded with `--pack` from elsewhere aren't the user's folder,
so they aren't exported either.

## The file

`*.nerka-bundle`, UTF-8 text: one file that can be mailed, kept in git and
read in a review. The items are the files and lines as they are, one after
another, each under a header line:

```text
nerka-bundle 1
nerka 0.1.0
# Comments and blank lines before the first item are ignored.

@@ pack vision format 1
pack vision

query cam
    apid == CAM
@@ preset "cam triage" format 1
preset "cam triage"
node include lit          query vision.cam
@@ check storm format 1
storm: lines level >= ERROR; rate 25 per 1s
@@ signal v format 1
v=/v=(?<v>\d+)/
```

```text
file   := "nerka-bundle" VERSION NEWLINE ["nerka" TEXT NEWLINE] (comment | blank)* item*
item   := "@@" KIND NAME "format" N NEWLINE line*      -- up to the next header or the end
KIND   := "pack" | "preset" | "plot" | "check" | "signal"
NAME   := a word, or "…" with \" and \\              -- as in presets
line   := any line; one that starts with "@@" is written with one more "@"
```

- `VERSION` is the bundle's own layout (1). A later layout (a zip, if
  bundles ever carry binary files) gets 2; this nerka refuses a bundle
  newer than it knows, saying which nerka wrote it.
- The `nerka` line names the version that wrote it, for messages only.
- **Escaping:** a content line starting with `@@` gets an extra `@` when
  written (`@@x` → `@@@x`), and the reader takes one off a line starting
  with `@@@`. Only lines starting with `@@` and a space or the end are
  headers, so nothing else needs escaping.
- The header's NAME is for people (and for an item that can't be read);
  the item's own text is what counts: its name is read from it again
  once it is upgraded.
- A KIND this nerka doesn't know (a later nerka's) is skipped with a note;
  the rest imports.

## Formats and upgrades

Every kind has a **format number**, today 1 for all of them (the files as
nerka 0.1.0 writes them). A breaking change to a kind's file format bumps
its number and adds a **migration step**: a function from that kind's text
in format *n* to format *n+1* (`bundle::STEPS` in core). An item in an
older format goes through the steps in order up to the current format, and
only then through the normal parser.

- An item in a **newer** format than this nerka knows is refused (`made by
  a newer nerka: check format 3, this nerka reads up to 2; update nerka to
  import it`); the rest of the bundle still imports.
- Steps work on text, so they never need the old parser; each comes with a
  test on a sample of the old format kept in the tests.
- **The config folder uses the same steps.** `<config>/nerka/format` lists
  each kind's format the folder's files are in (`pack 1` …; a folder
  without it is all format 1). When a newer nerka starts (the GUI, and the
  CLI), it first writes every item of the folder as it is into a backup
  bundle (`<config>/nerka/backups/before-upgrade-<UTC time>.nerka-bundle`, with
  their old format numbers, so it imports into any later nerka), then
  upgrades the files in place and writes the new numbers. A folder written
  by a *newer* nerka than the one running is left alone; nerka says so.

## Import

Importing is **planned first**, then applied. Each item of the bundle,
upgraded to the current format and parsed, is one of:

| Status      | Meaning                                   | Choices                          | Auto        |
|-------------|-------------------------------------------|----------------------------------|-------------|
| new         | no item of that kind and name             | add / skip                       | add         |
| same        | the same text as mine (line ends, trailing spaces and, for checks and signals, spelling aside) | — | skip |
| different   | mine has the name, with other text        | keep both / take theirs / keep mine | keep both |
| invalid     | doesn't upgrade or parse, or twice in the bundle | —                         | skip        |

Names compare without case (file names on Windows; signals are found
without case). **Keep both** imports theirs as `NAME_imported` (or
`NAME_imported_2`, …, whichever is free among mine and the bundle's), its
name changed in its text. A renamed pack keeps the ids inside it, so a
preset in the bundle that names `vision.cam` still names my `vision`.

**Auto** loses nothing and overwrites nothing. Before an import replaces
anything (take theirs), the items it replaces are written into a backup
bundle, `<config>/nerka/backups/before-import-<UTC time>.nerka-bundle`.

Applying writes new pack, preset and plot files as `<name>.<extension>`
(`<name>_2…` if a file of that name holds something else) and replaces a
taken item in its own file; checks and signals are added to (or replaced
in) their files line by line, other lines kept. Every write is atomic.

## Where

- **Core:** `nerka_core::bundle`: `Bundle` (read, write), `Kind`, `Item`,
  `STEPS` and `upgrade`; `Store` (a config folder: its items, `export`,
  `upgrade` of the folder, `plan` and `Plan::apply`).
- **CLI:** `nerka export [-o FILE] [--only KIND[:NAME]]…` (stdout without
  `-o`) and `nerka import FILE [--dry-run] [--conflicts both|theirs|mine]`
  (`both` is Auto; `-` reads stdin). Import prints the plan, a line per
  item, and exits 1 if an item was invalid (the others are imported).
  A GUI that is open doesn't see a CLI import until it loads the files
  again (restart it); its next save of checks or signals would write its
  own list.
- **GUI:** ⚙ Settings → Settings bundle: **Export…** (a list of the items
  with checkboxes, all ticked, then a save dialog) and **Import…** (an open
  dialog, then the preview: a row per item with its status and a choice;
  **Auto** sets every choice back to Auto; **Import** applies and reloads
  the packs, presets, plots, checks and signals).

## Not yet

- Bundles of other things (prefs, histories), picked separately.
- Dropping a `.nerka-bundle` on the window to import it.
- A step that changes an item's name (steps keep it).
