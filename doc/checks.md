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

## Next, to decide with the owner

- Marks: the evidence in the log view's gutter, the plot, and a badge in the lifecycle list.
- Profiles, and where checks live (today: one list for every tab).
- More tests: a signal's condition held for a time (`@temp > 90 for 5s`), conditions on the lifecycle's outcome, several tests in one check, a check using another's result.
