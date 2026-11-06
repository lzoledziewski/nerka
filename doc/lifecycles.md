# Lifecycles — design

_Status: design for review, written 2026-11-06 with the owner. Nothing is built yet. Current state and decisions: `status.md`._

## Goal

Logs from devices span several **lifecycles**: an ECU boots, runs, shuts down or crashes, and boots again. Most questions only make sense per lifecycle: "3 resets" means nothing over a whole trace, but "an OTA job in `downloading` when the boot ended unclean" is a finding. Several entities (ECUs, nodes, hosts) have their own lifecycles in one view, especially when files are merged by time.

v1 adds:

1. **Lifecycle definitions**: when a lifecycle of an entity starts and how it ends.
2. **Signals bound to a lifecycle**: their state resets when a new lifecycle starts, optionally to a default value.
3. **Validation** of the definition against the time and the uptime, with findings that point to lines.
4. **Boundaries** in the plot and the log view, a lifecycle table, and the CLI.

It is the groundwork for **checks** (known-issue signatures that run on every trace), which come after it. General state machines (transition rules, timeouts, events from other machines) are left out on purpose. The owner's usage is "state straight from a signal or a regex, reset on a new boot", which signals bound to a lifecycle already cover.

## Nothing format-specific (owner, 2026-11-06)

The engine knows no DLT here. A lifecycle is built from generic pieces: queries, signals, times and fields. A source may provide a built-in **`@uptime`** the way it provides `@time` (DLT gives it from the header's `tmsp`). Ready-made definitions, such as `boot` per `ecu` for DLT, ship as **data** in a pack that can be edited or replaced, not as code.

## Definition

```
lifecycle boot
  key      ecu                  # optional: one lifecycle stream per value
  reset    @uptime              # a num signal that goes back: a new lifecycle
  start    sys.boot             # a query: a new lifecycle
  gap      30s                  # no line of the key for longer: a new lifecycle
  planned  sys.shutdown         # an outcome marker
  crash    sys.watchdog         # an outcome marker
```

- **Name:** the lifecycle's name, used by `@NAME`, `.NAME` and `:per(NAME)`.
- **`key`** (optional): a field (`ecu`, `host`) or a signal (`@node`). Each value has its own lifecycles, and lines are only compared with lines of the same key. Without a key there is one stream.
- **Start triggers**, any combination, at least one (or a derived one, see `@uptime`):
  - `start QUERY`: a line matching it starts a lifecycle.
  - `reset @SIGNAL [tolerance T]`: a numeric signal goes **back** by more than the tolerance. Lines from different writers interleave, so small steps back aren't resets. The default tolerance comes from validation (the measured spread), else 1 s.
  - `gap T`: no line of the key for longer than T (needs times). It ends the previous lifecycle as `lost`.
- **Outcome markers:** `planned QUERY`, `crash QUERY`. More names may be allowed later; v1 has these two.
- Queries are library query ids (`pack.query`, with parameters as in selections), or NFD in quotes.

The CLI takes the same lines joined with `;`: `--lifecycle 'boot(ecu): reset @uptime; planned sys.shutdown'`.

### Outcomes

A lifecycle ends where the next one of its key starts, or where the log ends.

| Outcome | When |
|---|---|
| `planned` | a `planned` marker matched in it (the last marker wins) |
| `crash` | a `crash` marker matched in it |
| `unclean` | no marker, and a new lifecycle started |
| `lost` | it was ended by a `gap` |
| `open` | the log ends in it |

### Numbering

Lifecycles are numbered per key from 1, in row order. In a merged view that's the time order. The first lifecycle of a key may have started before the log did (see "late start").

## `@uptime` and validation

| Have | Then |
|---|---|
| `@time` only | Lifecycles from `start` / `gap`. **`@uptime` is derived** as `@time − the lifecycle's first time`. |
| `@uptime` only | Lifecycles from `reset @uptime` (where it rolls back). |
| both | Lifecycles as defined, **validated** against the boot time. |

**Boot time.** Within one lifecycle `@time − @uptime` is the moment the entity booted, and it should stay constant. Delivery delays (buffering, transport) only ever make `@time` later, so the lifecycle's **minimum** of `@time − @uptime` is the boot time. How far the values sit above it (the **spread**) is the lifecycle's natural tolerance.

**Findings**, each with the line that shows it:

| Finding | Meaning |
|---|---|
| missed reset | The boot time jumps (uptime rolled back, time went on), but no trigger started a lifecycle. |
| false reset | A trigger started a lifecycle, but the boot time stayed the same: uptime kept counting. |
| clock jump | The boot time shifts while uptime keeps rising: the recording clock was set (NTP, a logger), not a reboot. Reported, not a reset. |
| late start | The lifecycle's first line came long after its boot (uptime 38 s): its early lines are missing. Information. |
| noisy | The spread is large: delivery delays are big, so the tolerance is loose. |

**The definition wins:** lifecycles always follow what was written, and a mismatch is only reported, so results never change silently. `reset auto` lets the uptime decide instead (an open question, see below).

**Where findings show:** a status column in the lifecycle table, a count in its header and in the tab's status ("5 lifecycles, 1 unclean, 1 finding"), bookmarks named after them (`lifecycle: missed reset`), and warnings on stderr in the CLI (`warning: A#2 line 48 211: late start, first line at uptime 38.0 s`). In `--json` they are an `issues` array, and `--strict` makes a finding fail the run (CI, agents).

## Signals bound to a lifecycle

Today a signal's state resets at each file boundary. Binding it to a lifecycle replaces "file" with "lifecycle". The spec gets two optional parts:

```
spec  := NAME [ "(" GROUP ")" ] [ ":" KIND ] [ ":" SCOPE ] [ ":per(" LIFECYCLE ")" ] [ ":default(" VALUE ")" ] "=" QUERY

ota(s):enum:per(boot):default(idle)=/OTA state=(?<s>\w+)/
retries:per(boot):default(0)=/retry #(?<retries>\d+)/
```

Parentheses, not `per=boot`: the spec is split at its first `=`, and the head already uses `(GROUP)`.

- **`per(LIFECYCLE)`:** the state at a row is the last event at or before it **of the same key, in the same lifecycle**. Otherwise it's unset, or the default. With a keyed lifecycle the signal is per key too: ECU B's state never shows on ECU A's lines, and A's reboot doesn't reset B. It **replaces** the per-file reset, so a boot split over several files (log rotation) keeps its state across the file boundary.
- **`default(VALUE)`:** the state from the lifecycle's first line (unbound: the file's first line) until the first event. It's checked against the kind: a number for `num`, any name for `enum`, not allowed for `time`. A default **isn't an event**: `.event` doesn't fire, and `.changed` fires at the first real event only if the value differs.
- **Independent of `all` / `filtered`;** both can be combined.
- **Cost:** a state lookup is already a binary search in the signal's cached events. Binding adds the key's lifecycle start for the row, computed once per tab, like the events.

The lifecycle itself is a signal: **`@boot`** is the lifecycle's number for the row's key (`@boot == 3 AND level >= ERROR`), and **`.boot`** is the event at each start (searches, bookmarks, navigation).

### How values look (owner, 2026-11-06)

| Look | Meaning |
|---|---|
| solid | a value from the log: an event set it |
| **dashed** | a default nerka assumed, not from the log, until the first event; `idle (default)` in the Signals panel |
| gap | unset: no default, no event yet |

"Dashed = nerka assumed it" is the rule for any value the definition supplies, including later kinds.

## Surfaces

- **Plot:** a vertical line at each lifecycle start, labelled `A boot 2`, red when the lifecycle before it ended `unclean` or `crash`. With several keys a line spans only that key's lanes. Bound signals get a lane per key (`A · @ota`, `B · @ota`). Boundaries show whether or not any signal is bound (owner, 2026-11-06: "lifecycle boundaries are nice").
- **Log view:** a thin separator above each lifecycle's first row, with the same label and colour. Next / previous lifecycle navigation works like bookmarks.
- **Lifecycles table** (a panel like Signals): key, number, start, end, duration, outcome, findings, and optionally the count of a chosen query's matches. A click jumps; "Filter to this lifecycle" sets `@boot == N AND ecu == K`.
- **Signal editor:** a lifecycle and a default per signal.
- **CLI:** `nerka lifecycles FILES` (the table, `--json`, `--strict`); `--lifecycle SPEC` for a definition outside packs. Bound signals work in `filter` / `search` as any signal does.

## Execution

One pass in row order, cancellable with `&Progress`:

1. Evaluate the start and marker queries through the engine (fast path), and the `reset` signal's events (already cached).
2. Walk the matches per key, plus the gaps where times are known, to produce per key the sorted lifecycle starts with their outcomes and findings.

The result is small (a few entries per lifecycle, nothing per line). A row's key is read from the row when needed (a field fetch, or a signal's state). `--stats` numbers will be reported against a filter over the same queries on the 1 GiB DLT benchmark.

## v1 scope

1. Definitions in packs and on the CLI: key, `start` / `reset` / `gap`, `planned` / `crash`, outcomes, numbering.
2. The source's `@uptime` (DLT: `tmsp`) and a derived one from `@time`; validation and findings.
3. `:per(…)` and `:default(…)` on signals, defaults drawn dashed.
4. `@NAME` / `.NAME`.
5. Boundaries in the plot and the log view, lifecycle navigation, the Lifecycles table, the signal editor.
6. CLI `nerka lifecycles`, `--lifecycle`, `--json`, `--strict`.
7. A shipped `boot(ecu)` definition for DLT as a pack.
8. Engine tests (every trigger and outcome, keys, merged files, signals bound across a file boundary), parser tests with error positions, the grammar in `signal.rs` and the pack format docs.

**Later:** checks (rules scoped to lifecycles: count, rate, sequence, absence, outcome), nested lifecycles (boot › ignition › OTA), transition rules and timeouts, events from other lifecycles.

## Open questions

- **`reset auto`**: should the uptime be allowed to override the definition, or should results only ever follow the definition?
- **Lines without a key** (a DLT message without an ECU id, a text line where the key signal is unset): v1 proposal is that they belong to no lifecycle, `@boot` is unset there, and bound signals act unbound.
- **Tolerance defaults** for `reset` and the threshold for "late start" need real traces. A `nerka lifecycles --probe` printing the detected resets and boot-time spread per key would let them be set from data.
- **Pack syntax** of the `lifecycle` block next to queries, and whether a pack can ship a definition that a selection turns on.
