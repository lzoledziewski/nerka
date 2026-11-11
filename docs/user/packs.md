# Query packs and presets

Queries you use again live in **packs**: plain-text `.nerka-pack` files
that merge well in git and can be shared. You pick queries from the
**library**; nerka expands them into plain NFD, which is what runs.

## A pack file

```text
# A comment: '#' in column 1, or ' #' followed by a space.
pack vision   # camera team

## The camera's lines: a note for whoever picks it (lines starting '##').
query cam_scope(ecu, cam)  labels: system/vision
    ecu == $ecu AND ctid == $cam

query frame_drops(cam)  parent: cam_scope  labels: system/vision, perf  color: violet
    payload ~ /$cam frame drop/
```

- `pack NAME` is the first line that isn't a comment; one pack per file.
  Names are letters, digits and `_`, or quoted to hold spaces
  (`pack "camera team"`, `query "xyz logs"`).
- `query NAME(PARAMS)` then, in any order and all optional:
  - `parent:` a query it refines (of this pack, or `pack.query`): its
    meaning is `parent AND its own text`;
  - `labels:` comma-separated, `/` for a hierarchy (`system/vision`);
  - `color:` a name or `#rrggbb`, for the lines it keeps;
  - `signals:` the kinds of its named groups as signals (`signals: t:time`).
- The **body** is the indented lines below, kept as written.
- `$name` or `${name}` is a **parameter**, declared in the query's list.
  `$$` is a `$`; so is a `$` not followed by a name (`/down$/`).
- `##` lines right above a query are its **note**, shown in the library.
- Errors are reported as `file:line:column: message`; field names are
  checked when a query runs on a file.

Your packs are in `%APPDATA%\nerka\packs\` (Linux: `~/.config/nerka/packs/`);
the CLI's `--pack DIR|FILE` and the GUI's `--pack` load more. Pack names
must be unique. Samples: `examples/packs/`.

## Library and selection (GUI)

📚 (`Ctrl+Shift+L`) opens two panes:

- The **Library**: queries grouped by label (or by pack), refining queries
  under their parent. `+` (or dragging a name) adds one to the selection,
  `🔍` searches for it, `⋮` edits it, refines it, excludes it, or adds it
  without its parents. A query using fields the open file doesn't have is
  greyed out.
- The **Selection**: what this tab picked, each with a role:
  - **Include** (`+`): the filter keeps lines of any included query;
  - **Exclude** (`−`): and leaves out lines of an excluded one;
  - **Signal** (`~`): not part of the filter; its named groups become
    [signals](signals.md), and it can colour the lines it matches.

  Parameters get their values here. Each include can colour the lines it
  keeps (click its swatch). `✎` edits a node; **Save to** writes it into a
  pack (or **Create** a new one). **+ New** starts a query of your own.

Applying the selection puts its expansion in the Filter box (NFD mode).

## Expansion

Included queries are joined with `OR`, each with its parent chain
(`(parent) AND (query)`); excluded ones follow as `AND NOT …`.
Parameter values are **escaped** where they stand: inside a regex as
literal text, inside a string as a string, as a bare value as a quoted
string. So `cam=CAM(1)` can't widen a regex. A raw value (regex syntax) has
to be asked for: CLI `--raw-param`.

A label is resolved when you pick it: a query added to a pack later doesn't
join a filter already applied.

```
nerka --pack examples/packs/ expand -l ota -p cam=CAM1 --parts
```

## Presets

A **preset** saves a tab's analysis to use again on other files: the
selection in order (roles, line colours), copies of queries you edited or
made, the parameter values, and the text typed in the Filter and Search
boxes. Choosing one replaces the tab's selection and runs both boxes.

The bar above the Selection pane: **New from this tab…** → **Keep** (this
session, marked `•`) or **Save**; **Update 'name' from this tab**; **Save to
disk**; **Delete**. Renaming or deleting a library query updates the
presets that pick it. Saved presets are `%APPDATA%\nerka\presets\NAME.nerka-preset`
and travel in [settings bundles](bundles.md).
