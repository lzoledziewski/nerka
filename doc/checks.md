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

A lifecycle's scope is its time span (from its first line to the next one's): lines of any entity in it count. **A `per` check's overall result is the whole log judged at once, reboots included, and it counts like any result** (owner, 2026-11-10, review 2026-11-09 U4 cancelled: the more general rule; one check answers both per lifecycle and across them, and nothing before the first lifecycle is missed). So a gap that spans a reboot shows overall and in no lifecycle; to ignore it, read the lifecycles' results. Tests on time need the lines' times (the source's, or a time signal).

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

- **Bug (owner, 2026-11-07): "how many" as an issue didn't match "how many" as a fact.** It said 1× (the condition held) with all its lines chained in one piece of evidence. Now it happens as often as there are lines, where its condition holds (none: any line), each line its own evidence. A test (`consistency`) runs every test as a fact and as an issue on random logs and checks they agree in every scope; on the 512 MiB DLT every pair agrees, and the counts equal plain filters' (49 064 overall, 3 295 in ECU1's second lifecycle). Durations show to the ms (a 1.04 s stop showed as "1.0 s" next to an issue for "longer than 1 s").

- **Signals in checks (owner, 2026-11-07: "can checks reference signals?"):** they couldn't (`unknown signal '@v'`, though the same filter worked). A check's queries (its lines, and `then` / `missing`'s other query) now read the tab's signals that are on, plain or bound to a lifecycle: `lines ecu == ECU1 AND @state == "session timeout"; count; per ecu1`. On the 512 MiB DLT a check finds what the same filter finds (99 729 lines for `@v > 250`; 130 707 for a lifecycle-bound enum); a check without signals is unchanged (26 ms). A check's `value` still reads its own query's named group, not a signal.

## Third round: cascading, plot (owner, 2026-11-07)

Owner: "some of the tests should be able to be cascaded", and plot integration "configurable: enable a check without plotting it (low-priority checks would obscure the plot)". Asked which cascading: a check feeding a check ("could be implicit, e.g. a signal with values, boolean"), several tests in one check, a sequence of steps (not "only inside another's results").

- **A check is a signal** `@name` (its name in lower case, other characters `_`: `Timeout unanswered` → `@timeout_unanswered`). An event where each occurrence shows (its last line: where it is known); its value counts them, 1, 2, 3, …, from 0 again in each lifecycle for a check `per` one. A fact on a rate, a gap or a value carries its measure there instead. So `@x.event` is where it found something (another check's lines: `lines @unanswered.event; rate 2 per 60s; per ecu1`), `@x > 0` is "has happened in this lifecycle", and filters use it too. Checks run in the order they use each other's signals (`Checks::run_all`); a circle, or a check using a failed one, says so. A signal of the tab with the same name wins.
- **Several tests in one check:** `count >= 3000; gap 700ms` is an issue where both find one (as often as they do together); `any` where one does. A fact has one test.
- **A sequence of steps:** `then` / `missing` clauses chain, each after the step before: `lines /session timeout/; then /connection established/ within 1s; missing level >= ERROR within 50ms`. `then` steps must happen; a `missing` step is an expectation (the issue is that it doesn't happen; not judged when the scope ends first). One `then` or `missing` is a sequence of one, as before.
- **Plot:** each check has 📈 (off at first): it plots the check's signal as a lane (a count rising at each occurrence, from 0 in each lifecycle). The Signals panel lists checks' signals too, read-only, to try (owner, 2026-11-07: "why isn't it visible in signals?"): plot box, value on the current line, "check" (click: the check in the Checks pane); no ✎ or −, the check is edited and switched off in its pane.
- **Bug (owner, 2026-11-07): with long lines the pane's left part couldn't be seen.** Its content grew as wide as its longest line (a long name, an error, a note, a log line), wider than the room beside the Signals panel, and its left part went under the log view. Text in the pane wraps now, and rows of buttons and labels wrap; a test renders it 340 wide with long names, errors, notes, log lines and queries and checks its content fits.
- **Signal name conflicts were silent (owner, 2026-11-07):** a check whose signal name was taken (a signal of the tab, or a check before it in the list) made no signal, and looked like a check that finds nothing. Now its row says why under it (⚠, 📈 off), the form says so while typing the name, the signal form says so when a new signal takes a check's name, and `nerka check` prints it.
- **Bug (owner, 2026-11-07): checks from a test launch stayed.** `--check` / `--signal` went into the saved lists, and the next save wrote them to the files. Now they last the session (a saved definition they replaced is what is saved); adding or editing one in a form makes it the user's.
- **Form:** ➕ Another test (and all / any); "followed (or not) by" lists steps, each followed by / not followed by, its query and time, ➕ Step. The form shows the check's signal name.
- **Tests:** steps, several tests, checks as signals (feeding in any order, circles, failures, names), a randomized walk for sequences (random 1–3 step sequences on 20 000-line logs against a plain walk).
- **512 MiB DLT:** a timeout without its answer per ECU1 lifecycle: 14 960 in the lifecycles; a check counting `@unanswered.event` finds 14 960; bursts of them per lifecycle 5, 13, 1, … `count >= 3000; gap 700ms` per lifecycle: the sum of the two where both find one (6 385 = 6 384 + 1), ok where only one does. Eight checks 0.9 s together; a single check without signals unchanged (25 ms).

## Next, to decide with the owner

- Marks: the evidence in the log view's gutter, the plot, and a badge in the lifecycle list.
- Profiles, and where checks live (today: one list for every tab).
- More tests: a signal's condition held for a time (`@temp > 90 for 5s`), conditions on the lifecycle's outcome, several tests in one check, a check using another's result.
