---
title: "Queries (NFD)"
description: "NFD, nerka's filter language: fields, operators, regexes, log levels, signals and bookmarks, and how the search boxes read plain words."
---

# Queries (NFD)

**NFD** is nerka's filter language. The same query works in the GUI's boxes,
in `nerka filter`, in query packs, signals, lifecycles and checks.

```
level >= ERROR AND raw ~ /SSL|CERT/i AND NOT raw ~ /healthcheck/
apid == NAV AND level >= WARN AND payload ~ "timeout"
@speed > 100 OR bookmark
```

## Conditions

| Write | Holds when |
|---|---|
| `FIELD OP VALUE` | the line's field compares so: `level >= WARN`, `apid == NAV`, `mcnt < 10` |
| `/regex/`, `/regex/i` | the whole line (`raw`) matches the regex (`i`: ignore case) |
| `"text"` | the whole line contains the text |
| `@signal OP VALUE` | a [signal](signals.md)'s value compares so |
| `bookmark` | the line has a bookmark |
| `A AND B`, `A OR B`, `NOT A`, `( … )` | logic; also `&&`, `\|\|`, `!` |

**Operators:** `==`, `!=`, `>=`, `<=`, `>`, `<`, and `~` / `!~` (matches /
doesn't match a regex, or contains / doesn't contain a quoted text).

**Values:** a word (`NAV`, `42`, `0x2a`), a quoted string (`"two words"`,
with `\"` and `\\`) or a regex (`/re/`, `/re/i`). Keywords and field names
ignore case; values don't (except `proto`).

**A line without the field** satisfies only `!=` and `!~`.

## Fields

| Source | Fields |
|---|---|
| Every line | `raw` (the line as shown), `level` (compares by severity: `TRACE < DEBUG < INFO < NOTICE < WARN < ERROR < FATAL`), `bookmark` |
| DLT | `ecu apid ctid type mcnt tmsp seid mid mode payload` |
| SOME/IP | `service method client session iface type rc len payload` |
| syslog | `facility host app pid msgid stamp payload` |
| A protocol definition | its own ([Protocol definitions](protocol-definitions.md)) |
| Captures | `proto` (the protocol's name; `==` ignores case), and where a datagram came from: `net.src`, `net.dst` (addresses), `net.sport`, `net.port` (ports), `net.vlan` |
| JSON records | their keys (`ctx.user`, `latency`) |
| Several files | `file` (below) |

**Kinds:** text fields take `==`, `!=`, `~`, `!~`; numbers take every
comparison, and `~` / `!~` on their decimal text; a number with a fraction
(`temp > 20.5`) comes from JSON. A field like DLT's `payload` decodes the
message, so nerka checks it after the cheap conditions.

**Names that clash:** nerka's own fields can also be written `nfd.raw`,
`nfd.level`, `nfd.bookmark`; a JSON key named like one of them is
`json.raw`. In a view of several protocols, a field two protocols give
different kinds is written with the protocol's name: `dlt.type`,
`sens.type` (a plain `type` says which to write).

**Levels in text** are recognised from a whole upper-case word in a line's
first 160 bytes: `ERROR`, `[WARN]`, `level=INFO`, also `WARNING`, `ERR`,
`CRIT`. DLT, syslog, SOME/IP and protocol definitions bring their own
levels; JSON takes a `level`, `severity`, `lvl` or `loglevel` key (any case,
or a number: 30 info, 40 warn, 50 error).

## `file` in a view of several files

`file` is the file's name, or the end of its path that tells two files of
one name apart:

```
file == trace.dlt
file == "ecu1/trace.dlt"        # quoted: a / would start a regex
file ~ /ecu1/
```

A `file` value no file has is warned about, with what to write instead.

## Signals and bookmarks

```
@speed > 100                 # the signal's state: the value it holds on the line
@gear.event == D             # a line that carries the value
@gear.changed                # a line where the value changed
@time >= "2025-10-18 10:00:00"
@ecu1 == 2                   # the line is in ECU1's second lifecycle
bookmark == B2               # the bookmark's name
bookmark ~ /fan/             # its name or note
```

`@name` is a signal of the tab or command ([Signals](signals.md)); an
unknown one is an error. `@time` is the line's time where the file knows it
(DLT, captures): compare it with a quoted date-time, or a time of day
(`"10:00:00"`).

## How the boxes read what you type

In the GUI's **Auto** mode (the default), you can type words, a query, or
both:

| Typed | Read as |
|---|---|
| `connection reset` | text `connection reset` |
| `level >= ERROR` | query `level >= ERROR` |
| `timeout AND level >= WARN` | query `"timeout" AND level >= WARN` |
| `NOT healthcheck` | query `NOT "healthcheck"` |
| `error and timeout` | text `error and timeout` (only upper-case `AND`, `OR`, `NOT` are operators) |
| `[ERROR] disk full` | text (`[` isn't part of a query) |
| `apid == NAV` on a text log | text (a text log has no field `apid`; the preview says so) |

A run of plain words is one exact phrase, as typed. A whole box that is one
phrase is a fast text search. A box that doesn't make a valid query for
the open file is text, and the preview names the error. **Text** mode reads
everything as text (or a regex); **NFD** mode only as a query.

## Speed

Conditions are reordered so cheap ones run first. On a 1 GiB file
(release build, Windows): `level >= WARN AND raw ~ /timeout/` on text
34 ms, `apid == NAV AND level >= WARN` on DLT 30 ms, `raw ~ /timeout/` on
DLT 0.3 s (every message is rendered as text for it).
