# Records: where they end, lines outside them, numbers, matches (design; not built)

Status: **proposed** (2026-11-25), for the owner's review. Re-evaluated
from the user's and the agents' side after the evaluation rounds
(`ai-evaluation.md`); the owner: "I liked the original version for one
use case … it might be my bias … spend more time on this point".

## What records are used for

`start=/RE/` serves two uses that want different things:

| | Records as **entries** | Records as **sessions** |
|---|---|---|
| Example | a stack trace, a goroutine of a dump, a request block | a boot of a UART log, a test run |
| Before the first match | unrelated log lines (525 debug lines before a dump) | **the tail of a session that began before the capture** (UART started mid-boot) |
| Wanted there | each line a row | **one record**, a session like the others |
| Then | filter for a stack, read it | filter whole sessions on a startup line, then search inside them |
| Where a record ends | often before the next start (a dump inside a server's log) | at the next start |

The owner's use (sessions): "match any boot on some specific startup line;
since UART started in the middle there was no obvious start of the first
record, but after all it was a single record I could filter on and then
search in".

## What is wrong now

1. **The head rule serves one use.** Until 2026-11-25 the lines before
   the first match were one record (sessions: right). The change kept in
   the evaluation (`ai-evaluation.md`, "lines before … stay rows") made
   them rows (entries: right) and broke the session path: a partial first
   boot is scattered into rows.
2. **A record never ends before the next start.** In a log with a dump
   inside it, everything after the dump is swallowed by its last record.
   A server's log with crash dumps, read as goroutine records: 3,394 lines become 398 rows,
   one record 38,177 characters holding the next session's server lines;
   a record's level and time are its first line's (`goroutine …`: none),
   so `level >= ERROR` misses the four "llama runner terminated" lines,
   silently. For an agent this is a wrong count that looks right.
3. **Two numbering systems.** In records mode the gutter, Ctrl+G and
   the CLI / MCP `line` count records; since the head change they equal
   file lines up to the first record, then drift (record 580 is
   file line 1352): wrong in a way that looks right. Agents juggle `line`
   and `first_line` (evaluation rounds 2–4).
4. **Why a record matched is hidden.** A filter keeps a record when any
   of its lines matches (right: a frame keeps its goroutine, a startup
   line its boot), but a folded record shows its first line only:
   `raw ~ /makeListDir/` shows a goroutine header and "+38 lines"; a boot
   that matched on one line of 5,000 shows nothing of it.
5. Counts say "lines" where they count records (`filter -c` 62 records
   against 79 lines).

## Proposal

### Where a record starts and ends

```
start=/RE/                      a record starts at each match and runs to the next (as now)
start=/RE/ end=/RE/             … and ends at the first line matching end (that line included)
end=/RE/                        a record ends at each match; the next line begins the next
start=/RE/ outside=rows         what to do with lines outside every record (below)
```

- `end=` makes a record stop early: what lies between an end and the next
  start is **outside** every record. A blank line is one case
  (`end=/^$/`: a goroutine ends at the blank line after it), a closing
  bracket another (`end=/^-+ end -+$/`), a Java trace's last line a third
  (`end=/^\s+\.\.\. \d+ more$/`).
- The end line is part of the record (the closing bracket and "… 12
  more" stay with their record; a blank one is harmless at its bottom).
- A start line before any end ends the record and begins the next.
- In a spec of several parts the regexes are written between slashes
  (`\/` or `[/]` for a slash inside), the parts separated by spaces; a
  spec of one part keeps today's forms (`start=RE` without slashes).

### Lines outside records

| `outside=` | Each run of lines outside every record |
|---|---|
| `one` | is one record (a partial session) |
| `rows` | is a row per line (the log around a dump) |

- Defaults: **`start=` alone: `one`** (the original rule: the owner's
  UART path is back); **with `end=`: `rows`** (lines between records are
  usually ordinary log lines: shown).
- The stack-dump hint (and so every agent) says `outside=rows`
  explicitly.
- Each row gets the field **`outside`** (true for a row outside every
  record), so `NOT outside` hides them, reversibly: counts, search and
  bookmarks still see them (owner, 2026-11-25: the field only, no
  `outside=drop`).
- `nerka open` / `summary` say what a split did: records, rows outside
  them, and for `one`: "the first record (lines 1–412) starts before the
  first match: a partial session".

### Numbers: file lines everywhere

- A row is numbered by its **first line in the file**: the gutter, Ctrl+G
  (the record holding that line), the CLI's text and JSON `line`, MCP's
  answers, bookmarks shown. A jump in the numbers (1352 → 1391) shows
  where lines are folded, as an editor's folding does.
- `first_line` goes; a record of several lines says how many
  (`"lines": 39` in JSON; `▸ +38 lines` in the view, as now). Lines
  inside a record are addressed by their file line (`lines FILE 1360
  1370` reads those lines whatever the split).
- Counts say what they counted: "62 records (2,904 lines)".

### Why it matched

- A folded record that a filter or search kept for a line other than its
  first shows that line under its header (dimmed, its file line in the
  gutter); clicking it unfolds there.
- JSON gives `"match_line"`: the file line of the first matching line, so
  an agent bookmarks the line that matters, not the header.

### Later, if a log needs it (owner, 2026-11-25: `cont=` later)

- `cont=/RE/`: only lines matching join the record above (Filebeat's
  multiline). It mirrors `start=` (`cont=/^\s/` is `start=/^\S/`, the
  Java hint today) and adds only a negation a character class can't say
  (no lookahead in nerka's regex): add it, or `start=!/RE/`, when a real
  log needs one.
- Sessions overlap lifecycles (boots with `reset uptime` and outcomes).
  What records give that lifecycles don't is "keep the whole boot where X
  happened"; a query over lifecycles ("those that contain X") would be
  the lifecycle side of it. Not now.

## Where

- **Core:** `input/records.rs`: `RecordSplit::Start { start, end,
  outside }` (and `End`), the split's walk (start, end, outside runs);
  the `outside` field beside a JSON record's keys; `LogSource` rows
  numbered by file line for display (`line_in_file`, built 2026-11-25, is
  the mapping); `agent.rs` line objects (`line` = file line, `lines`,
  `match_line`; `first_line` gone).
- **Window:** Records…: *starts at*, *ends at (optional)*, *lines
  outside: one record / rows / hidden*; the gutter and Ctrl+G by file
  line; the matching line under a folded record.
- **CLI / MCP:** `--records 'start=/…/ end=/…/ outside=rows'`,
  `set_records`; the hints; `help agent`.
- **Docs:** `formats.md` (records), `agents.md`, `queries.md` (`outside`).

## Compatibility

- A records choice is saved per file (`<config>/nerka/records`), in
  profiles (`records` clause) and in bundles. Specs with one part read as
  before. An older nerka given `start=/A/ end=/B/` would read
  `A/ end=/B` as one regex, **silently wrong**: the multi-part form needs
  a guard (a version marker in the saved line, or a bundle format step,
  `bundles.md`).
- `first_line` leaves the JSON (agents read `line`); the guide and the
  MCP tool descriptions say so; the evaluation's scripts change with it.

## Questions for the owner

1. ~~The head default~~ **Answered (owner, 2026-11-25): yes**, one record
   for `start=` alone, rows with `end=`.
2. ~~`drop`~~ **Answered (owner, 2026-11-25): the field only**: no
   `outside=drop`; `NOT outside` hides those lines, reversibly.
3. **Numbering by file line:** in 0.3.0, or after it?
4. **The matching line under a folded record:** the first match only, or
   every matching line (a boot with many)?
5. ~~`cont=`~~ **Answered (owner, 2026-11-25): later.** Added when a real log needs a negation `start=` can't say.
