# Checks — design, built in small steps

_Status: first slice built 2026-11-07 (core, CLI, GUI pane), to iterate on with the owner's feedback (owner: "let's iterate through implement / test / feedback loops"). Lifecycles: `lifecycles.md`. Current state: `status.md`._

## Goal (owner, 2026-11-07)

- Identify **issues** (a known problem shows up) and **facts** (a value, a count).
- **Scopes:** overall, or per lifecycle.
- A **pane** of its own.
- **Primitives:** exists, frequencies, gaps, …; advanced logical reasoning later.
- **Results:** overall, plus marks that behave like bookmarks (plot, log view), and information near lifecycles.
- **Profiles** (open: when): a set of signals, lifecycles and checks for one kind of log (a UART log from one device, DLT from another), so checks that make sense on one don't show on the other.

## First slice (built)

A check has **its lines** (NFD or a library query `{pack.query}`), **one test**, and a scope (`per LIFECYCLE`; overall always comes first).

| Test | Kind | Meaning |
|---|---|---|
| `exists` | issue | one of its lines shows up (each is evidence) |
| `count` | fact | how many lines |
| `count OP N` | issue | when the count compares so (`>= 3`) |
| `rate N per DUR` | issue | more than N of its lines within a DUR window (a burst; one per stretch) |
| `gap DUR` | issue | its lines stop for longer |
| `then QUERY within DUR` | issue | one of its lines, then QUERY's within DUR |
| `missing QUERY within DUR` | issue | one of its lines, no QUERY line within DUR after it (not judged when the scope ends first) |
| `value first\|last\|min\|max` | fact | what its query's first named group captures |

Spec (CLI `--check`, saved in `<config>/nerka/checks`, one per line, `off ` for one switched off):

```
stall: lines {net.down}; missing {ota.progress} within 30s; per ecu1
storm: lines ecu == ECU1 AND level >= ERROR; rate 25 per 1s; per ecu1
temp: lines /temp=(?<t>\d+)/; value max; per ecu1
```

A lifecycle's scope is its time span (from its first line to the next one's): lines of any entity in it count. Tests on time need the lines' times (the source's, or a time signal).

- **Core:** `core/src/check.rs`: `CheckDef` (spec, `resolved` with the library, `run` → `CheckResult`: per scope a verdict (ok / issue ×N / fact value) and up to 1000 pieces of evidence (lines and a note)), `Checks` (the saved list).
- **CLI:** `--check SPEC`, `nerka check FILE` (`-e N` prints N pieces of evidence per result with the lines, `--json`, `--strict` fails on issues, `-n NAME`).
- **GUI:** ✔ Checks in the toolbar opens the pane. Each check: on/off, its kind, ✎, −, then its results, overall │ per lifecycle (`#2: 3×`, red for issues). Click a result: its evidence below (line numbers → go there; the note; the line). A form: name, its lines (📚 / drag from the Library), the test as a choice with its fields, for the whole log or each lifecycle of …, Preview, Add. Checks run in the background when they, the signals or the file change. `nerka-gui --check SPEC` adds one for the session.

512 MiB DLT: each check 30–300 ms (a `count` per ECU1 lifecycle 211 ms with the lifecycles; a `gap` on GW's NAV lines 30 ms; a `value max` per lifecycle 302 ms).

## Second round (owner's feedback, 2026-11-07)

- **The pane grew to the whole window:** at most half the window now.
- **"Didn't notice how to create one":** ➕ New check in the pane's header; the form opens at the top.
- **Issue or fact is a choice:** every test measures something. As a fact: `exists` how many lines, `count` how many, `rate` the most lines in one window, `gap` the longest stop, `then` / `missing` how often, `value` the value. As an issue: each line / burst / stop / occurrence, or for a count or a value a condition (`count >= 3`, `value max > 90`; a count without one: any line). A check says `fact` or `issue` when it isn't its test's default (a count or a value without a condition is a fact).
- **Groups:** `labels ota, network`. The pane groups checks by label (a check with several shows under each); a label's box switches all its checks on or off.
- **Status near lifecycles:** the lifecycle list (Signals panel) shows per lifecycle what its checks found (`⚠2 ℹ3`; hover: each check's result; click: the Checks pane on its first issue). Checks run whenever some are on, not only while the pane is open.
- **"Cannot build own primitives":** the built-in tests stay as the easy path; building one's own needs a formula over measures (`count(A) > 2 * count(B)`, `exists(crash) AND NOT exists(shutdown)`): to propose next.

- **Bugs (owner, 2026-11-07):** a check's name had to be an identifier: any text now (not `:` or `;`; the same check whatever the case). A library query whose name has spaces (`{Aurix_logs.Fusa Fault}`) read the rest of its name as a broken parameter: its name is everything before the first `NAME=VALUE` word (lifecycles too).

## Next, to decide with the owner

- Marks: the evidence in the log view's gutter, the plot, and a badge in the lifecycle list.
- Profiles, and where checks live (today: one list for every tab).
- More tests: a signal's condition held for a time (`@temp > 90 for 5s`), conditions on the lifecycle's outcome, several tests in one check, a check using another's result.
