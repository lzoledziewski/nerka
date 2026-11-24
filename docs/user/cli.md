---
title: "The command line"
description: "The nerka command line: filter, search, open, lines, check, lifecycles, patterns, compare, anonymize, export and import, with every option for DLT, pcap, MF4, JSON and text logs."
---

# The command line

```
nerka [OPTIONS] <COMMAND>
```

`nerka help <command>` (or `nerka <command> --help`) lists every option.
Options for opening files (below) go before or after the command.

## Commands

| Command | Does |
|---|---|
| `open FILE…` | index the file(s) and print lines, size, throughput, fields, protocols and what was skipped |
| `filter QUERY FILE…` | print the lines an [NFD query](queries.md) keeps |
| `search PATTERN FILE…` | print the lines a regex matches, like `grep -n` |
| `lines FILE FROM TO` | print a range of lines (1-based, inclusive) |
| `marks FILE` | list the bookmarks saved for a file |
| `expand` | turn [Library](library.md) picks into plain NFD |
| `packs` | list the loaded packs and their queries |
| `check FILE…` | run the [checks](checks.md) (`--check`): issues and facts |
| `lifecycles FILE…` | print the [lifecycles](lifecycles.md) (`--lifecycle`) and their findings |
| `patterns FILE…` | group lines into message patterns (experimental), each with its count and a query |
| `compare A B` | what differs between two files, or two filters' lines (experimental): patterns and values new, gone, more or less often |
| `anonymize -o OUT FILE…` | the lines with every word made up, to share a trace: same structure, same patterns |
| `export` | write your setup as a [settings bundle](bundles.md) |
| `import FILE` | read a settings bundle into your setup |

```
nerka filter 'level >= ERROR AND raw ~ /SSL|CERT/i' app.log
nerka filter 'apid == NAV AND level >= WARN' trace.dlt
nerka search -i 'timeout|refused' app.log
nerka search --in 'level >= WARN' -c timeout app.log     # only among a filter's lines
nerka open trace.pcap
nerka lines app.log 100 120
```

## Output

Lines print with their number (`1234:…`); `-N` leaves it out. With several
files, each line prints as `FILE:LINE:` with the file's own line number.
`-c` prints only the count. `--stats` prints timing to stderr.

Warnings go to stderr: a query value no line can have (`proto == FOO` in a
capture without it, a `file` no file is), what a reader skipped, a check
whose lines' times go backward.

## `filter` and `search`

| Option | |
|---|---|
| `-c, --count` | only the number of lines (with pack picks, the hits per query too) |
| `-N, --no-line-numbers` | no line numbers |
| `--stats` | timing on stderr |
| `search -i`, `-F`, `-v` | ignore case, a literal pattern, the lines that don't match |
| `search --in FILTER` | search only among the lines an NFD filter keeps |

Instead of a pattern, `search` and `expand` take **pack picks**, and
`search` then prints which query matched each line:

| Option | |
|---|---|
| `-q, --query ID` | include a query (`pack.query`) |
| `-l, --label LABEL` | include every query with this label (or one below it) |
| `-r, --require ID`, `-R, --require-label LABEL` | lines must match it too (AND) |
| `-x, --exclude ID`, `-X, --exclude-label LABEL` | leave out what it matches (AND NOT) |
| `-p, --param NAME=VALUE` | a parameter's value, escaped where it's used |
| `--raw-param NAME=VALUE` | a parameter's value as regex syntax inside regexes |
| `expand --parts` | also each picked query's part of the expansion |

```
nerka --pack examples/packs/ expand -l ota -p cam=CAM1 --parts
nerka --pack examples/packs/ search --in 'level >= WARN' -l ota -p cam=CAM1 app.log
```

## `check` and `lifecycles`

| Option | |
|---|---|
| `-n, --name NAME` | only this check / lifecycle |
| `--json` | JSON output |
| `--strict` | exit status 1 when an issue (or a finding) shows up |
| `check -e, --evidence N` | print up to N places per result, with their lines |
| `lifecycles --count QUERY` | count a query's lines per lifecycle |

## `patterns` (experimental)

`patterns` groups lines by the message they print, whatever values they
carry, and keeps the values. `connection established peer=10.0.3.7` and
`… peer=10.0.9.12` are one pattern, `connection established peer=<*>`;
`gear=D`, `gear=P` and `gear=N` are one, `gear={D|P|N}`: a part with a few
values that repeat is a **state**, shown with its values (most lines
first), while a part with many (users, addresses, numbers) shows as `<*>`.

How: values holding a digit (numbers, hex, times, addresses) are masked
first; messages of the same length, first word and fields (DLT `apid`,
`ctid`, …) that are alike enough join a template, the words where they
differ becoming slots; a message differing from a template in two words or
more starts a template of its own. Words most messages share at the same
place from their start or end whatever their length, a header or footer
such as `LOG:` … `Elapsed[us]: 412; #3`, don't count as alike: two short
messages between them are two patterns (a field's values with only a few
lines here and there are sampled across the whole file for this). Then templates are aligned atom by atom (words and
single characters), so a varying part can also sit inside a word
(`gear=D`), come first (`{RUNNING|IDLE} state entered`) or have different
lengths (`thermal throttling {engaged at <*> C|released}`, `job done
{∅|fast}`, `∅`: nothing there). Two templates join when they share enough
words and differ, at each place, by one word at most on one side, and in
one new place at most. Two slots made at once would always change together:
`warning the data was read` and `status the data is read` stay two
patterns. Once `warning the data is read` turns up, the two places are seen
to vary on their own, and all three join as `{warning|status} the data
{was|is} read`. Last, a pattern whose lines another's query takes too (a
message split by the shape of a value, `proxy.lab.example.edu:<*> open
through proxy …` beside `<*> open through proxy …`) joins that one,
however few words they share.

A DLT message's `payload` is grouped (other files: the line), and fields
such as `apid` that tell the patterns apart print with them. The file
isn't one of them: files read together are one set of lines. Each pattern
prints its count, its share, its first line and the template, most lines
first.

| Option | |
|---|---|
| `--in FILTER` | only the lines an NFD filter keeps |
| `--rare` | the rarest patterns first: the odd message out |
| `--values` | also each slot's values: lines, first and last line |
| `--queries` | also each pattern's NFD query, to `filter` its lines |
| `-l, --lines N` | also each pattern's first N lines with their numbers (`0`: all) |
| `-n, --top N` | print at most N patterns (default 30; `0`: all) |
| `--field NAME` | group this field's text (`raw`: the line) |
| `--pattern-config FILE` | settings and rules (below; default: the saved ones, `-`: none) |
| `--set KEY=VALUE` | a setting for this run, e.g. `similarity=0.7` (repeatable) |
| `--rule RULE` | a rule for this run, e.g. `'apid == NAV: keep Elapsed'` (repeatable) |
| `--similarity S` | the setting `similarity` (0–1, default 0.5): how alike a message must be to join a template |
| `--no-merge` | the setting `merge off`: don't align templates (only messages of the same length join) |
| `--stats` | timing on stderr, and what each stage did: templates, and those kept apart and why |

```
nerka patterns app.log
nerka patterns --values --in 'apid == NAV' trace.dlt
nerka patterns --rare --in 'level >= WARN' --queries trace.dlt
nerka patterns -n 5 --lines 3 app.log
nerka patterns --rule 'apid == PWR: apart voltage= undervoltage' --set new-slots=2 trace.dlt
```

Non-verbose DLT messages without a [FIBEX](formats.md) have no text to
group, only their bytes. Lines that hardly repeat (a binary file read as
text) don't each become a pattern: past a few thousand, the rest of a
length go into one, marked *not sorted*. Messages longer than 64 words are
grouped by their first 63, the rest shown as `…`.

### The pattern configuration

How lines group can be tuned without a new nerka: one file,
`%APPDATA%\nerka\patterns` (Linux `~/.config/nerka/patterns`), read by
`patterns` and `compare`, holds settings and rules, a line each, `#` to the
end of a line a comment. A mistake (an unknown setting, a bad regex) stops
with its line number. It is part of [settings bundles](bundles.md), so a
tester's configuration can be sent with a bug report. `--set` and `--rule`
come after the file's lines, `--similarity` and `--no-merge` last.

```
# Settings (these are the defaults)
similarity 0.5            # stage 1: share of words alike to join a template
new-wildcards 1           # stage 1: words a message may turn into wildcards at once
new-slots 1               # stage 2: slots one merge may make
side 4                    # stage 2: tokens on the longer side of a slot
state-values 16           # a slot with more values (2–16) is <*>, not a state
header sample 32 min 8 share 0.5 places 3   # header/footer detection
merge on                  # stage 2 at all

# Rules: SCOPE: ACTION
apid == NAV: keep Elapsed
ecu == ECU1 AND apid == GPS*: max-slots 2
*: value /[a-z][\w-]*(\.[a-z][\w-]*)+/
apid == PWR: apart voltage= undervoltage
ctid == TCP: no-merge
*: header INFO: WARN: ERROR:
*: ignore /heartbeat/
ctid == TCP: set similarity 0.7
```

The header settings: lines of a context held back per chunk (`sample`),
the fewest to detect a header or footer on (`min`), the share of them a
word must be on, more than (`share`), and how many tokens from the start
and the end it may be at (`places`).

A rule's **scope** is the fields lines are grouped by: for DLT `ecu`,
`apid`, `ctid`, `type`, `mode` (the short text fields), for a text file
its profile's text fields; `FIELD == VALUE`, `*` in a value for any text,
`"…"` for a value with spaces, joined by `AND`; `*` alone is every line. A
field a file doesn't have is empty there.

| Action | Effect |
|---|---|
| `keep WORD…` | the word is never in a slot: a message differing there is a pattern of its own (`keep undervoltage`: `undervoltage <*> mV` apart from `voltage=<*> mV`) |
| `value /REGEX/` | text a token holds that the regex finds is a value (`<*>`), as a digit makes one: host names |
| `literal /REGEX/` | text the regex finds is never a value, though it holds a digit (`ECU1`, `v2`) |
| `max-slots N` | a pattern has N slots at most; a merge making more is refused |
| `apart WORD WORD…` | words (or parts like `voltage=`) never in one slot together |
| `header WORD…` | header or footer tokens, passed over whatever the detection says |
| `no-merge` | templates aren't aligned (stage 1 only) |
| `ignore`, `ignore /REGEX/` | the lines (whose message the regex finds) are left out, of `compare` too |
| `set SETTING` | a setting for these lines only: `set similarity 0.7` |

Rules are read top to bottom. Of those setting one thing (`max-slots`,
`no-merge`, each setting of `set`) the first that applies counts; the
others add up (two `keep` rules keep both words).

## `compare` (experimental)

`compare` groups two sets of lines into patterns together, so a pattern is
the same on both sides, then lists what differs, in three parts:

- **New in B**: patterns, and values of a pattern's slots, that only B has;
- **Gone from B**: those only A has;
- **Changed**: those in both, more or less often (a pattern against all of
  its side's lines, a value against its pattern's), surest first.

So a value stands out even inside a pattern it was merged into:
`undervoltage` new in B, while `{voltage=|undervoltage} <*> mV` holds
steady. The score is a log-likelihood ratio (G²): it weighs the evidence,
so 1 line against none scores low and 500 against none high; `--min-score`
applies to changes only, new and gone ones are always listed. Each line
prints the score, the lines in A and B, the change (`new`, `gone`, `×2.08`:
how many times as often, by share), and the first line on the side it's on
or grew on (`B:6412`).

The sets are two files, or one file's lines of two filters, e.g. two
[lifecycles](lifecycles.md):

```
nerka compare good.dlt bad.dlt
nerka compare --in 'apid == NAV' good.dlt bad.dlt
nerka --lifecycle 'ecu1: lines ecu == ECU1; reset uptime; planned payload ~ /shutdown/' compare --a '@ecu1 == 1' --b '@ecu1 == 2' trace.dlt
```

| Option | |
|---|---|
| `--a FILTER`, `--b FILTER` | set A's, set B's lines: only those an NFD filter keeps (one file: both needed) |
| `--in FILTER` | both sets: only the lines an NFD filter keeps |
| `--min-score S` | only changes at least this sure (default 10.8, p = 0.001; 3.84 is p = 0.05) |
| `-n, --top N` | print at most N differences (default 30; `0`: all) |
| `--queries` | also each pattern's NFD query |
| `--field NAME`, `--pattern-config FILE`, `--set`, `--rule`, `--similarity S`, `--no-merge` | as for `patterns` |
| `--stats` | timing on stderr |

## `anonymize`

`anonymize` writes the files' lines with every word replaced by a
made-up one, so a trace can be shared (to tune patterns, with a bug
report) without what it says. A word becomes letters of the same length
and case, the same everywhere and never another word's, so messages keep
their structure and the same [patterns](#patterns-experimental) with the
same counts; a run holding a digit keeps its digits as digits (made up per
run); spaces and punctuation stay. A DLT file stays DLT: times, levels and
the ECU, application and context IDs are kept (`--ids`: made up too), the
payload is written as one string. Other files become text.

The words are keyed at random each time, so nobody can map them back;
`--seed` makes the same words every time (two files anonymized alike).

| Option | |
|---|---|
| `-o, --output OUT` | the file to write (not one of those read) |
| `--in FILTER` | only the lines an NFD filter keeps |
| `--keep WORD` | a word left as it is (repeatable, any case): `LOG:`, `Elapsed` |
| `--seed TEXT` | the same seed, the same words |
| `--ids` | make up DLT IDs too |

```
nerka anonymize -o shared.dlt trace.dlt
nerka anonymize -o shared.log --keep ERROR --keep WARN --in 'level >= WARN' app.log
```

## `export` and `import`

| Option | |
|---|---|
| `export -o, --output FILE` | the bundle file (default: stdout) |
| `export --only KIND[:NAME]` | only these items (`packs`, `check:storm`; repeatable) |
| `import --dry-run` | only print what would happen |
| `import --conflicts both\|theirs\|mine` | an item that differs from yours: keep both (default), take theirs, keep yours |

## Options for opening files

These apply to every command:

| Option | |
|---|---|
| `--merge` | several files: their lines sorted by time (files that know their times: DLT, captures, text with a time; ties in the order given); else one after another |
| `--records SPEC` | read text as records: `start=/REGEX/`, `blank`, `sep=/REGEX/`, `json`, `lines` or `auto` ([Files and formats](formats.md)) |
| `--profile NAME` | read every text file with this [profile](formats.md#profiles) (a saved one, or `default`) |
| `--profile 'NAME: CLAUSES'` | define a profile for the run, chosen by its rules before the saved ones (repeatable) |
| `--offset FILE=OFFSET` | move a file's times (`trace.dlt=+3h`, `app.log=-0.250s`; the file as given or its name; repeatable) |
| `--dlt-port PORT`, `--someip-port PORT`, `--syslog-port PORT` | UDP ports of these protocols in captures (repeatable; defaults 3490, 30490, 514) |
| `--protocol FILE` | a [protocol definition](protocol-definitions.md) (`.ksy`) |
| `--protocol-port NAME=PORT` | a protocol's UDP port (short for `--route NAME:PORT`) |
| `--route NAME:RULE` | which datagrams are a protocol's: a port or range, or conditions (`DLT:port=3490,src=10.0.0.0/8`; keys `port`, `sport`, `src`, `dst`, `vlan`) |
| `--disable-protocol NAME` | don't read this protocol in captures |
| `--fibex FILE` | FIBEX file for DLT non-verbose messages |
| `--unpack-dir DIR` | where compressed MF4 data is unpacked (default: the per-user cache) |
| `--local-time` | show and read times in this computer's zone (default: UTC) |
| `--mf4-utc` | read an MF4 start time marked as local time as UTC |
| `--pack DIR\|FILE` | load query packs besides your own |
| `--signal SPEC` | define a [signal](signals.md) |
| `--lifecycle SPEC` | define a [lifecycle](lifecycles.md) |
| `--check SPEC` | define a [check](checks.md) |

## Exit status

- **0:** success, also when nothing matched.
- **1:** an error (printed as `nerka: …` on stderr), including `check
  --strict` finding an issue, `lifecycles --strict` a finding, and `import`
  meeting an invalid item (the others are still imported).
- **2:** a usage error (an unknown option, a missing argument).
