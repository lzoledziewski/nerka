# Instances of a parameterised query

A parameterised query (`query drops(cam)`) run for several values of its
parameter at once, to track several instances side by side (owner,
2026-11-15: so far the query was duplicated by hand, one per camera).
Built for 0.2.2: `features.md` → "Instances of a parameterised query".

## Decisions (owner, 2026-11-15)

- **A parameter can have several values** (alternative C of five weighed:
  instances under a pick, picks of their own, values per parameter, values
  per parameter with per-pick overrides, values per query). They are given
  in the selection's **Parameters** box, one field per value (no commas:
  raw regex values hold them, `a{1,3}`), **+ value** and ✖; + on a library
  query already picked adds a value instead of a second pick.
- **Every picked query using a parameter with several values is an
  instance per value**, a part of its own: shown as `drops (CAM1)`, with
  its own colour (the first the query's), in the query's role. With
  several such parameters, every combination; the selection shows the
  count next to the query (`× 6`). A field not filled in yet and a value
  given twice don't count (the second is marked ⚠).
- **Values belong to the parameter, not to a query:** one query can't have
  other values of `$cam` than another (cases weighed and accepted: rare;
  per-pick overrides could come later). Instances of a query share its
  text and settings.
- **Signals follow the pack's group names (no names made up):** a group
  that uses the parameter (`(?<drops_$cam>…)`) gives a signal per instance
  (`@drops_CAM1`, `@drops_CAM2`); one that doesn't (`(?<drops>…)`) is one
  signal from every instance, a valid use, never a conflict (the same
  query, the same settings).
- **`signals:` lines take parameters:** `signals: drops_$cam:num:per(ecu_$cam):default(0)`,
  filled in per instance as the text is; an option may use only the
  parameters its group's name uses (checked when the pack is read), so a
  signal several instances feed has one set of options. **Save to pack**
  writes the parameterised form back (`drops_$cam`), not the instance's
  name.
- **When values apply:** a value counts once Enter is pressed or its field
  is left; filter picks still need **Apply**, signal picks follow then.

## Design

**Core (`pack/expand.rs`).** `ParamValues` (several values per name);
`expand_values` makes each picked query (or a label's) an instance per
combination of the parameters it uses that have several values
(`instances`), the first values shared (`first_values`). A part keeps its
pick's index and the values of those parameters (`Part::pick`,
`Part::args`), since instances share the query id. `Pick::QueryWith` (a
pick with its own arguments) stays in core; nothing in the GUI or CLI
writes it.

**GUI (`ui/src/library.rs`).** `Picks::params` holds a list per name;
`params_ui` shows them with drafts that count on Enter or focus leaving
(`Picks::drafts`); `instance_count` gives the `× N`; `expand` maps parts
back to nodes by pick index and colours each instance
(`Node::instance_color`).

**Presets (`preset.rs`).** `param cam = CAM1` then `param cam += CAM2`: a
new operator, so an older nerka refuses the preset instead of keeping only
one value. **CLI.** `-p cam=CAM1 -p cam=CAM2`; `--parts` names each
instance's values. `nerka-gui --param` likewise.

## Open

- Different values of one parameter per query (`drops` for CAM1 and CAM2,
  `fps` for CAM1 only) or per role: per-pick overrides, if it is needed.
- Lifecycle rules that use a library query (`{pack.query}`) take the first
  values. Instances of a lifecycle (one per ECU) are the same idea:
  `lifecycles.md` lists "add one per ECU" for later.
