# Patterns in the GUI (design, not built)

Status: **proposed** (2026-11-23), for the owner to settle. The engine and
the CLI are built (`core/src/pattern.rs`, `nerka patterns`, `nerka compare`,
`docs/user/cli.md`); this is the GUI and the tuning around them.

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

A saved kind like checks and signals: spec lines in
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

**Scope:** the fields of the context (`ecu`, `apid`, `ctid`, any field the
file has), `==` with `*` as a wildcard, joined by `AND`; `*` alone is every
line. The GUI form offers the three DLT fields as boxes (empty = any); the
file may hold any NFD condition for the rare case that needs more (open
question 2).

**Actions:**

| Action | Effect |
|---|---|
| `keep WORD…` | the word never becomes a slot: a line differing there starts its own template; a merge putting it in a slot is refused |
| `value /REGEX/` | tokens matching it are values (`<*>`), like those holding a digit |
| `literal /REGEX/` | tokens matching it are never values, though they hold a digit (`ECU1`, `v2`) |
| `max-slots N` | a pattern there has N slots at most; a merge making more is refused |
| `apart WORD WORD…` | patterns with these words in one slot are kept apart (`{voltage=\|undervoltage}`) |
| `header WORD…` | known header/footer words: passed over, whatever the detection says |
| `no-merge` | stage 2 doesn't merge templates there |
| `ignore` | lines there are left out of patterns and compares (`apid == GPS: ignore`; `*: ignore /heartbeat/` for messages matching) |
| `set KEY VALUE` | a setting for that scope only (`ctid == TCP: set similarity 0.7`) |

The first matching rule of each kind applies; the list is read top to
bottom.

## The Patterns pane

**🧩 Patterns** in the top bar (as ✔ Checks): a pane per tab, docked like
the Checks pane or in a window of its own like the plot (open question 1).

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
- Later (open question 5): **groups**, e.g. every `crash` lifecycle against
  every `planned` one.

## The tuning pane (🛠)

For the alpha rounds; visible in alpha builds, behind ⚙ Settings →
*Developer: pattern tuning* after (open question 3).

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

## Order of work (alpha rounds)

1. **Core:** `PatternConfig` (settings now constants become fields), rules
   with scopes, the `patterns` file and bundle kind, the CLI flags, and the
   verdict counts. Tests per rule. The LogHub benchmark takes a config too,
   so a setting's effect on GA / FGA / WGA is measured.
2. **α1:** the Patterns pane, off until ▶ Run: list, unfold, go to line,
   Filter / Search / Lines, on all lines, the filter result or one
   lifecycle.
3. **α2:** Compare (lifecycles, filters, tabs) with Only / Ignore and
   ignoring from the results, and the tuning pane (settings, rules, re-run,
   change since the last run, why apart).
4. **α3, from the owner's use:** ➕ To library with state signals, groups
   of lifecycles compared, pooling the header sample per context (rare
   contexts), `nerka anonymize` if real traces are needed for tuning.

## Open questions for the owner

1. **Where the pane lives:** docked like Checks, a window of its own like
   the plot, or a tab of the bottom pane next to the search hits?
2. **Rule scopes:** the three DLT fields with `*` (the form), plus any NFD
   condition in the file for the rare case (proposed), or fields only?
3. **The tuning pane after the alphas:** keep it behind a Settings switch
   (proposed), or remove it and keep only the file?
4. **Where the configuration lives:** one global file (proposed, exported
   with bundles), or also per pack, so a project's rules travel with its
   queries?
5. **Comparing groups** of lifecycles (every crash against every planned
   boot) in the first round, or later?
6. **`keep WORD`:** when two messages differ only in a kept word, they stay
   two patterns (proposed). Should a kept word also refuse to be a state
   value (`{D|P}`), or only a `<*>` slot?
