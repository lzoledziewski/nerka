# Instances of a parameterised query

A parameterised query (`query drops(cam)`) picked several times in one
selection, each pick with its own arguments, to track several instances side
by side (owner, 2026-11-15: so far the query was duplicated by hand, one per
camera). Built for 0.2.2: `features.md` → "Instances of a parameterised query".

## Decisions (owner, 2026-11-15)

- **Each pick carries its own arguments.** A pick of a parameterised query
  can set its own value for each parameter; a parameter it doesn't set takes
  the selection's shared value (the `param` values of today). A selection
  with one pick per query works as now.
- **Instances are argument sets of one pick.** The pick holds the query
  (its text, options and role) and a list of argument sets; **+ instance**
  adds a set. Instances of one query share its settings and can't
  disagree: editing the query edits it for all of them. The same query in
  another role (include CAM1, exclude CAM2) is another pick, as today. A
  shortcut makes several sets at once from a list of values, one per line
  (not comma-separated: raw regex values hold commas, `a{1,3}`). The same
  argument values twice are refused.
- **Each instance is a part of its own:** shown as `drops (CAM1)`, with its
  own colour; Include instances are OR'ed, Require ones each ANDed, Exclude
  ones each excluded; the instance's arguments fill its parent chain too.
- **Signals follow the pack's group names (no names made up):** a group
  that uses the parameter (`(?<drops_$cam>…)`) gives a signal per instance
  (`@drops_CAM1`, `@drops_CAM2`); one that doesn't (`(?<drops>…)`) is one
  signal from every instance, a valid use (several values feeding one
  signal), shown in the Signals panel as `@drops ← 3 instances`. Instances
  never conflict: they come from the same query with the same settings.
- **`signals:` lines take parameters:** `signals: drops_$cam:num:per(ecu_$cam):default(0)`,
  filled in from the instance's arguments as its text is; after filling in,
  a name must be a plain name. An option may use a parameter only if the
  group's name uses it too (`drops:per(ecu_$ecu)` is an error when the pack
  is read), so a signal several instances feed has one set of options.
  **Save to pack** writes the parameterised form back (`drops_$cam`), not
  the instance's name.

Settled with them (same day): conflicting options between queries are shown
as a warning and the first picked query's win; signals merge across packs,
no namespaces.

## Design

**Core (`pack/expand.rs`).** `Pick::Query` gains its argument sets
(`Pick::Query { id, instances: Vec<Params> }`; none: one instance with the
shared values). Each instance expands to a part of its own; its effective
parameters are its own over the shared ones; an instance whose effective
arguments equal an earlier one's is dropped. Ancestor coverage (`covers`)
holds only between instances with the same effective arguments. `Part` gets
the pick's index in its role's list and the instance's index, so callers map
parts back without the query id (which instances share), and the instance's
arguments for display.

**Packs (`pack/parser.rs`, `signal.rs`).** A `signals:` entry's group and
option values may hold `$name` / `${name}` (checked as a name with
parameters when parsed; `SignalOptions` keeps the text). The parser refuses
an option that uses a parameter its group's name doesn't. Filling in happens
where the query's text is filled in; a filled-in group that isn't a name is
an expansion error naming the query and the value.

**GUI (`ui/src/library.rs`).**
- `Node` gets `instances` (argument sets; raw flag as the shared ones) and
  a colour slot per instance. `expand` maps parts back by pick and instance
  index, not by `expand_id`. Editing the node edits the query for all its
  instances.
- The node's chip shows the query; under it a chip per instance
  (`CAM1`), each with its colour, its fields and ✖; the shared fields stay
  in the selection's parameter list.
- **+ instance** on a parameterised pick; **Instances…** asks for values,
  one per line, and adds a set per value; values already there are refused
  with a message.
- `search_term`, line colours and the signals' pins use each instance's
  effective arguments; `Picks::save_signal_options` maps `drops_CAM1` back
  to the entry or group `drops_$cam` of that query.

**Presets (`preset.rs`).** A node's instances follow it, a line per
argument: `node include lit query cam.drops`, then `instance 1 cam = CAM1`,
`instance 2 cam = CAM2` (`instance 2 re raw = …`). A new keyword, not an
indented `param`: an older nerka refuses the preset instead of reading the
values as the shared ones. Existing presets read as before, so no
`bundle::STEPS` migration.

**CLI.** `-q 'cam.drops(cam=CAM1)' -q 'cam.drops(cam=CAM2)'`: picks of the
same query and role become one pick with two instances (several arguments
with `,`; a value with `,`, `)` or spaces quoted). `-p` stays the shared
value. The same for `-r` and `-x`.

**Docs.** `docs/user/packs.md` (instances, `signals:` with parameters),
`docs/user/signals.md` (signals per instance), the preset format in
`preset.rs`'s header.

## Open

- Lifecycle rules that use a library query (`{pack.query}`) take the shared
  parameter values. Instances of a lifecycle (one per ECU) are the same
  idea: `lifecycles.md` lists "add one per ECU" for later; it could reuse
  this (a lifecycle over a parameterised query, one per value).
