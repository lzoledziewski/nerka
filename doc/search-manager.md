# Search manager — design

Agreed with the owner on 2026-10-18 after a brainstorm with mockups. Nothing is built yet; the POC scope is at the end.

## Goal

Nerka is a **log viewer first**. The search manager helps you build good queries once and reuse them across components and investigations, without rewriting or editing them for each context. Automation (for example exporting an analysis as an agent skill) should become possible later, but it isn't the goal now.

## Decisions (owner, 2026-10-18)

- **No result tabs** (no second layer of tabs, one per query).
- **Two functions, same inputs:**
  - **Filter** decides which lines stay.
  - **Search** highlights and jumps between hits **inside the filter result only**. It doesn't search the whole file and doesn't depend on which view has focus.
- **Queries live in packs.** A pack is a file and the unit of sharing. Packs can come from different projects or teams and can be mixed.
- **Labels** group queries across packs (many-to-many, hierarchical with `/`, e.g. `system/vision`). Picking a label selects every query that has it.
- **Parent → child refinement:** a component's parent query lists all of its traces (`ecu == SOCB`). Child queries refine it for one aspect (`apid == SWUP`) and carry labels (`ota`). Picking `ota` then gives "OTA across SoC A, SoC B and the gateway", even though each logs it differently.
- **Parameters** are in (`$cam` for "troubleshoot camera X"). They will mostly be used inside payload regexes.
- **NFD (the query language) stays self-contained.** No pack references (`@pack.query`), no `$params` and no `label:` in the language: they would mean different things in different deployments. Picking from the library **expands** into plain NFD, and that text is what runs, goes into history and can be pasted anywhere.
- **Names:** stored qualified (`vision.frame_drops`); the UI shows the short name in context.
- **No parameter-name mapping** between packs. Nobody has to use the same set of queries.
- **Scenarios** (saved investigations, steps, skill export) are **parked**: too abstract for now.

## Model

| Concept | What it is |
|---|---|
| **Query** | Id, NFD text, optional parent query, labels, parameters, optional colour. Its full meaning is `parent AND own text`, wherever it's shown. |
| **Pack** | A text file of queries; namespace = pack name. |
| **Label** | Grouping only; it adds no meaning to a query. |
| **Selection** | What you picked for Filter (include / exclude) and for Search, plus parameter values. Lives in the tab (UI state); expanded into NFD when you run it. |

### Expansion (core, not UI)

`expand(selection, packs, params) -> Expansion` in `nerka-core`:

- returns **plain NFD** (`String`): included queries OR'ed, excluded ones AND NOT, each query ANDed with its parent chain;
- also returns **provenance**: which part of the expanded expression came from which query. Search colours and the planned details view use it, so the text never has to be parsed back;
- substitutes parameters, **regex-escaped by default** (a value like `1.2.3` or `cam(1)` must not widen or break a regex); inserting a raw value has to be asked for explicitly;
- resolves labels **at the moment of picking**. A label is a snapshot: a query added later doesn't silently join an applied filter. Editing a pack changes only the next expansion, never a filter that is already running;
- reports errors instead of guessing: a missing parameter, a child whose parent no longer exists (never silently widened to "all lines"), an unknown query id.

The UI only shows the selection and sends the expanded NFD (spec §4).

As built (`core/src/pack/expand.rs`):

- Shape: `(root) AND (…) AND (query)` per picked query (every query's own text in parentheses); several includes are joined with `OR` (AND binds tighter, so a chain needs no extra parentheses) and grouped in parentheses only when excludes follow; each exclude is `AND NOT …`. Only excludes means "every other line". The result is **one line** (whitespace outside regexes and strings collapsed), so it fits the history file.
- Parameter values depend on where they stand: inside `/…/` regex-escaped (`/` and `\` as `/` / `\`, so a value can't end the regex); inside `"…"` string-escaped; as a bare value (`ecu == $ecu`) they become a string (`ecu == "SOCB"`), so a value like `A OR x` can't change the query. Raw (`ParamValue::raw`, CLI `--raw-param`) only matters inside a regex: the value is inserted as regex syntax (only `/` is still escaped). A parameter can't stand for a field name.
- Parameter names are shared by every picked query (no mapping between packs).
- Labels resolve when `expand` runs. `Selection::snapshot` replaces labels by their current queries, for a UI that keeps a selection around.
- Errors: nothing selected, unknown query id, a label no query has, a missing parameter (naming the query that needs it), an orphaned child (its parent isn't loaded), a parent cycle, and a raw value that makes a query invalid NFD. Each part is checked to parse.

### Display of long expansions

The expanded NFD can get long (a label with ten queries). The filter and search boxes show it **on several lines, grouped** (by label, by query or by parent), with **name chips** for the picked queries. The full text stays available and is what actually runs.

### History

History stores the **expanded NFD** plus the chip names for display. Old entries keep working after packs change or disappear.

## Pack file

Plain text that merges well in git: one query per block, stable ids, order kept as written, nothing generated. Files are UTF-8 with the extension `.nerka-pack`. Parser and full grammar: `core/src/pack/` (the doc comment of `mod.rs`).

```text
# A comment: '#' in column 1, or ' #' followed by a space.
pack vision   # camera team

query cam_scope(ecu, cam)  labels: system/vision
    ecu == $ecu AND ctid == $cam

query frame_drops(cam)  parent: cam_scope  labels: system/vision, perf  color: violet
    payload ~ /$cam frame drop/
```

- `pack NAME` is the first non-comment line; one pack per file. Pack and parameter names are `[A-Za-z_][A-Za-z0-9_]*`, so `.` only separates pack and query (`vision.frame_drops`). A query name may also be quoted to hold spaces (owner, 2026-10-18): `query "xyz logs"`, `parent: "vision.xyz logs"`; quoted names can hold anything but `.` `"` `#` `:` and don't start or end with a space. Pack names may be quoted the same way (owner, issue #4): `pack "camera team"`, `parent: "camera team.scope"`; a new pack file is `<name>.nerka-pack`, so the GUI's **Create** also refuses `/ \ * ? < > |`. Names are quoted only when needed.
- `query NAME(PARAMS)` then attributes, all optional and in any order: `parent:` (a query of the same pack, or `pack.query` from another pack), `labels:` (comma-separated, `/` for hierarchy), `color:` (a lowercase name or `#rrggbb`).
- The body is the indented lines below the header, up to the next line starting in column 1. It is kept verbatim (no comments inside), with the common indent removed. Blank lines don't end a body.
- `$name` or `${name}` is a parameter and must be declared in the query's own list. `$$` is a literal `$`; so is a `$` not followed by a name (the regex anchor in `/down$/`) and `\$` inside a regex or string.
- The parser checks the body's NFD syntax (with placeholder parameter values) and reports every error as `file:line:column: message`. Field names are checked only when a query runs on a source.
- Parents are resolved at expansion time, so a child can refine a query from another pack. A missing parent is an expansion error, not a parse error (the parent's pack may simply not be loaded).

Where packs are loaded from: `%APPDATA%
erka\packs\` (Linux: `~/.config/nerka/packs/`), plus `--pack <dir|file>` on the CLI (a directory loads its `*.nerka-pack` files, not recursively, in name order). Pack names must be unique across everything loaded. Packs stored with a project are an open question.

## Later (not in the POC)

- **Details view:** hover over or select a line to get a tooltip or panel with which queries matched (from provenance) and statistics. It is also where future **replace** (e.g. numbers to enum names) and **extract** functions go (numbers, states and floats feed a state machine, a plot or a high-level bookmark). Ties in with `sources-and-signals.md`.
- **Collaboration:** dirty / clean pack state, pushing local changes to a repo, pulling others' changes. Keep the file format git-friendly now so this stays possible.
- **Scenarios** and export as an agent skill (`SKILL.md` plus packs plus a runnable file). That needs an agent-friendly CLI: search inside a filter, `-c` first, `-m N`, context lines, `--json`.
- Density strip of search hits next to the scrollbar.
- Editing packs inside the GUI.

## POC scope (proposal)

1. **Core: packs.** Parser and model in `core/src/pack/` (no new dependency): queries, parents, labels, parameters, colours. Errors carry file and line positions; parser tests for each error.
2. **Core: expansion.** `expand` with provenance, regex-escaped parameters, error cases. Tests: the expanded NFD compiles and finds the same lines as the hand-written query (against the naive per-line path, as for every engine test).
3. **Core: search within the filter result.** Today there is only the filter (n / N jump to the next filter match in the main view). Add a search that runs over the filter's line list and reports which search query matched each hit.
4. **CLI:** `--pack`, `nerka expand` (prints the NFD), and `search --in <filter>`. Thin, as always.
5. **GUI:** a library panel (grouped by label or by pack; include / exclude for Filter, a tick for Search; parameter fields), a Search box next to the Filter box, multi-line grouped display with chips, search hits coloured per query. Packs are **edited as text files** in the POC; the GUI only reads and reloads them.

### POC status (2026-10-18)

All five steps are built (no new dependency). Step 5 (GUI) is described at the end of this section.

- `core/src/pack/`: parser (`parse_pack`, `Library`), `expand` → `Expansion { nfd, parts }`, `Expansion::search_set`.
- `core/src/engine/within.rs`: `search_within(source, filter_lines, SearchSet)` → `SearchHits` (lines plus, per hit, which include queries matched; any number of queries). A hit matches any include query and no exclude query. Two paths: per-line over the filter's lines, or (every include query has a prefilter and the filter keeps ≥ 1/16 of the lines) the buffer scan per query intersected with the filter's lines. Tests run both against a naive per-line evaluation.
- CLI: `--pack <dir|file>` (global, repeatable; the per-user directory is loaded too), `nerka packs`, `nerka expand -q ID -l LABEL -x ID -X LABEL -p NAME=VALUE --raw-param NAME=VALUE [--parts]`, `nerka search --in FILTER PATTERN FILE` and `nerka search [--in FILTER] -q/-l … FILE` (prints `line:[queries] text`; `-c` adds hits per query). Filtering by picks goes through `nerka filter "$(nerka expand …)"`.
- 1 GiB text log: `search ERROR`, `level >= ERROR` and `level >= WARN AND raw ~ /timeout/` unchanged (~32 ms); `search --in 'level >= WARN' timeout` 36 ms filter + 57 ms search.

Choices made while building (not in the decisions above). Owner's answers, 2026-10-18, in italics:

1. A parameter used as a bare value becomes a string (`ecu == "SOCB"`), so it can't stand for a field name or inject operators. _Confirmed; no operator parameters until a real query needs one (then a typed parameter such as `op: ==|!=`, not a raw bare value)._
2. Raw parameter values only differ from escaped ones inside regexes. _Confirmed._
3. Picking only excludes means "every line except these". _Confirmed._
4. Picking a label includes the labels below it (`system` → `system/vision`). _OK for now._
5. A child's parent may live in another pack (`parent: vision.cam_scope`); a missing parent is an expansion error, not a load error. _Doubts, but try it._
6. The CLI puts `-q` picks before `-l` picks (clap loses the order between the two options); without `--in`, a pick search runs over every line. _Not evaluated: the CLI isn't a focus now._

GUI (step 5), as built (`ui/src/library.rs`, `ui/src/main.rs`):

- **Library and Selection (owner, 2026-10-18):** two panes on the right (📚 in the toolbar, Ctrl+Shift+L). The **Library** browses and edits the packs: queries grouped by label (default; a query shows under each of its labels, unlabelled ones last) or by pack; hover shows the NFD, the parent and the labels. A query that uses fields the open file doesn't have (with its parents; e.g. `apid` on a text log) is **greyed out**: its `+`, `🔍` and drag are disabled and hovering names the missing fields and the ones the file has (`Library::missing_fields`, `Expr::field_names` in core). Queries are shown as a **tree** (owner, 2026-10-18): a query refining another one of the group sits under it (fold arrow); one whose parent is outside the group shows `↑ parent`. Per query: `+` (or `+ all` for a group, resolved when clicked) or **dragging** its name adds it to the selection, `🔍` searches for it, and `⋮` at the right end of the row opens **Edit…**, **New query refining this…**, **Exclude from the filter** and, for a query with a parent, **Add without its parents** (owner, 2026-10-18: only its own text, e.g. a generic SoC child on a `.txt` log where the DLT-only parent can't run; it stays enabled when only the parents need missing fields; the selection row says "without parents", and its 🔍 searches without them too) (owner: few buttons; edit and refine live in that menu). ⟳ reloads the packs; bad pack files are listed in red and the good ones still load.
- The **Selection** is the ordered list of filter queries in use (per tab). Each row: `⠿` drag handle to reorder, a role (`+` include in the filter, `−` exclude from it), a colour swatch (for included queries it switches the line colour), the name (italic with `•` when it isn't saved in a pack), `🔍` search, `✎` edit, `−` remove. Order counts: includes are OR'ed in this order and a later one wins where line colours overlap. Below: a field for every parameter the selection needs (parent chains included) with a "regex" tick for raw, the expansion error if any, **Apply** (into the Filter bar, and run) and **Clear**.
- **Search is an action, not a role (owner, 2026-10-18):** a query's `🔍` menu (library or selection) has **Search for it** (replaces the Search box) and **Add to search (OR)**. Adding keeps what the box holds: earlier queries stay as they are, and typed text is first turned into the query that finds the same lines (`text_to_nfd` in core: `"text"`, `/regex/i`, …). Each query of the box is searched on its own, so hits keep a colour and a chip per query. Typing in the box afterwards turns it into one plain query again; nothing stays "selected for search".
- **Editing in the library:** `⋮` → **Edit…** opens an editor at the top of the library pane (name, labels, colour, NFD text, parent, live validation) with **Save** (renames in place if the name changed; refuses a name that exists), **Cancel** and **Delete** (asks again). In the Pack grouping, **+ New query** writes a new query into that pack. `⋮` → **New query refining this…** opens the editor for a new query that refines it (in the same pack). Both editors have a **Refines** dropdown (the library's queries, or "(nothing)"); it leaves out the query itself and the queries that refine it, so a chain can't loop. Selections that use a renamed query follow it.
- **Editing in the selection:** `✎` opens the node's name, labels (comma-separated), colour, NFD text (multi-line), "refines …" with × to drop the parent, live validation by the pack parser, and **Save to** a loaded pack (or **Create** a new `<name>.nerka-pack` in the pack folder first). Saving into the pack the node came from renames that query in place; into another pack it adds the query (or replaces one with that name), keeping every other line (`upsert_query` / `rename_query` in core). **+ New** starts an empty, **unnamed** node (owner, issue #3): the selection, the chips and the search show its query text on one line (cut at 40 characters) instead of a name, until one is typed. A name is needed only to save it into a pack (Save explains that). Unsaved (new or edited) nodes expand like library queries: they go into a temporary pack `_sel` for `expand`. After ⟳, unedited nodes take the library's current text; a node whose query disappeared stays as a local copy.
- **Layout (owner, 2026-10-18, after trying the first version):** the Filter decides what the main view shows; the Search looks among those lines and lists its hits in the bottom pane.
- **Filter box** (top of the window, under the tabs): the picks arrive as Query-mode NFD and the filter is recorded in the history as that text.
- **Line colours:** an included filter query can colour the whole lines it keeps (off by default; click its swatch in the selection or its chip under the Filter bar, filled = on). A later node wins where lines overlap. Core reuses `search_within` over the filter result for it; the hit masks are indexed in selection order, and the view takes the last one that matched. Startup: `--highlight ID`.
- **Search box** under the Filter box: the same modes as the Filter box (Auto, Text with Regex, NFD; Ignore case; `search-modes.md`), Enter or Find runs it, Clear removes it. It looks only inside the filter result (the whole file without a filter) and runs again by itself when the filter result changes. Hits are listed in the bottom pane; clicking one moves the main view there. n / N jump between search hits.
- **Grouped display:** while a box holds an expansion unchanged, its picked queries appear under it as **one row of name chips** (owner, 2026-10-19; NFD on hover); ▸ unfolds one line per picked query: how it joins (`OR`, `AND NOT`), a chip with the short name in the query's colour (qualified name on hover, or shown when two picks share a short name), and that query's NFD. Up to 8 lines; the full text stays in the box. Editing the text turns it into a plain query (no chips).
- **Colours:** a search query's hits are highlighted in its pack colour (named colours or `#rrggbb`), otherwise in a palette colour by position; a typed search uses the first palette colour. Hit lines also get one thin stripe per matched query in the gutter, so queries without regex spans (`level >= ERROR`) show too. The filter's yellow stays; search colours win where they overlap.
- **Command line** (also used for screenshots): `--pack <dir|file>`, `--pick ID`, `--exclude ID`, `--search-pick ID`, `--param NAME=VALUE`, `--library`.
- Status line: hits and time, plus hits per query when there are several.

Choices made for the GUI (open questions below answered provisionally; confirm or change):

7. The Search box has the Filter box's modes (Auto, Text, NFD since 2026-10-19), without Invert.
8. Colours come from the pack, with a palette fallback; there is no colour picker in the UI.
9. Parameter values live in the tab and aren't remembered between runs.
10. ~~Search history isn't kept~~ The Search box has its own history with ↑ / ↓ (owner, issue #5; see `status.md`). Both histories store the expanded NFD but not the chip names (the history file is one `tag<TAB>pattern` per line; adding names needs a format change).
11. The selection is per tab; the library's grouping is shared by all tabs.
12. Chips only switch line colours (no × to drop a query from the box).
13. Library `+` and drops add a query as "include in the filter"; change the role in the selection. The same library query isn't added twice in one role.
14. Saving rewrites the query's header line (a comment at its end is lost); the body and every other line stay. New packs are created in the per-user pack folder only.

## Open questions

- Packs stored with a project (next to the logs or in a repo), besides the app-data folder?
- Search box: the same Text / Regex / Query modes as the filter box?
- Colours: set in the pack, chosen in the UI, or both?
- Parameter values: remembered per parameter name, per pack, or not at all?
