# Settings bundles: export, import, upgrades

_Owner, 2026-11-07: an import / export of the analysis setup, so it can be
shared with a colleague or moved to another computer, and so that a later
nerka with a breaking change in a file format adapts the data instead of
failing on it._

## For users

What a bundle holds, its file format, importing and where things are:
[`docs/user/bundles.md`](../user/bundles.md). This file keeps the design: format
numbers and upgrades.

## Formats and upgrades

Every kind has a **format number**, today 1 for all of them (the files as
nerka 0.1.0 writes them). A breaking change to a kind's file format bumps
its number and adds a **migration step**: a function from that kind's text
in format *n* to format *n+1* (`bundle::STEPS` in core). An item in an
older format goes through the steps in order up to the current format, and
only then through the normal parser.

- An item in a **newer** format than this nerka knows is refused (`made by
  a newer nerka: check format 3, this nerka reads up to 2; update nerka to
  import it`); the rest of the bundle still imports.
- Steps work on text, so they never need the old parser; each comes with a
  test on a sample of the old format kept in the tests.
- **The config folder uses the same steps.** `<config>/nerka/format` lists
  each kind's format the folder's files are in (`pack 1` …; a folder
  without it is all format 1). When a newer nerka starts (the GUI, and the
  CLI), it first writes every item of the folder as it is into a backup
  bundle (`<config>/nerka/backups/before-upgrade-<UTC time>.nerka-bundle`, with
  their old format numbers, so it imports into any later nerka), then
  upgrades the files in place and writes the new numbers. A folder written
  by a *newer* nerka than the one running is left alone; nerka says so.

## Not yet

- Bundles of other things (prefs, histories), picked separately.
- Dropping a `.nerka-bundle` on the window to import it.
- A step that changes an item's name (steps keep it).
