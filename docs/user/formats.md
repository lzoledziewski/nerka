---
title: "Files and formats"
description: "Log formats nerka reads: text logs and multi-line records, JSON Lines, AUTOSAR DLT with FIBEX, and pcap, pcapng and MF4 captures carrying DLT, SOME/IP and syslog."
---

# Files and formats

nerka recognises a file by its **content**, not its name. Files are
memory-mapped, never modified or copied, and indexed once; `nerka open
FILE` prints what a file was read as, its fields and anything skipped.

## Plain text

UTF-8 (or ASCII) text, one row per line; invalid bytes show as `�`. The
level comes from an upper-case word near the line's start
([Queries](queries.md)). How a text file is read (its records, its time,
signals for it) is its [profile](#profiles): by default a line's time is
the date-time it starts with, so most logs have `@time` and merge by time
with DLT files and captures without any setup.

### Multi-line records

A stack trace belongs to its message. Read text as **records**:

| Spec | A record |
|---|---|
| `start=/REGEX/` | starts at each line matching the regex (e.g. a date: `start=/\d{4}-\d\d-\d\d/`); lines before the first match stay one row each |
| `blank` | is separated from the next by blank lines |
| `sep=/REGEX/` | is separated by lines matching the regex (not part of either record) |
| `json` | is a JSON value (below) |
| `lines` | is one line |
| `auto` | JSON if the file looks like JSON, else lines (the default) |

GUI: the tab's menu → **Records…**, for the tab's text files (also beside
DLT files and captures), remembered for each of them; **Automatic** forgets
it. A file opened later with one of them keeps its own. CLI: `--records
SPEC` (every text file). A record's level is read from its first line; a
regex can span its lines. A `start=` or `sep=` that matches no line of a
file leaves it one record, and the status bar (`nerka open`: a note) says
so.

### Profiles

A **profile** says how a text file is read. It is one line, `NAME:
CLAUSES`, the clauses separated by `;`:

| Clause | Meaning |
|---|---|
| `name *.log, app-*.txt` | the profile is for files whose name matches (`*`, `?`; any case) |
| `first /REGEX/` | … whose first non-empty line matches |
| `records lines`, `start=/REGEX/`, `sep=/REGEX/`, `blank`, `json`, `auto` | how lines group into rows ([Multi-line records](#multi-line-records); `auto` by default) |
| `time auto` | a line's time is the date-time it starts with |
| `time /REGEX/` | a line's time is the regex's group `time` (or the whole match) |
| `time none` | no time |
| `zone utc`, `zone local`, `zone +02:00` | the zone of times written without one (default: as shown, [Times](#times)) |
| `numbers s`, `ms`, `us`, `ns`, `auto` | a time written as a plain number (`1760695201250`, `[ 12.345]`): seconds, milliseconds, microseconds or nanoseconds since 1970 (or since boot: give the file an [offset](#a-files-offset)); `auto` (the default) guesses by its size: up to 11 digits seconds, then ms, µs, ns |
| `signal SPEC` | a [signal](signals.md) for its files (repeatable): it reads only their lines, never another file's in the same view |

```
app: name app-*.log; records start=/\d{4}-/; time /^(?<time>\S+ \S+)/; zone local
kernel: name dmesg*; time /^\[\s*(?<time>[\d.]+)\]/; numbers s
json: first /^\{/; records json; time /"ts":"(?<time>[^"]+)"/
```

**Which profile a file gets:** the one forced on it (the tab's **Files…**,
remembered for the file; CLI `--profile NAME`), else the first whose `name`
matches (and whose `first` matches too, if it has one), else the first
whose `first` alone matches, else the **default**: a profile of yours named
`default`, or the built-in one (`time auto`). A profile with neither `name`
nor `first` is used only when forced. A split chosen in **Records…** wins
over the profile's `records`.

GUI: ⚙ Settings → **Profiles…** (kept in `profiles` in the configuration
folder, part of [settings bundles](bundles.md)); the tab's **Files…** shows
each file's profile and why it was chosen. Each line of a text file has
the field `profile`, its profile's name: `profile == app` keeps one kind of
log in a view of several ([Queries](queries.md#fields)).

**Signals of profiles** are shown in the Signals panel as `profile app`,
read-only: change them in Profiles…. Two profiles in one view giving a
signal of one name with the same options are one signal, each file its own
profile's regex; with other options the first file's profile is used. A
signal of yours (saved, or `--signal`) of the same name wins over a
profile's, and a profile's over a query's group or a FIBEX argument: the
signal that wins shows ⚠, its hover says what it hides (the CLI: a note). CLI: `--profile 'NAME: CLAUSES'`
defines one for the run, `--profile NAME` forces one; `nerka open` says how
each file's time is read.

**When a time can't be read** (best effort):

- `time auto` is used for a file only if at least half of its first 200
  non-empty lines start with a time; a profile's own regex if any of them
  has one. Otherwise the file has no time, as if the profile had `time
  none`, and **Files…** / `nerka open` say so.
- A line without a time of its own (a stack trace's next lines, a wrapped
  message) has the time of the line before it, and lines before the first
  time have the first one: they stay with their neighbours when sorted or
  merged by time, and `@time` is theirs too. The `time` column of a view
  of several files shows only a line's own time.
- A time of day without a date (`10:00:01.250`) is on 1970-01-01, and
  midnight isn't counted; an offset puts such a file on the others' timeline.

### A file's offset

A file's times can be moved by an **offset** (`+3h`, `-0.250s`, `90s`,
`-2m`): a logger whose clock was off, a log that counts from 1970 or from
boot. Every use of the time sees the moved one: the time column, Sort by
time, plots, checks and `@time`. Any file can have one, DLT files and
captures too. GUI: the tab's **Files…** (remembered for the file); CLI:
`--offset FILE=OFFSET` (the file as given, or its name).

## JSON

**JSON Lines** (one value per line), values one after another, or one array
of values: each value is a row and its keys are fields, nested keys with a
dot (`ctx.user == bob AND latency > 2.5`). The level is a `level`,
`severity`, `lvl` or `loglevel` key. Two JSON files open as one view when
their keys are the same.

## DLT

AUTOSAR DLT files with storage headers. The table shows the time, the
ECU's uptime (`tmsp`), `ecu`, `apid`, `ctid`, `type` and the payload as
text; [fields](queries.md#fields) filter on the headers without decoding
the payload. **Non-verbose** messages are decoded with **FIBEX** files
(`--fibex FILE`, or the Signals panel's FIBEX section): their text, APID,
CTID and level, and their arguments can be [plotted](signals.md).

## Captures: pcap, pcapng, MF4

Network captures carry protocols over UDP; nerka reads them in place:

- **Containers:** pcap (µs and ns, both byte orders), pcapng (sections,
  interfaces, time resolution), MF4 (Ethernet bus events and DLT channels;
  compressed data is unpacked once into a cache and reused).
- **Network:** Ethernet, stacked VLAN tags, Linux cooked capture, raw IP,
  loopback; IPv4 and IPv6 with fragments reassembled.
- **Protocols, by UDP destination port:** DLT (3490), SOME/IP (30490, service
  discovery; add your services' ports), syslog (514), and
  [your own](protocol-definitions.md). Settings: ⚙ Settings → Protocols (a
  rule per protocol: ports, ranges, addresses, VLAN; the most specific rule
  wins); CLI: `--dlt-port`, `--someip-port`, `--syslog-port`, `--route`,
  `--protocol-port`, `--disable-protocol`.
- Messages of several protocols share **one table** (`time`, `proto`,
  `info`, `payload` and every protocol's fields): `proto == SOMEIP AND
  service == 0x1234`.
- Every message has `net.src`, `net.dst`, `net.sport`, `net.port`, `net.vlan`
  (where its datagram came from). A capture with more than 65,536 senders
  keeps the first 65,536; the rest have no `net.*` values, and nerka says so.

What a reader couldn't read (bytes that weren't messages, datagrams not
reassembled) is reported: `nerka open` prints it, the GUI shows ⚠ in the top
bar.

## Several files as one view

Open files together and they become one view, one after another in the
order given:

```
nerka filter 'apid == NAV' ecu1.dlt ecu2.dlt bus.pcap          # FILE:LINE: per line
nerka --merge filter 'apid == NAV' ecu1.dlt ecu2.dlt bus.pcap  # sorted by time
nerka-gui ecu1.dlt ecu2.dlt bus.pcap
```

- **Which files mix:** files of one kind (text with text, JSON with the same
  keys), and messages of any protocols (a DLT file with a capture of your
  own protocol). A text or JSON file **with a time** (its
  [profile](#profiles)) joins DLT files and captures: its rows show its own
  time, `TEXT` as their `proto` (`proto == TEXT`) and the line as the
  payload; its level is read from the line as usual. Text without a time
  doesn't mix with them; the GUI opens such files as separate tabs and
  says why.
- **Sort by time** puts the lines of all a view's files in the order of
  their times, for files that know them (DLT, captures, text with a time);
  lines of the same time stay in the order of the files given, then of
  each file. GUI: the tab's menu, for one file or several; CLI: `--merge`.
- **`file`** tells the files apart in queries ([Queries](queries.md#file-in-a-view-of-several-files)).
- Bookmarks stay in each file's own `.nerka-marks`.

## What wasn't read

A damaged file still opens: what can't be read is skipped, and the status
line and the top bar say how much with ⚠ (`120 bytes weren't DLT messages:
skipped`, `3 datagrams lost a fragment: not read`, `2 MiB of the file
couldn't be read`). **Click ⚠ in the top bar** for where, one by one:

| Column | |
|---|---|
| kind | *not whole messages* (bytes of a `.dlt` file or a datagram that weren't messages), *lost a fragment* (a datagram whose IP fragments didn't all arrive), *unreadable* (the rest of a pcap after a broken block, MF4 blocks) |
| line | the first line read after it: click the row to go there |
| time | its frame's capture time; in a `.dlt` file, the message's before it |
| frame | its frame's number in a pcap / pcapng: Wireshark's `frame.number` |
| bytes | how many (a lost fragment: what did arrive) |
| detail | the protocol, the datagram's addresses, ports and VLAN, and why, as far as nerka can tell: *a message of 1520 bytes with 900 left: cut short*, *no DLT header here*; and *the capture cut its frame: 1514 of 9000 bytes kept* when the frame is shorter in the file than it was on the wire (the capture's snap length) |

Hovering a row shows its offset in the file and its first 16 bytes in hex.
Each file lists its first 10,000; past that they are counted.

What the rows usually mean:

- *cut short* with *the capture cut its frame*: the capture's snap length
  was too small for these frames; capture again with a larger one.
  *Cut short* without it: the sender wrote a message longer than the
  datagram.
- *no DLT header here*: the data stopped being DLT within a datagram. One
  sender in the summary: that sender; a whole port: something else uses
  it (Settings → Captures: protocols says which datagrams are DLT).
- *lost a fragment*: an IP fragment never reached the capture. Above the
  list, a pcapng's own statistics say whether the capture dropped packets
  (*capture drops: some*) or not (*none*: they were lost before it, on the
  network); pcap and MF4 files don't record that.
- *unreadable*, at the end: the file stops in the middle (a capture or a
  copy cut off).

**Export…** writes every skip, however many, to a tab-separated file: the
files are read again and nothing is kept in memory. At the top it says
nerka's version, the system, each file's name (not its folder), size and
kind and the capture settings; each row has the columns above and the
skip's first 16 bytes; a summary at the end counts them per kind and per
sender. That is enough to attach to a bug report without the file itself
(the 16 bytes are the only data from the file in it).

## Times

Times are kept in UTC. The GUI shows **local time** by default, the CLI
**UTC** (stable output for scripts; `--local-time` switches): ⚙ Settings →
Times. A date-time written without a zone is read in the same zone as
shown, unless the file's [profile](#profiles) names one (`zone utc`): a
text log written in UTC, merged with DLT in the GUI's local time, needs
that (or an offset). An MF4 start time marked as local time is read in this
computer's zone (`--mf4-utc` turns that off).

## Limits

- Text must be UTF-8 or ASCII; other encodings show replacement characters.
- DLT files need storage headers; FIBEX in the common convention
  (non-verbose messages; no value tables); arrays, structs and fixed-point
  arguments show as hex.
- The line index takes 8 bytes per line (about 94 MB for 12 M lines).
- No follow / tail mode yet. On Linux a file shortened while it is open can
  crash nerka; Windows doesn't allow shortening an open file.
