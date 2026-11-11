# Files and formats

nerka recognises a file by its **content**, not its name. Files are
memory-mapped, never modified or copied, and indexed once; `nerka open
FILE` prints what a file was read as, its fields and anything skipped.

## Plain text

UTF-8 (or ASCII) text, one row per line; invalid bytes show as `�`. The
level comes from an upper-case word near the line's start
([Queries](queries.md)). A text file has no time of its own; a
[time signal](signals.md) can read one from the lines.

### Multi-line records

A stack trace belongs to its message. Read text as **records**:

| Spec | A record |
|---|---|
| `start=/REGEX/` | starts at each line matching the regex (e.g. a date: `start=/\d{4}-\d\d-\d\d/`) |
| `blank` | is separated from the next by blank lines |
| `sep=/REGEX/` | is separated by lines matching the regex (not part of either record) |
| `json` | is a JSON value (below) |
| `lines` | is one line |
| `auto` | JSON if the file looks like JSON, else lines (the default) |

GUI: the tab's menu → **Records…** (remembered per file). CLI: `--records
SPEC`. A record's level is read from its first line; a regex can span its
lines.

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
nerka --merge filter 'apid == NAV' ecu1.dlt ecu2.dlt bus.pcap  # interleaved by time
nerka-gui ecu1.dlt ecu2.dlt bus.pcap
```

- **Which files mix:** files of one kind (text with text, JSON with the same
  keys), and messages of any protocols (a DLT file with a capture of your
  own protocol). Text and JSON don't mix with the others; the GUI opens them
  as separate tabs and says why.
- **Merge by time** interleaves lines by their time, for files that all
  know it (DLT, captures); GUI: the tab's menu → **Files…**.
- **`file`** tells the files apart in queries ([Queries](queries.md#file-in-a-view-of-several-files)).
- Bookmarks stay in each file's own `.nerka-marks`.

## Times

Times are kept in UTC. The GUI shows **local time** by default, the CLI
**UTC** (stable output for scripts; `--local-time` switches): ⚙ Settings →
Times. A date-time written without a zone is read in the same zone as
shown. An MF4 start time marked as local time is read in this computer's
zone (`--mf4-utc` turns that off).

## Limits

- Text must be UTF-8 or ASCII; other encodings show replacement characters.
- DLT files need storage headers; FIBEX in the common convention
  (non-verbose messages; no value tables); arrays, structs and fixed-point
  arguments show as hex.
- The line index takes 8 bytes per line (about 94 MB for 12 M lines).
- No follow / tail mode yet. On Linux a file shortened while it is open can
  crash nerka; Windows doesn't allow shortening an open file.
