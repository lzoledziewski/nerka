---
title: "Checks"
description: "Checks: find known issues and measure facts in logs, overall and per boot: counts, bursts, gaps, missing answers and sequences, in the GUI or the CLI."
---

# Checks

A **check** looks for a known problem (an **issue**) or measures something
(a **fact**) in a log, overall and per [lifecycle](lifecycles.md).

## Defining one

```
storm: lines ecu == ECU1 AND level >= ERROR; rate 25 per 1s; per ecu1
stall: lines {net.down}; missing {ota.progress} within 30s; per ecu1
temp: lines /temp=(?<t>\d+)/; value max; per ecu1
lost: lines /timeout/; missing /connected/ within 200ms
bursts: lines @lost.event; rate 2 per 60s
```

`NAME: lines QUERY; TEST[; TEST…]` and, in any order, `per LIFECYCLE`,
`issue` or `fact`, `any` or `all`, `labels a, b`. The **lines** are NFD or a
library query (`{pack.query}`, `{pack.query name=value}`, written as in
[lifecycles](lifecycles.md#defining-one)); they can use the
tab's [signals](signals.md). Durations: `500ms`, `30s`, `5m`, `1h`.

| Test | Finds |
|---|---|
| `exists` | one of its lines |
| `count`, `count OP N` | how many lines; as an issue, when the count compares so (`count >= 3`) |
| `rate N per DUR` | more than N of its lines within DUR (a burst) |
| `gap DUR` | its lines stop for longer than DUR |
| `then QUERY within DUR` | one of its lines, then a QUERY line within DUR |
| `missing QUERY within DUR` | one of its lines, and no QUERY line within DUR after it |
| `value first\|last\|min\|max [OP N]` | what its query's first named group captured |

- **Issue or fact:** each test can be either (`issue` / `fact`; the
  default comes from the test). As a fact it measures (how many, the
  biggest burst, the longest gap, the value); as an issue it lists each
  occurrence.
- **Several tests:** `count >= 3000; gap 700ms` is an issue where all of
  them find one (`all`, the default); with `any`, where one does. A fact
  has one test.
- **Sequences:** `then` / `missing` clauses chain, each after the step
  before: `lines /session timeout/; then /connection established/ within 1s;
  missing level >= ERROR within 50ms`.
- **Per lifecycle:** `per ecu1` gives a result per lifecycle; the overall
  result is still the whole log judged at once (a gap across a reboot shows
  overall, in no lifecycle).
- **A check is a signal:** `@storm` (its name in lower case, other
  characters `_`) counts what it found; `@storm.event` is where. Another
  check can use it (`bursts` above).
- **Time order:** `rate`, `gap`, `then` and `missing` compare each line's
  time with the one before. Where time goes backward in the view (files
  one after another whose times overlap, a clock that jumps) their results
  may be wrong, and nerka says so once. Merge the files by time
  (`--merge`, the tab's **Files…**).

## The Checks pane

✔ **Checks** opens the pane: each check with on / off, its results (overall
│ per lifecycle, `#2: 3×`, red for issues), 📈 to plot its signal, ✎, −.
Click a result to see its evidence (lines, with a note; click to go
there). **➕ New check** opens a form (its lines from the library or typed,
the test, the scope, **Preview**). Checks with labels are grouped; a label's
box switches all of them.

- Which checks are **on** is each tab's; a new tab starts with each check's
  **On in new tabs**.
- **Auto** (default) runs them when the file, the signals or the checks
  change; without it, **▶ Run** does ("Out of date: ▶ Run").
- The lifecycle list (Signals panel) shows what checks found per
  lifecycle (`⚠2 ℹ3`).
- `nerka-gui --check SPEC` adds one for the session.

Saved checks are in `%APPDATA%\nerka\checks`, one per line (`off ` in front:
off in new tabs).

## From the command line

```
nerka --check 'storm: lines level >= ERROR; rate 25 per 1s' check -e 2 trace.dlt
nerka --check 'lost: lines /timeout/; missing /connected/ within 200ms' \
      --check 'bursts: lines @lost.event; rate 2 per 60s' check trace.dlt
```

`-e N` prints up to N pieces of evidence per result with their lines,
`--json` prints JSON, `--strict` exits 1 when an issue shows up, `-n NAME`
runs one check.
