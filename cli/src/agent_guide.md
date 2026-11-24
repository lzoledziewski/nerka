# Analysing logs with nerka

nerka reads text logs, JSON Lines, DLT and captures (pcap, pcapng, MF4) and
filters them with NFD queries. Several files given together are one view
(`--merge`: sorted by time). Every command below prints JSON with `--json`.

- **Exit status:** 0 lines found (or done), 1 nothing found, 2 an error (the
  reason on stderr, a query error with a caret under its place).
- **Lines** count from 1, in their own file. **Times** in JSON are UTC
  (RFC 3339).
- **Quote queries** in single quotes: `'level >= ERROR AND raw ~ /x/'`.
- **Results can be huge** (millions of lines): count first (`-c`), then read
  in pages (`-m N`, `--skip N`).

## 1. What the file is

    nerka open --json FILE         format, lines, fields (name, kind, costly),
                                   first and last time, lines per level, notes
    nerka help fields FILE         the fields a query can use on it
    nerka help nfd                 the query language, with examples

`notes` say what a reader skipped or dropped: read them, a gap there can
explain a gap in the log.

## 2. Counts first, then lines

    nerka filter -c 'level >= ERROR' FILE
    nerka filter --json -m 50 'level >= ERROR' FILE
    nerka filter --json -m 50 --skip 50 'level >= ERROR' FILE

A line is `{"file", "line", "time", "level", "raw"}`; `--fields apid,ctid`
adds `"fields"`, typed. When `-m` cut the result, the last line is
`{"more": N, "skip": K}`: `--skip K` for the next page.

    nerka search --json -i -m 50 'timeout|refused' FILE    a regex, like grep
    nerka search --json --in 'level >= WARN' -m 50 timeout FILE

## 3. What stands out

    nerka patterns --json --rare -n 30 FILE
    nerka patterns --json --in 'level >= WARN' --queries -n 30 FILE

Patterns are message templates (`connect to <*> failed`) with their count and
first line; `--queries` gives each an NFD query that finds its lines, `--lines 5`
its first lines, `--values` the values of its slots. Rare patterns are where
unusual things happen.

    nerka compare --json good.dlt bad.dlt
    nerka compare --json --a 'QUERY_A' --b 'QUERY_B' FILE

What differs between two sets of lines: patterns and values new in B, gone
from B, more or less often (scored). A good run against a bad one, before
against after.

## 4. Around a finding

    nerka filter --json --before 30s --after 5s 'raw ~ /watchdog reset/' FILE
    nerka filter --json -m 100 'before(10s, level >= FATAL) AND level >= WARN' FILE
    nerka lines --json FILE 1200 1260

`--before` / `--after` give the lines within that time of each match,
`"match": true` on the matches themselves. Context is by time; for lines
around a line number, use `lines`.

## 5. Over time: lifecycles, signals, checks

    nerka --lifecycle 'ecu1: lines ecu == ECU1; reset uptime' lifecycles --json FILE
    nerka --signal 'speed=/speed=(?<speed>\d+)/' filter -c '@speed > 100' FILE
    nerka --check 'storm: lines level >= ERROR; rate 25 per 1s' check --json -e 3 FILE
    nerka --check 'lost: lines /timeout/; missing /connected/ within 200ms' check --json -e 3 FILE

A lifecycle is one entity's run (an ECU's boot to its reset); a signal is a
value lines carry (`@speed`); a check tests a condition (a rate, a gap, a
missing answer) overall and per lifecycle, with evidence lines.

## 6. Leave your findings for the user

    nerka marks --add LINE --note 'what this line shows, and why it matters' FILE
    nerka marks --add LINE --name crash --note '…' FILE
    nerka marks --json FILE
    nerka marks --remove LINE FILE

Bookmarks are kept next to the file (`FILE.nerka-marks`). The user sees them
in nerka-gui as your annotations (🤖) until they accept them as their own;
a window that has the file open takes them in when it gets the focus back. Bookmark the few lines that explain the
problem (the first error, the cause, the change), each with a note that
stands on its own. `bookmark` in a query finds them: `filter 'bookmark'`.

## 7. Show it in a plot

    nerka-gui FILE --signal 'speed=/speed=(?<speed>\d+)/' --plot speed --screenshot plot.png

The window opens, plots the signal, saves itself as a PNG and closes: a
picture to look at, or to give the user.

## A way to start

1. `open --json`: the format, the time span, the levels, the notes.
2. `filter -c` by level; `patterns --json --rare --in 'level >= WARN'`.
3. Read around the first serious error: `filter --json --before 30s`.
4. `compare` a good part against the bad one (two files, or `--a` / `--b`).
5. Bookmark what explains it with `marks --add`, and tell the user which
   bookmarks to look at.
