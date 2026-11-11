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
| `expand` | turn [pack](packs.md) picks into plain NFD |
| `packs` | list the loaded packs and their queries |
| `check FILE…` | run the [checks](checks.md) (`--check`): issues and facts |
| `lifecycles FILE…` | print the [lifecycles](lifecycles.md) (`--lifecycle`) and their findings |
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
| `--merge` | several files: interleave their lines by time (files that know their times: DLT, captures); else one after another |
| `--records SPEC` | read text as records: `start=/REGEX/`, `blank`, `sep=/REGEX/`, `json`, `lines` or `auto` ([Files and formats](formats.md)) |
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
