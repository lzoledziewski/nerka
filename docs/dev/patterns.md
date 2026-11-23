# Patterns in the GUI (design; core configuration and α1 built)

Status: **settled** with the owner (2026-11-23). Built: the engine and the
CLI (`core/src/pattern.rs`, `nerka patterns`, `nerka compare`); round 1
below, the core configuration (2026-11-24: `core/src/pattern/config.rs`,
the `patterns` file and bundle kind, `--pattern-config` / `--set` /
`--rule`; user guide `docs/user/cli.md#the-pattern-configuration`); and
α1, the Patterns window (2026-11-24: `ui/src/patterns.rs`; user guide
`docs/user/gui.md#patterns`). Not built: α2 (Compare, tuning pane) and
α3.

## What the owner asked for (2026-11-23)

- Patterns in the GUI, with **comparing lifecycles**.
- A **development pane** to tune the algorithm **without a rebuild**: it
  goes through a round of alpha releases, and the settings must be
  adjustable on real traces in between.
- **Rules**, simple and scoped by `ecu` / `apid` / `ctid` or a wildcard:
  e.g. a keyword that must never become a slot, the most slots a pattern
  may have, and whatever else can be tuned.
- **Not on by itself (owner, 2026-11-23):** patterns are not enabled or
  computed automatically for now; they run only when asked (▶ Run).
- **Compare chooses its content (owner, 2026-11-23):** the lines compared
  can be narrowed to what matters, and unwanted content ignored.

## Principle

The core does everything (spec §4): one **pattern configuration**
(`PatternConfig` in `core/src/pattern.rs`) holds every setting and rule,
and both the GUI and the CLI read it. The GUI only edits it and shows
results. A setting changed in the pane is what `nerka patterns` does with
the same file, so a result seen in the GUI can be reproduced, shared and
put in a bug report.

## The configuration

One global file (owner, 2026-11-23; not per pack), a saved kind like
checks and signals: spec lines in
`%APPDATA%\nerka\patterns` (Linux `~/.config/nerka/patterns`), one setting
or rule a line, `#` comments, unknown keys an error with their line. A new
bundle kind (`patterns 1`, `docs/dev/bundles.md`), so a tester's settings
can be exported and imported. CLI: `--pattern-config FILE`, and `--set
KEY=VALUE` / `--rule 'RULE'` for one run.

```
# Settings (the defaults as built today)
similarity 0.5            # stage 1: share of words alike to join a template
new-wildcards 1           # stage 1: words a line may turn into wildcards at once
new-slots 1               # stage 2: slots one merge may make
side 4                    # stage 2: tokens on the longer side of a slot
state-values 16           # a slot with more values is <*>, not a state
header sample 32 min 8 share 0.5 places 3   # header/footer detection
merge on                  # stage 2 at all

# Rules: [scope:] action
apid == NAV: keep Elapsed              # a word that never becomes a slot
ecu == ECU1 AND apid == GPS*: max-slots 2
*: value /[a-z][\w-]*(\.[a-z][\w-]*)+/ # hostnames are values (Proxifier)
apid == PWR: apart voltage= undervoltage   # never in one pattern
ctid == TCP: no-merge                  # stage 1 only here
*: header INFO: WARN: ERROR:               # known header words, not detected
```

**Scope (owner, 2026-11-23: what the engine groups by, and persistent):**
the fields the engine keeps lines apart by, its context: for DLT `ecu`,
`apid`, `ctid`, `type` and `mode` (the short text fields; not the payload
or the numbers), for a text file with a profile its text fields; `==` with
`*` as a wildcard, joined by `AND`; `*` alone is every line. No other NFD:
a rule applies to whole contexts, as the engine works on them. The GUI
form offers those fields as boxes (empty = any). Rules are saved with the
configuration (a rule made in the pane too), never only for the session.

**Actions:**

| Action | Effect |
|---|---|
| `keep WORD…` | strict (owner, 2026-11-23): the word is never in a slot, neither `<*>` nor a state's value: a line differing there starts its own template, and a merge putting it in a slot is refused (`keep undervoltage`: `undervoltage <*> mV` a pattern of its own, not a value of `{voltage=\|undervoltage} <*> mV`) |
| `value /REGEX/` | tokens matching it are values (`<*>`), like those holding a digit |
| `literal /REGEX/` | tokens matching it are never values, though they hold a digit (`ECU1`, `v2`) |
| `max-slots N` | a pattern there has N slots at most; a merge making more is refused |
| `apart WORD WORD…` | patterns with these words in one slot are kept apart (`{voltage=\|undervoltage}`) |
| `header WORD…` | known header/footer words: passed over, whatever the detection says |
| `no-merge` | stage 2 doesn't merge templates there |
| `ignore` | lines there are left out of patterns and compares (`apid == GPS: ignore`; `*: ignore /heartbeat/` for messages matching) |
| `set KEY VALUE` | a setting for that scope only (`ctid == TCP: set similarity 0.7`) |

Rules are read top to bottom. Of those setting one thing (`max-slots`,
`no-merge`, each setting of `set`) the first that applies counts; the
others add up (two `keep` rules keep both words).

As built (2026-11-24): `keep` and `apart` words never become or join a
wildcard in stage 1 (a token holding one), and stage 2 refuses a merge
putting a `keep` word in a slot or two items of an `apart` in one;
`max-slots` holds in both stages. `value` and `literal` act where a token
is masked (text a regex finds, not only whole tokens); a pattern with
`value` rules gets `\S+?` for its masked values in its query.
`state-values` is 2–16 (a slot keeps 16 values). `--stats` prints the
verdict counts the tuning pane will show: per stage-2 pass, templates kept
apart by two new slots, a long side, or a rule.

## The Patterns pane

**🧩 Patterns** in the top bar (as ✔ Checks) opens, per tab, a **window of
its own** (owner, 2026-11-23: for now), titled like the plot window
(`<window name> · Patterns · <file> — nerka`). Docking may come later.

```
┌ 🧩 Patterns ──────────────────────────────────────────────────────────────┐
│ Lines: [All ▾ | Filter result | Lifecycle ecu1 #2 ▾]  Field: [payload ▾]  │
│ [▶ Run]  [Compare…]  [🛠 Tuning]       142 patterns in 2.3 M lines, 1.1 s │
├───────────────────────────────────────────────────────────────────────────┤
│ 51.2%  1 204 133  NAV GPS   speed=<*> km/h heading=<*>                     │
│  0.8%     20 412  NAV GEAR  gear={D|P|N}                              ▸    │
│  0.4%     10 220  NAV GPS   GPS fix lost, <*> satellites                   │
│ ▾ 0.1%     1 203  TCP       session <*> to <*> {established|timeout …}     │
│        slot 3 (state): established 1 100 · timeout after <*> ms 103        │
│        [▾ Filter] [🔍 Search] [➕ To library] [Lines…] [⋮ Rule…]          │
└───────────────────────────────────────────────────────────────────────────┘
```

- **Off until asked (owner, 2026-11-23):** nothing is computed when a file
  opens or the pane is shown; only **▶ Run** runs it (no Auto, unlike
  checks), as a cancellable job with progress. The result stays until the
  next run, marked "out of date" when the file, the lines chosen or the
  configuration change; it is never re-run by itself.
- **Lines:** all, the filter result, or one lifecycle (`@ecu1 == 2`).
- **Rows:** share, count, the fields that tell it apart, the template with
  state values in colour; sorting by count, rarest first, first line.
  Click: go to its first line. ▸ unfolds the slots and their values
  (count, first and last line, click to go there).
- **Actions:** ▾ **Filter** (its query into the Filter box), 🔍
  **Search**, ➕ **To library** (a query in a pack, with its state slots as
  enum signals: `gear(g):enum=… /gear=(?<g>D|P|N)/`), **Lines…** (its lines
  in the bottom pane, as search hits are), ⋮ **Rule…** (below).

## Comparing

**Compare…** in the pane, and from the lifecycle list in the Signals
panel (right-click a lifecycle → *Compare with the previous one* /
*Compare with…*):

```
A: [Lifecycle ecu1 #2 (planned) ▾]   B: [Lifecycle ecu1 #3 (crash) ▾]   [▶ Compare]
── New in B ──────────────────────────────────────────────────────────────
   0 → 1   ECU1 SYS  kernel panic in task <*>                 first: B 712 044
   0 → 9   GW PWR    → slot 1: undervoltage                   first: B 705 120
── Gone from B ────────────────────────────────────────────────────────────
  14 → 0   ECU1 OTA  OTA state={idle|…|done}                  first: A 410 220
── Changed (score 10.8 or more) ───────────────────────────────────────────
 300 → 41  ×0.14  GW TCP  session <*> to <*> {established|…}  score 212
```

- **A and B:** a lifecycle, the filter result, a typed filter, or another
  open tab (two files).
- **Content (owner, 2026-11-23):** two filters apply to both sides before
  anything is grouped: **Only** (e.g. `ecu == ECU1 AND level >= WARN`, or
  library picks) and **Ignore** (e.g. `apid == GPS OR payload ~ /heartbeat/`:
  periodic or noisy messages that differ every boot and say nothing). Both
  are NFD, typed or picked from the library like the Filter box.
- **Ignoring from the results:** ⋮ on a row → **Ignore this pattern** or
  **Ignore this value**: it leaves the list (a count of ignored rows stays
  visible, with **Show ignored**), and is added to the compare's ignore
  list; **Save as rule** makes it an `ignore` rule in the configuration,
  so the next compare leaves it out too.
- Rows as in `nerka compare`: counts, ratio, score, the first line on the
  side that has it (click: go there; in another tab, switch to it).
- **Groups of lifecycles (owner, 2026-11-23: in the first round, it is
  cheap):** A and B can each be several lifecycles: by outcome (every
  `crash` against every `planned`), or picked (#2, #5 against #7). Each
  side is the union of its lifecycles' lines, so counts are compared as
  rates per side's lines, as now; the rows also say in how many of a
  side's lifecycles a pattern or value appears (`3/3` against `0/5`),
  which tells a constant symptom from a one-off.

As built (2026-11-24, α2; `ui/src/compare.rs`): **⚖ Compare** is a mode of
the Patterns window (beside 🧩 Patterns), not a window of its own. A side
is all lines, the filter result, a typed filter, lifecycles of `@name` by
number (`2`, `2, 5-7`) or every one of an outcome, or another tab (all its
lines or its filter result); a lifecycle side takes its lifecycles' own
lines (its lines rule), as the Signals panel's ▾ filter does. Only and
Ignore are typed NFD (library picks not yet). **Ignore this pattern /
value** hides the row, remembered by its template for this window's
session (so it stays hidden when compared again), with **Show ignored**;
it doesn't change the Ignore filter. **Save as ignore rule** (patterns
only) appends `SCOPE: ignore /REGEX/` to the configuration file
(`Patterns::ignore_rule`, `PatternConfig::append_saved`). The presence
(`A 3/3 · B 0/5`) shows on pattern rows (values: not yet). The Signals
panel's lifecycle rows have **⚖** (from #2): compare with the one before,
run at once. Changing the minimum score filters the rows without a new run.

## The tuning pane (🛠)

For the alpha rounds; visible in alpha builds, behind ⚙ Settings →
*Developer: pattern tuning* after (owner, 2026-11-23: a switch, off by
default; the configuration file works either way).

```
┌ 🛠 Pattern tuning ─────────────── config: patterns (saved) [Save][Load][Export] ┐
│ Stage 1   similarity [0.50]  new wildcards [1]  header: sample [32] min [8] ●   │
│ Stage 2   merge [on]  new slots [1]  side [4]  state values [16]                 │
│ Rules     ▲▼ apid == NAV: keep Elapsed                                  ✎ −      │
│           ▲▼ *: value /[a-z][\w-]*(\.[a-z][\w-]*)+/                     ✎ −      │
│           ➕ New rule                                                             │
│ Last run  stage 1: 2 140 templates 0.8 s · stage 2: 142 patterns 0.3 s           │
│           kept apart: 311 two new slots · 95 side too long · 12 by rules         │
│ Change    +4 patterns, −1, 2 changed   (since the run before)   [Show]           │
└──────────────────────────────────────────────────────────────────────────────────┘
```

- Each setting shows its default and **●** when changed; a click on ● resets
  it. Changing one re-runs the patterns (debounced, on the same lines).
- **Change:** what the last change of settings did: patterns new, gone and
  split or joined, with **Show** listing them, so the effect of a setting
  is seen on the trace at once.
- **Why apart / why joined:** pick two patterns → their alignment, and the
  rule or setting that kept them apart or joined them (the verdicts
  `accept` gives, with the numbers). This is what makes tuning on a real
  trace possible without reading code.
- **Rules from the pane:** ⋮ **Rule…** on a pattern or a slot value fills
  the form: *keep this word*, *these values apart*, *at most N slots here*,
  scoped by the pattern's `ecu` / `apid` / `ctid` (editable).
- Save / Load / Export: the configuration file, so a tester sends theirs
  with a bug report and it is loaded as it was.

As built (2026-11-24, α2; `ui/src/tuning.rs`): **🛠 Tuning** is the third
mode of the Patterns window, shown when the version is a pre-release
(`-alpha…`) or ⚙ Settings → Developer → Pattern tuning (`prefs`:
`pattern_tuning`) is on. The pane loads the saved configuration once and
from then on the window's runs use the pane's (saved or not); a change
runs again after 0.6 s on the same lines. **Change** follows each line
from the run before to this one (joins and splits; past 4 M lines only new
and gone). **Why** is `pattern::explain`: stage 2's verdict on the two
patterns as they came out, every word weighing 1 (an approximation:
header words weigh less in the run, and stage 1 may have kept lines apart
first). Rules are edited as text lines plus a form; ⋮ on a pattern row
fills the form (ignore it, at most N−1 slots, a slot's values apart, a
value kept). Save writes the settings that aren't the defaults and the
rules (`PatternConfig::to_text`).

## Order of work (alpha rounds)

1. **Core (built 2026-11-24):** `PatternConfig` (settings now constants become fields), rules
   with scopes, the `patterns` file and bundle kind, the CLI flags, and the
   verdict counts. Tests per rule. The LogHub benchmark takes a config too,
   so a setting's effect on GA / FGA / WGA is measured.
2. **α1 (built 2026-11-24):** the Patterns pane, off until ▶ Run: list,
   unfold, go to line, Filter / Search / Lines, on all lines, the filter
   result or one lifecycle. As built: an immediate viewport per tab (its
   own window; the plot's is deferred, this one is light enough to share
   the tab's state directly); lines can also be a typed filter; **Lines…**
   is in the window (▸ unfolds the pattern's own lines, kept by the run,
   a click goes there), not the bottom pane, which shows search hits, and
   🔍 searches by the pattern's query; 📋 copies the query. ⋮ Rule… comes
   with the tuning pane (α2), ➕ To library in α3.
3. **α2:** Compare (lifecycles and groups of them, filters, tabs) with
   Only / Ignore and ignoring from the results, and the tuning pane
   (settings, rules, re-run, change since the last run, why apart).
4. **α3, from the owner's use:** ➕ To library with state signals, pooling
   the header sample per context (rare contexts), `nerka anonymize` if real
   traces are needed for tuning.

## Settled with the owner (2026-11-23)

All the plan's questions are answered: the Patterns pane is a window of its
own (for now); rule scopes are the engine's context fields, and rules are
always saved; groups of lifecycles come with the first compare; `keep
WORD` is strict; after the alphas the tuning pane is behind a Settings
switch, off by default; the configuration is one global file, moved and
shared with settings bundles (not per pack).

## Assumptions to review (2026-11-24)

Taken while building the rounds unattended (owner, 2026-11-24: "you can
take assumptions, let me review them in the morning"); each is a small
change if it should be otherwise.

1. Rules: of those setting one thing (`max-slots`, `no-merge`, `set`) the
   first that applies counts; list rules (`keep`, `value`, `literal`,
   `apart`, `header`, `ignore`) add up.
2. `state-values` is 2–16: a slot keeps 16 values while grouping (more
   would cost time on every line).
3. Lines… of a pattern are shown in the Patterns window (▸), not in the
   bottom pane, which keeps the search hits.
4. Compare is a mode of the Patterns window, not a window of its own.
5. Ignoring a row in Compare hides it (by template, for the window's
   session); it doesn't rewrite the Ignore filter. Save as ignore rule
   writes a regex rule scoped by the pattern's telling fields.
6. A lifecycle side takes the lifecycles' own lines (their lines rule).
7. Presence (`3/3`) is shown for patterns, not for values.
8. Signals panel: ⚖ on a lifecycle row compares it with the one before and
   runs at once (the plan said a right-click menu; rows have buttons, not
   menus, so a button).
9. Tuning is a mode of the Patterns window; once opened, its configuration
   (even unsaved) is what that window runs with, until Revert.
10. Save writes only the settings that differ from the defaults, so a
    later default reaches files that didn't change it.
11. ⋮ "At most N slots here" proposes one less than the pattern has (its
    last merge made one too many); the form can be edited before Add.
12. "Why" explains stage 2 only, with every word weighing 1.
13. The tuning pane shows in any version with a `-` (pre-releases) and is
    a Settings switch otherwise.
