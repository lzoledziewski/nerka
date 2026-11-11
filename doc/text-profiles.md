# Text profiles (design, not built)

A **profile** says how a text file is read: how its lines group into records,
where a line's level and time are, and which other fields it has. It picks up
review 2026-11-09 R11 (levels in text are upper case only) and "Decisions
pending" 2 in `status.md`. Discussed with the owner on 2026-11-11; nothing is
built yet, and the open questions below are the owner's to settle.

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

## Proposal

**What a profile holds**

| Part | Choices |
|---|---|
| Records | lines, multi-line (`start=`, `sep=`, `blank`), JSON: today's `RecordSplit` |
| Level | none; upper case anywhere (today's scan); **marked forms** (below); a regex with a `level` group and a word map (`E` → Error, `crit` → Fatal) |
| Time | none; a regex with a `time` group, read by `parse_time` (a format string where that can't read it) |
| Fields | other named groups of the line regex become fields (`thread`, `module`), as JSON keys do |

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
| A plain regex match over the file, `raw ~ /\[error\]/` | 43 ms |

The signal's time includes its own machinery, so a level regex done directly
lands between 43 ms and 1 s. Two ways keep a regex rule near today's speed:

- **As a match.** `level >= ERROR` under the rule `\[(?<level>\w+)\]` becomes
  "the line matches `\[(error|fatal|…)\]`" (the words of the map at or above
  ERROR), which runs over the whole buffer like `raw ~`. Level colours are
  worked out only for the rows on screen.
- **Once, kept.** One pass per file (about 1 s per GiB, on first use) and a
  byte per line (12 MB for this log); queries then cost what they do today.

Marked forms were not measured; they read the same 160 bytes as the trial and
check fewer words.

## Open questions

1. **How a profile is written**: a one-line spec like `--records` and
   `--lifecycle` (CLI-friendly), or a YAML file like a `.ksy`.
2. **Where profiles live and are edited**: one file in `<config>/nerka/`, a
   folder of files, or inside packs; the GUI's place for them.
3. **First-line rule**: how many lines it reads; several profiles matching
   (the first in list order?).
4. **Protocol level maps**: in profiles, or in the protocols' own settings (⚙
   → Captures).
5. **R11 before profiles**: make marked forms the default now, or wait for
   profiles.
6. **JSON's fallback**: drop it now (a small change of behaviour), or with
   profiles.
7. **CLI**: `--profile NAME` and an inline spec.

Prior art: lnav's log formats (a regex with named groups, a level map and a
time format per format, chosen by trying each on the file's lines).
