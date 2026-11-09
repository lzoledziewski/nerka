# Lifecycles — design

_Status: designed 2026-11-06 with the owner, reworked 2026-11-07 after trying v1 (one lifecycle per entity, library queries, a form instead of clauses typed by hand). Built: core, CLI, GUI; see "Not built yet". Current state and decisions: `status.md`._

## Goal

Logs from devices span several **lifecycles**: an ECU boots, runs, shuts down or crashes, and boots again. Most questions only make sense per lifecycle: "3 resets" means nothing over a whole trace, but "an OTA job in `downloading` when ECU1's boot ended unclean" is a finding.

Lifecycles are the groundwork for **checks** (known-issue signatures that run on every trace), which come next. General state machines (transition rules, timeouts, events from other machines) are left out on purpose. The owner's usage is "state straight from a signal or a regex, reset on a new boot", which signals bound to a lifecycle cover.

## Decisions (owner)

- **Nothing format-specific (2026-11-06).** A lifecycle is built from generic pieces: queries, signals, times. A source may record its lines' writer's **uptime** (`LogSource::uptime`, DLT's `tmsp`); nothing else knows DLT.
- **One lifecycle per entity (2026-11-07).** v1 had a key field (`boot(ecu)`: lifecycles for every ECU at once, one set of rules). The owner wants to define one ECU (or application) at a time, each with its own reset criteria. A lifecycle has **its lines** (a query, e.g. `ecu == ECU1`); everything else is about them.
- **No ambiguous `@uptime` (2026-11-07).** v1 had a built-in `@uptime` signal: in a view of several ECUs it mixes their clocks. Now `reset uptime` means the uptime **its lines** carry, so whose uptime it is follows from the lines. There is no `@uptime` signal.
- **Binding a signal only resets it (2026-11-07, "A").** `per(ecu1)` resets the signal's state each time `ecu1` starts; the signal's own query decides its lines (no hidden filter; one ECU's state can reset on another's reboot, e.g. the gateway's). Picking a lifecycle in the form fills an empty query with the lifecycle's lines.
- **Reuse library queries (2026-11-07).** Every query of a lifecycle can be a library query by name, `{pack.query param=value}`, so a fix in its pack reaches every lifecycle using it.
- **Defaults are dashed (2026-11-06).** A default is assumed, not from the log: dashed in the plot, `idle (default)` in the panel. Boundaries show in the plot and the log view ("lifecycle boundaries are nice"). In the plot only for the lifecycles ticked in the Signals panel (owner, 2026-11-07: "lifecycles enable-able in plots"; before, every lifecycle's starts were drawn): a ticked lifecycle is its starts, no lane; ticked alone, the plot spans its lifecycles. The log view still shows every lifecycle's boundaries.

## Definition

| Part | Spec | Meaning |
|---|---|---|
| Name | `ecu1:` | `@ecu1` (the lifecycle's number), `.ecu1`, `per(ecu1)` |
| Its lines | `lines QUERY` | the entity's lines; default every line |
| Starts when | `reset uptime [tolerance 5s]` | the uptime its lines carry goes back by more than the tolerance (default 5 s; lines of different writers interleave) |
| | `reset @signal [tolerance …]` | a numeric signal of the tab goes back (a dmesg `[ 12.3]` capture in a text log) |
| | `start QUERY` | one of its lines matches |
| | `gap 30s` | its lines stop for longer; ends the previous one as `lost` |
| Ended by | `planned QUERY`, `crash QUERY` | the outcome (the last marker in it wins) |
| | `late 10s` | a first line above this uptime is a "late start" finding |

`QUERY` is NFD or `{pack.query name=value}`. The spec line (CLI `--lifecycle`, the saved signals file as `lifecycle …`):

```
ecu1: lines {sys.of ecu=ECU1}; reset uptime; planned {sys.shutdown}
gw: lines ecu == GW; reset uptime tolerance 2s; planned payload ~ /shutdown requested/
app: lines apid == NAV; start payload ~ /NAV started/; crash payload ~ /panic/
```

**A lifecycle lasts** from its first line to the next one's first line. Every line in between is in it, whoever wrote it (`@ecu1 == 2 AND ecu == GW` is "GW's lines during ECU1's second boot"). Before the entity's first line there is none.

### Outcomes

| Outcome | When |
|---|---|
| `planned` | a `planned` marker matched in it |
| `crash` | a `crash` marker matched in it |
| `unclean` | no marker, and a new one started |
| `lost` | it was ended by a `gap` |
| `open` | the log ends in it |

### Validation

Within one lifecycle, `time − uptime` is the boot time and should stay constant. Delivery delays only make the time later, so the least value is the boot time, and how far the others sit above it is the **spread**. Findings, each with its line:

| Finding | Meaning |
|---|---|
| missed reset | the uptime went back by more than the tolerance but nothing started a lifecycle |
| false reset | a start line or a gap started one but the uptime kept counting |
| late start | its first line has an uptime above `late`: its early lines are missing (or the log started during it) |

The definition wins: lifecycles follow what was written; a mismatch is only reported. A finding that repeats is listed once per lifecycle with how often (stress test, 2026-11-07: a lifecycle over several ECUs' uptimes had 2 M missed resets; now `missed reset ×90838`, with a hint to give it one writer's lines).

## Signals bound to a lifecycle

```
spec := NAME [ "(" GROUP ")" ] ( ":" PART )* "=" QUERY
PART := KIND | SCOPE | "per(" LIFECYCLE ")" | "default(" VALUE ")"

ota(s):enum:per(ecu1):default(idle)=ecu == ECU1 AND /OTA state=(?<s>\w+)/
```

- **`per(ecu1)`:** the state on a line is the last event since the line's lifecycle started, else the default, else unset. It replaces the per-file reset, so a boot over several files (log rotation) keeps its state.
- **`default(VALUE)`:** checked against the kind (a number for `num`, text for `enum`, none for `time`). Not an event: `.event` doesn't see it, `.changed` fires at the first event only if it differs. Without `per`, a default applies from each file's start.
- The lifecycle is a signal too: `@ecu1` is its number (`@ecu1 == 3 AND level >= ERROR`), `.ecu1` an event where one starts.

## Surfaces

- **CLI:** `--lifecycle SPEC` (repeatable; `{pack.query}` from `--pack`), `nerka lifecycles FILE` (every lifecycle defined, or `-n NAME`; a table, findings on stderr; `--json` with an `issues` array; `--count QUERY` per lifecycle; `--strict` fails on findings). Bound signals and `@ecu1` work in `filter` / `search`.
- **GUI, Signals panel:** the form makes a signal or a lifecycle. A lifecycle's form: Name; Its lines; Starts when (☐ its uptime goes back by more than …, ☐ one of its lines matches …, ☐ its lines stop for …); Ended as planned by; Ended as a crash by; Late start above; **Preview** (what it finds in the file: count, outcomes, findings, or why it can't). Every query field takes NFD or a library query: 📚 picks one (its parameters to fill), or drag one from the Library onto the field. A signal's form has Per (a lifecycle) and Default. Every saved signal and lifecycle has an on/off box (owner, 2026-11-07): off keeps it (saved as `off …`, listed greyed) but it isn't computed nor usable in queries. Under the signals, each lifecycle's list (or why none were found, e.g. a `reset @signal` that isn't in the tab; it used to say "finding them…" for good): number, first line (click: go there), duration, outcome (red when bad), `▾ filter` (`@ecu1 == N AND (its lines)`), its findings (click: go to the line).
- **Plot:** a dashed line where each lifecycle starts, labelled `ecu1 #2` where there is room, red after a bad end. Each plotted lifecycle has a row of labels and a dash of its own (owner, 2026-11-09: with ECUs booting together only one was labelled); the hover over a start line names every lifecycle starting there. A bound signal's lane has a point where each lifecycle starts: its default (dashed) or nothing (unset). An enum lane is grey where its value changes within a pixel (owner, 2026-11-07: a colour there looked like one value, zooming in showed another); a default band is labelled `none (default)`.
- **Log view:** a line with the label above each lifecycle's first row.

## Execution and cost

Its lines come from the engine (the fast path for `ecu == ECU1`); the start and marker queries too. Then one walk over its lines in order; their time and uptime are read in parallel chunks. Nothing per line is kept: a few numbers per lifecycle. Cached per source.

512 MiB DLT (6.25 M messages, 3 ECUs; `gen_dlt` simulates reboots), release, Windows: `ecu1: lines ecu == ECU1; reset uptime; planned payload ~ /shutdown requested/` 158 ms (16 lifecycles), `gw` 153 ms (12); `apid == NAV AND level >= WARN` unchanged at 16–17 ms. (v1, one key over all ECUs: 267 ms.)

## Stress tests (2026-11-07)

- **Random logs against a plain reference** (`lifecycle/tests.rs` → `stress`): 3 seeds × 6 definitions over 150 000 lines (across chunk boundaries; jitter, gaps, lines without an ECU, a time or an uptime): the lifecycles found equal a line-by-line walk's; a bound signal's state equals the last event since its lifecycle started; filters agree on the fast and naive paths.
- **Fuzzed specs** (`fuzz`): 50 000 lifecycle and signal specs from random pieces and clauses: no panic, error positions inside the text, what is written back reads back the same.
- **Edges** (`edges`, `merged`): an empty log, lines matching nothing, one line, a start line that is also a marker, a reset signal bound to its own lifecycle, a filtered bound signal, two DLT files merged by time.
- **Found and fixed:** a lifecycle whose first line carried no uptime said its lines carry none (now any of its first thousand); repeated findings (above).
- **512 MiB DLT and the same as a pcap:** three per-ECU lifecycles 145–152 ms each; the pcap finds the same lifecycles (its times are the datagrams'); bound-signal filters 167–206 ms including the detection; a whole-log start-line lifecycle 193 ms (546 ms before findings were counted).

## Not built yet

Lifecycles stored in packs (a pack shipping definitions; today they live with the saved signals and on the CLI), a derived uptime from the time for logs without one, the findings clock jump and noisy, findings as bookmarks, next / previous lifecycle navigation, start / end / count columns in the GUI list, "add one per ECU" (copy a definition for each value of a field), `--probe`. **Later:** checks; nested lifecycles; transition rules and timeouts.
