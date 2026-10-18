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

### Display of long expansions

The expanded NFD can get long (a label with ten queries). The filter and search boxes show it **on several lines, grouped** (by label, by query or by parent), with **name chips** for the picked queries. The full text stays available and is what actually runs.

### History

History stores the **expanded NFD** plus the chip names for display. Old entries keep working after packs change or disappear.

## Pack file (tentative)

Plain text that merges well in git: one query per block, stable ids, order kept as written, nothing generated.

```text
pack vision   # camera team

query cam_scope(ecu, cam)  labels: system/vision
    ecu == $ecu AND ctid == $cam

query frame_drops(cam)  parent: cam_scope  labels: system/vision, perf  color: violet
    payload ~ /$cam frame drop/
```

Where packs are loaded from: `%APPDATA%\nerka\packs\` (Linux: `~/.config/nerka/packs/`), plus `--pack <dir|file>` on the CLI. Packs stored with a project are an open question.

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

## Open questions

- Packs stored with a project (next to the logs or in a repo), besides the app-data folder?
- Search box: the same Text / Regex / Query modes as the filter box?
- Colours: set in the pack, chosen in the UI, or both?
- Parameter values: remembered per parameter name, per pack, or not at all?
