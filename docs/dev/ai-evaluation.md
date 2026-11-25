# The AI workflows evaluated

How well an agent finds the cause of a problem in a real log with nerka,
on its own with the CLI (workflow A) or on the user's window through MCP
(workflow B); what it tripped over, and what changed because of it.

## Method

- **Logs:** closed bug reports with public logs whose cause the
  maintainers named, of several kinds: Go goroutine dumps (alone, and at
  the end of a debug log), two trace logs of one server compared good
  against bad, a daemon's own text log, a binary DLT trace, and a logfmt
  server log with crash dumps between its lines.
- **Blind runs:** an agent gets the reporter's words and the log only and
  reads the log only through nerka (A: the CLI, `nerka help agent`
  first; B: `nerka mcp` on a window with the log open), about 25 calls.
  It reports a cause with its confidence, the evidence lines, a next
  step, and feedback on the tool.
- **The loop:** graded against the maintainers' cause; the feedback
  sorted into fixes, made, and the same logs run again; the rest kept as
  ideas.

## Results

- **Both workflows find causes.** Of 20 runs: 14 right, 4 mostly right,
  2 partly right, none wrong. A and B reach the same verdicts on the same
  log; B also leaves the user annotations, a filter on the evidence and
  the line shown, which A can't.
- **Past the first reply:** several runs named the mechanism more
  precisely than the maintainers' first answer (a file still being
  written while it was sent; the memory a default context size takes).
- **A log's ceiling:** where the last step is in the program's code (why
  so many connections were opened), no run gets there from the log; the
  agents said so and named what to check next.
- **The fixes paid:** the same logs run again took about a third fewer
  calls.

## What the agents tripped over, and what changed

| Finding | Change |
|---|---|
| Unknown arguments silently ignored; a page silently capped | an error naming the arguments a tool takes; `more` / `skip` on every page |
| Answers too big: the path on each line, null fields, long templates, kilobytes of hex a line | the file once per page, nulls left out, `width` (MCP) / `--width` (CLI) with `raw_chars` |
| Dumps not read as records (nobody found the option) | hints in the first answer (`open --json`, `summary`); several Go crashes in a log hinted as crashes; records that end (`end=/^$/`) |
| Entries spanning lines (bodies in a trace) | a hint to read them as records starting at a date |
| No time found: an uptime at the line start, logfmt's `time=`, a tool's output between timed lines | a hint with the profile that reads the uptime; `time=` / `ts=` read; a file whose first line is timed and a tenth of the rest |
| Record numbers mistaken for file lines | one numbering, the file's (`line`, `lines`, `match_line`; `records.md`) |
| A regex's `/` lost to JSON escaping | the error advises `[/]` |
| A signal's regex reads the whole line, its value carries on | the guide: anchor on the payload, count with `@x.event` |
| One-word messages merged into one pattern | the setting `lone-word` (`patterns.md`) |
| `compare` read as broken on a difference inside a line | its description says it compares templates and counts |
| Git Bash turns `/…/` into a path | a warning, and `MSYS_NO_PATHCONV=1` in the guide |

## Patterns over stack traces

By line, the frames of a Go dump over-merge (frames of ten packages in
one template). Read as records, a Python traceback is one pattern, but
Go stacks that differ in an argument or an offset stay apart: a stack
signature is designed (`stack-dumps.md`).

## Ideas, not built

- Distinct values of a regex capture with their counts (asked by most
  runs on the DLT trace).
- A signal defined through MCP; a signal's value as an output field.
- A DLT file-transfer view (start, data and end messages checked against
  each other).
- logfmt keys as fields; a line starting `error:` as ERROR.
- Patterns over hex payloads.

## The tools on 1 GiB (release, Windows)

Bounded answers don't hurt big logs: bounds cap what an answer returns,
not what is searched (`count`, totals, `patterns` and `compare` run over
every line). Each MCP call end to end (a fresh `nerka mcp`, ~0.05 s of
it), on the benchmark files (`gen_log`, `gen_dlt`):

| Call | 1 GiB text (12.0 M lines) | 1 GiB DLT (12.5 M messages) |
|---|---|---|
| `window` / `summary` (levels counted) | 0.42 / 0.09 s | 0.06 / 0.14 s |
| `count 'level >= ERROR'` (2.0 M) | 0.09 s | 0.09 s |
| `filter`, a page of 50 (also at skip 100,000) | 0.09 s | 0.09 s |
| `filter 'payload ~ /timeout/'` | — | 0.28 s |
| `lines` around line 6,000,000 | 0.06 s | 0.06 s |
| `filter … before: 1s` (2 M matches: every line) | **13.1 s** | 0.23 s |
| `patterns`, all lines / in 2 M errors | 0.70 / 0.26 s | 1.20 / 0.25 s |
| `compare` 2 M warnings against 2 M errors | 0.38 s | 0.46 s |

The one slow call is a time window on text: the text profile reads each
line's time, where DLT has its own. A text log's times indexed once would
fix it, if agents use windows on big text logs.
