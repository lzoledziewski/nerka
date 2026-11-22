# Text profiles (first slice built 2026-11-22)

A **profile** says how a text file is read: how its lines group into records,
and where a line's level and time are. It picks up
review 2026-11-09 R11 (levels in text are upper case only) and "Decisions
pending" 1 in `status.md`, and comes with **adjusting a file's time** (Time
per file, below). Discussed with the owner on 2026-11-11, 2026-11-14 and
2026-11-22; the open questions below are the owner's to settle.

**Built (2026-11-22, branch `claude/text-profiles`)**: profiles as spec
lines (`name`, `first`, `records`, `time auto|none|/REGEX/`, `zone`,
`numbers`, `signal`; `core/src/input/profile.rs`, the user guide's
`formats.md` → Profiles), chosen forced / by name / by first line /
default, the built-in default `time auto`; a text file's time from its
profile as the file's own (`LogSource::time`, `own_time`, `has_time`;
`input::timed::Timed`); an offset per file for any file (the time column
of a table moved too); text with a time in the mixed layout with DLT and
captures (`proto` `TEXT`, its line the payload) and merged by time; the
CLI's `--profile` and `--offset`, `nerka open` saying how each file's time
is read; the GUI's ⚙ Settings → Profiles… (the file's lines as text) and
per file in **Files…** (a forced profile, an offset, how its time was
read), both remembered in `records`; profiles in settings bundles (kind
`profile`, format 1). **Not built**: the level rule (marked forms, a regex
with a word map), JSON without a level key having none, protocol level
maps, a zone or number unit per file (only per profile), anchors, midnight
roll-over of times of day.

## What is there today

- **Plain text and text records** (`--records`): the level is the first
  whole upper-case word of the first 160 bytes that names a level (`ERROR`,
  `[WARN]`, `level=INFO`): a hand-written scan (`event::detect_level`), not a
  regex. `[error]`, `level=warn` and `Error:` have no level, and a level
  condition drops those lines without a word. Measured on the 1 GiB log with
  its levels in lower case: `level >= ERROR` finds 0 of 2,002,175.
- **Text has no time.** `LogSource::time` is `None` for text, so there is no
  time column, no Merge by time and no checks on times for a text file. A
  signal can already read one (`--signal 't:time=/^(?<t>\S+ \S+)/'`, read by
  `time::parse_time`: ISO dates, times of day, AM / PM, zones, epoch numbers),
  but it isn't the line's time.
- **How a text file is split** (lines, multi-line records, JSON) is chosen per
  tab (its menu → Records…) and remembered per file in `<config>/nerka/records`
  (`spec<TAB>path` per line); unlisted files are read automatically.
- **JSON** takes its level from a top-level key `level`, `severity`, `lvl` or
  `loglevel` (any case; the value in any case too, or a number: 30 info, 40
  warn, 50 error). **Without such a key it falls back to the text scan**, so
  `{"msg": "ERROR while writing"}` is level ERROR (seen, 2026-11-11).
- **Protocols** bring their own levels, mapped onto nerka's seven (Trace …
  Fatal) in code: DLT nearly one to one (verbose is Trace); syslog's eight
  severities into seven; SOME/IP has none, so one is made up (an error message
  or return code is Error, the rest Info). A protocol definition (`.ksy`) maps
  its values itself (`-nerka: level:`, `protocol-definitions.md`).

## The owner's direction (2026-11-11)

- **A profile per pattern.** The default is the default; otherwise a profile
  is chosen by the file's name or by its first line, so a few simple rules
  decide what kind of file it is and how it is read. It can be forced in the
  Files… dialog, and the forced choice is remembered in `records`.
- **JSON has no level unless a key says so.** Matching the key's value
  regardless of case is fine.
- **A protocol's levels may not match nerka's (or DLT's) definition**: the
  mapping is a choice, not a fact.
- **Built together with adjusting time per file**: a text file gets a level
  and a time, and the time can be in any form: UTC, seconds, a date from 1970
  (a clock that was never set).

## Proposal

**What a profile holds**

| Part | Choices |
|---|---|
| Records | lines, multi-line (`start=`, `sep=`, `blank`), JSON: today's `RecordSplit` |
| Level | none; upper case anywhere (today's scan); **marked forms** (below); a regex with a `level` group and a word map (`E` → Error, `crit` → Fatal) |
| Time | none; a regex with a `time` group, read by `parse_time` (a format string where that can't read it) |

**No fields of its own (owner, 2026-11-11).** Other values of a line
(`thread`, `module`) stay with signals (`--signal 'thread=/\[(?<thread>\w+)\]/'`,
`@thread`): they already take a value out of each line, only when a query
uses it. A profile pulling fields out of each line would do the same at the
same cost (about 1 s per GiB through a capture, against 33–70 ms for a
match). Later a profile could name signals to switch on for its files.

**Marked forms** (the level rule proposed as the default): upper case anywhere
in the first 160 bytes as today, plus lower case and capitalised words where
the place says they are a level: `[error]`, `<warn>`, `level=info` (also
`lvl=`, `severity=`), and the first word after a leading time (`10:00:01 Error:
…`). A word in the message (`connection error, retrying`) stays a word.

**Choosing a profile**: the first that applies wins.

1. Forced: Files… dialog or the tab's Records…, remembered per file in
   `records` as one more kind of spec (`profile=NAME`). Older lines read as
   before, so the format extends without breaking.
2. By name: a file-name pattern (`*.syslog`, `app-*.log`).
3. By first line: a regex on the first line (or first few lines): a JSON `{`,
   a header, `<165>1 2025-…`.
4. Default.

In a view of several files each file has its own profile, as each has its
own records today.

**JSON**: no level key, no level (`input/records.rs`, `supplied_level`: "no
level" instead of falling back to the scan). A profile can name another key
(`sev`) and a value map.

**Protocols**: today's maps stay as the defaults; a profile (or the protocol's
settings) may replace one, e.g. no level for SOME/IP, or syslog's `crit` as
Error. `.ksy` definitions keep their own `level:` map.

## Time per file

**The forms a time takes, and what `time::parse_time` makes of them today**

| Form | Example | Today |
|---|---|---|
| Date-time with a zone | `2025-10-17T10:00:01Z`, `…+02:00` | read |
| Date-time without a zone | `2025-10-17 10:00:01` | UTC, or local time in the GUI: one switch for the whole process (R12) |
| Time of day | `10:00:01.250` | day 0 (1970-01-01); a signal adds a day at each midnight |
| A number | `1760695201`, `1760695201250` | seconds since 1970, or ms / µs / ns, guessed by its size |
| Seconds since boot | `[   12.345678]` (kernel) | read as a number: 12 s after 1970-01-01 |
| A clock never set | `1970-01-01 00:00:12` | read as written; under 1000 days after 1970 it shows without a date |

So the last two can be read but don't land on the same timeline as the other
files, and Merge by time puts them all first.

**Adjusting a file's time** (proposal):

- **Zone**: the zone of the times written without one, per file (UTC, this
  computer's, a fixed offset), not one switch for the process.
- **Kind**: a number is seconds since 1970 (or ms, µs, ns) or seconds since
  boot, as the file says, not guessed by its size.
- **Offset**: move the file's times by a fixed amount, given as a value
  (`+3h`, `-0.250s`) or by an **anchor**: pick a line and say when it was, or
  that it is the same moment as a line of another file; nerka works out the
  offset. That puts a device whose clock started at 1970, or a log with
  uptime only, on the same timeline as the rest.
- Every use of the time sees the adjusted one: the time column, Merge by
  time, plots, checks, `@time` and signals' times.
- **Not only text**: a DLT logger's or a capture's clock can be off too, so
  the offset (and the zone, where a format has none) applies to any file; the
  level and time rules are for text.
- The offset belongs to a recording, not to a kind of file: it is remembered
  per file (like the forced profile in `records`), not in a profile; a
  profile gives the zone and kind defaults.
- `sources-and-signals.md`'s time model (`Timestamp { ns, clock }`, one offset
  per clock domain) is where this leads; an offset per file is its first step
  (DLT's uptime per ECU is another clock).
- Cost: an addition per time read (not measured).

**Saved files**: profiles are a new saved format (where and how is open,
below), so they get a version, a step in `bundle::STEPS` when it changes, and
a place in settings bundles (`bundles.md`).

## Cost (measured 2026-11-11, 1 GiB `big.log`, 12 M lines, Windows release)

| How the level is found | `level >= ERROR` |
|---|---|
| Today's scan, levels at the line's start | 33 ms |
| Any case anywhere (a trial build), the same lines | 35 ms (+0–7 % over the level queries) |
| Any case anywhere, lines with no level at all (the whole 160 bytes read) | 78 ms (today's scan: 33 ms) |
| A regex taking the level out of every line (a signal, 3 variants) | 0.96–1.08 s |
| A plain regex match over the file, `raw ~ /\[error\]/` (2 M of 12 M lines match) | 39–43 ms |

A level query as one regex over the file costs by **how many lines match**,
not by how many level words there are (the words are one alternation, read
in one pass; checked 2026-11-11, search times, 5 runs):

| Query | Lines matching | Search |
|---|---|---|
| `raw ~ /\[(error\|err\|fatal\|crit\|critical) *\]/` | 2 M | 47 ms |
| `raw ~ /(?:\[\|<\|level=\|lvl=\|severity=)(?i:error\|…\|critical)\b/` | 2 M | 69 ms |
| The same with all 11 level words, any case (`level >= TRACE`) | 12 M (all) | 245–285 ms |
| `raw ~ /\]/` (one character) | 12 M (all) | 217 ms |
| Built-in `level >= INFO` / `level >= TRACE` (today's scan) | 10 M / 12 M | 49 / 53 ms |
| Any of the regexes above on lines with no level | 0 | 22–29 ms |

So a regex costs about 20 ns per matching line on top of the scan (the
engine finds each match's line and starts again after it), and a broad level
query (`level >= INFO`) is 4–5× today's. A file uses one profile, chosen once
when it opens, so only its rule runs per line. Two ways to keep a regex rule
near today's speed:

- **As a match.** `level >= ERROR` under the rule `\[(?<level>\w+)\]` becomes
  "the line matches `\[(error|fatal|…)\]`" (the words of the map at or above
  ERROR), which runs over the whole buffer like `raw ~`. Fast when few lines
  match (40–70 ms), 4–5× today's when most do. Level colours are worked out
  only for the rows on screen.
- **Once, kept.** One pass per file (about 1 s per GiB, on first use) and a
  byte per line (12 MB for this log); every level query then costs what it
  does today, however many lines match.
- The built-in rules (today's scan, marked forms) stay hand-written scans:
  33–78 ms whatever matches.

**Decided (owner, 2026-11-11): no level in the index.** A level query on a
text file with a regex rule is a niche search, so it isn't worth changing the
index: the level stays worked out per query, as today (a regex rule as a
match), and a broad query under a regex rule may cost 4–5× today's.

Marked forms were not measured; they read the same 160 bytes as the trial and
check fewer words.

## Settled (owner, 2026-11-14)

- **Written as spec lines**, one per profile (like `--records` and
  `--lifecycle`, so the CLI takes the same text), kept in a file under
  `<config>/nerka/profiles`, edited in a GUI dialog, part of settings
  bundles.
- **A profile also switches signals on** for its files (a text log's `@time`
  from a regex, its other values), with the options of signals in packs
  (`sources-and-signals.md`, "Decisions taken with the owner").
- **Views of several files**: a text log can be merged by time with DLTs and
  captures (once a profile gives it a time); the tab's **Files…** window shows
  and overrides each file's profile and its time (zone, kind, offset,
  anchor). A **saved view** is a workspace (the files with their profiles and
  times, reopened in one step): low priority.
- **Order**: built after the signal options of pack queries, time per file
  first, then profiles, then views of several files (`status.md`, "Next
  steps").

## Settled (owner, 2026-11-22)

- **A built-in default reads a time**: a text file no profile of the
  user's matches gets the date-time its lines start with (`time auto`); a
  profile named `default` replaces the built-in one.
- **Best effort when a time can't be read**: the built-in rule is used for
  a file only if at least half of its first 200 non-empty lines start with
  a time (a profile's own regex: if any does), else the file has no time
  and Files… / `nerka open` say so; a line without a time of its own has
  the time of the line before it (lines before the first time: the first),
  so it stays with its neighbours when sorted or merged and has `@time`;
  the time column of a merged view shows only a line's own time; a time of
  day without a date stays on 1970-01-01 (no midnight roll-over) until the
  file gets an offset.
- **CLI**: one flag, `--profile NAME` forces a profile on every text file,
  `--profile 'NAME: CLAUSES'` defines one for the run (chosen by its rules
  before the saved ones).
- **A file's offset is remembered per file** (in `records`, like a forced
  profile), one per file in a view of several.
- **First-line rule**: the regex is tried on the file's first non-empty
  line; several profiles matching: the first in list order wins (name
  rules before first-line rules).

## Open questions

1. **Protocol level maps**: in profiles, or in the protocols' own settings (⚙
   → Captures).
2. **R11 before profiles**: make marked forms the default now, or with the
   profiles' level rule.
3. **JSON's fallback**: drop it now (a small change of behaviour), or with
   the profiles' level rule.
4. **Anchors in the GUI**: "this line was at …", "this line is the same
   moment as that one", and how an anchored file shows that it was moved.
5. **Seconds since boot**: shown as uptime until anchored, or placed on the
   timeline only with an anchor; a file with several boots (lifecycles).
6. **Zone per file and the GUI's Local time / UTC switch** (R12): a
   profile's `zone` replaces the switch for its files (built); whether a
   file can have its own zone besides its profile's. Seen 2026-11-22: in
   the GUI's local time a text log written in UTC lands hours away from
   the DLT it was merged with unless its profile says `zone utc`.

Prior art: lnav's log formats (a regex with named groups, a level map and a
time format per format, chosen by trying each on the file's lines).
