# Protocol definitions — design

_Status: first version built (2026-11-08, branch `claude/ksy-protocols`): the language subset, the `-nerka:` section, captures routing UDP ports to definitions, CLI `--protocol`. DLT's headers as a definition (`core/tests/data/dlt.ksy`) give the same results as the native reader on every test; numbers below. Not yet: GUI, packs and bundles, typed payloads, containers. Current state: `status.md` → "Protocol definitions"._

## Goal

Protocols inside captures that Nerka has no native reader for, described in a
file instead of in code: a header with fields at fixed or flag-dependent
places, a length field, a payload. A definition is loaded at runtime (CLI
`--protocol FILE`), routed by UDP ports like the native protocols, and gives a
source columns, fields and a level, so NFD, signals, checks and the table
views work on it unchanged.

Native readers stay (DLT, SOME/IP, syslog). The stress test of the language is
DLT itself: its standard and extended headers as a definition, compared with
the native reader message by message, field by field, and benchmarked against
it. Speed is best effort: as close to native as is reasonable, not a
requirement.

## Decisions (owner, 2026-11-08)

| Topic | Decision |
|---|---|
| Language | **Declarative**, no scripts. The best fit for describing protocols; an open standard if one fits: **Kaitai Struct** (`.ksy`, YAML). NFD is not extended for it. |
| What Nerka adds | A `-nerka:` section in the same file: name, framing, the fields NFD sees, columns, level, uptime. Kaitai's tools ignore keys that start with `-`, so the structure stays a standard `.ksy`. Not every field of the structure is exposed: only the ones `-nerka:` names. |
| Scope now | **Protocols** (messages inside captures). Containers (binary log files of their own) later; the same description language should serve them. |
| Payload | Shown as hex for now; decoding typed arguments (DLT verbose) is a later phase. |
| Execution | **Compiled at load time into an intermediate form**, not into Rust code (definitions arrive at runtime, from files, later packs and bundles). |
| Comparison with native | **Not a feature of the released core.** An integration test in `core/tests/` and a benchmark run of the normal CLI; no comparison mode, flag or API. |
| Dependencies | None. Nerka reads the YAML subset `.ksy` files use itself (as it reads JSON). |

## The language: a Kaitai Struct subset

What is read; anything else is an error naming the key, at its line and column:

- `meta`: `id`, `endian` (`be`, `le`, or `switch-on` + `cases`), `bit-endian: be`; `title`, `doc`, `ks-version`, `license`, `application`, `file-extension`, `xref`, `encoding`, `tags` are accepted and ignored.
- `seq` attributes: `id`, `type`, `size`, `size-eos`, `if`, `contents`, `valid` (a value, or `eq` / `min` / `max` / `any-of` / `expr`), `terminator`, `encoding`, `doc`.
  - Types: `u1`…`u8`, `s1`…`s8` (with `be` / `le` suffixes), `b1`…`b64` (bit fields, most significant bit first; `b1` is a boolean), `str` / `strz`, a user type (sized or not), or none (bytes). A sized `str` / bytes with a `terminator` is cut at it (NUL-padded ids).
- `instances`: `value`, or `pos` with `type` / `size` (read from the node's stream); `if`.
- `types`: nested, each with `meta` (`endian`, also switched at runtime), `seq`, `instances`, `types`; a type name is looked up in the type's own types, then its parents'.
- Expressions: integers (decimal, `0x`, `0b`, `0o`, `_` separators), floats, `true` / `false`, strings; `_root`, `_parent`, `_io.size`, `_` (in `valid`); `a.b`, `a[i]`, `.length` / `.size`; `not`, `and`, `or`, `?:`, comparisons, `|` `^` `&` `<<` `>>` `+` `-` `*` `/` `%`, unary `-` `~`. Precedence as in Kaitai (Python's: comparisons bind more loosely than bit operations, so `htyp & 4 != 0` is `(htyp & 4) != 0`). Integers wrap; `/` rounds down and `%` is never negative (Kaitai's rule); dividing by zero has no value. A user type used as a value is its bytes.
- Not yet: `repeat`, `switch-on` types, `enum` / `enums`, `params`, `process`, `imports`, `io`, floats as types, `pad-right` / `include` / `consume`, `_io.pos` / `_io.eof`, methods (`to_s`, …), little-endian bit fields, recursive types.

A field that isn't there (its `if` is false) has no value. `==` and the
orderings with it are false and `!=` is true, as in NFD, where a missing
field only satisfies `!=` and `!~`; arithmetic with it has no value.

## The `-nerka:` section

```yaml
-nerka:
  name: DLTK                     # the `proto` field and column; not a native protocol's name
  framing: { length: len }       # each message says its size; or `datagram` (one per UDP payload)
  fields:                        # what NFD sees, in this order
    ecu:  { from: body.ecu, else: origin.channel }
    apid: body.ext.apid
    type: { from: body.ext.mstp, map: { 0: log, 1: app_trace } }
    mode: "body.verbose ? 'verbose' : 'non-verbose'"
    payload: { from: body.payload, render: hex }
  level:
    if: has_ext and body.ext.mstp == 0
    from: body.ext.mtin
    map: { 1: fatal, 2: error, 3: warn, 4: info, 5: debug, 6: trace }
  uptime: body.tmsp / 10000.0    # seconds
  columns:                       # after `time`; the last one is the payload
    - { name: tmsp, from: body.tmsp / 10000.0, decimals: 4, width: 11 }
    - { name: ecu, width: 4 }
    - payload
```

A value is an expression, or a map: `from` (an expression), `if`, `map` (from
the value to a name), `else` (a value used when there is none: `if` false, a
missing field, no entry in `map`), `decimals` (numbers), `render` (`hex` or
`text`). A field also takes `kind` (`str`, `u64`, `f64`, `name`; else inferred:
numbers and booleans `u64`, floats `f64`, the rest text) and `costly` (hex
fields are). Field names are lower case and not NFD's own (`raw`, `level`,
`bookmark`, `proto`, `file`, `nfd`). `level` maps to level names (checked when
the file loads). `origin.channel`, `origin.secs` and `origin.usecs` are what
the container knows about the message. A column names a field (with an
optional `width`), or defines its own value with `name` and `from`. Text cells
show tabs and line breaks as spaces (`Protocol::render`).

## Compiled form

Loading a definition (`core/src/input/proto/ksy/`):

1. **YAML** (`yaml.rs`): block and flow collections (flow ones over several lines too), quoted and plain scalars, `|` / `>` blocks, comments; positions kept for errors. No anchors, aliases, tags or several documents. At most 1 MiB, nesting 64 deep.
2. **Expressions** (`expr.rs`): the parse tree, errors at their character.
3. **Model** (`model.rs`): keys checked, types resolved, every name bound. There are no arrays yet, so every attribute of every nested type has one static place: a tree of **nodes** (the root, each attribute of a user type, each positional instance; at most 1024). `body.ext.apid` becomes a node and an attribute number when the file loads; constant arithmetic is folded.
4. **Runtime** (`run.rs`): reading a field walks to its node and attribute and reads the bytes in place; nothing else is parsed. Where an attribute starts (in bits from its node), where a node starts, whether an attribute is there (its `if`) and small value instances (numbers, booleans) are **memoized in 256-entry tables keyed by one byte of the message**, when computing them read only that byte, which is what flag-dependent headers do (DLT: `htyp`). The tables fill themselves as messages are read, each entry once, from any thread. A computation is a function of what it reads, so the same byte value gives the same result; one whose first read is elsewhere, or that reads more (a length, the message's size), is marked and evaluated each time. Within one message, node starts and stream ends are kept, with what computing them read, so the tables learn the same dependencies.
5. **Splitting** checks each message in one sequential pass per node (`valid`, `contents`, every attribute inside its stream); a message that was split is known to fit, so reading its fields later checks only the buffer's bounds.

## Integration

- `Protocol` (`input/proto/mod.rs`) borrows its name, columns and schema from `&self` instead of `'static`; `ColumnDef::name` is a `Cow`.
- Captures: natives first (DLT 0, SOME/IP 1, syslog 2), loaded definitions after them, up to 16 in all. `CaptureOptions::protocols` lists each (`LoadedProtocol`) with its UDP ports; a port goes to the first protocol that lists it, natives first. To give a definition a port a native has by default, move the native one (`--dlt-port 1`).
- FIBEX goes to protocol 0 only (native DLT), never to a definition, whatever its name.
- CLI: `--protocol FILE` (repeatable), `--protocol-port NAME=PORT` (repeatable).
- Not yet: the GUI (loading definitions, their ports in ⚙), packs and bundles carrying definitions (a saved format: it gets a version and a `bundle::STEPS` entry from its first breaking change), MF4 channels routed to a definition, definitions for whole files (containers), typed payload arguments.

## Checking against native DLT

`core/tests/ksy_dlt.rs` with `core/tests/data/dlt.ksy` (public API only):

- 20,000 random messages, alone and with more bytes behind them (every flag combination, message type and type info, ids with NUL padding, with and without a container ECU): every field but the payload, the level, the uptime and every column but the payload are the same as native DLT's.
- Every `htyp` value (other versions too) with `LEN` a little off, and 20,000 damaged datagrams (bytes changed, cut, junk added): both readers split them the same way (offsets and bytes left), and the messages found agree.
- One capture with every datagram on two ports, native on 3490, the definition on 3491: 14 queries (header fields, levels, `NOT`, a regex over the line) find the same messages for `proto == DLT` and `proto == DLTK`.

Found on the way (native, not changed here): native DLT writes an ECU / APID / CTID id's tabs and line breaks into its columns as they are, so a damaged message's tab shifts the columns and a line break splits the line; the definition writes spaces, as `Protocol::render` asks. The test leaves those messages' columns out.

## Benchmark (2026-11-08, Windows, release, 1 GiB `gen_dlt`, 12.5 M messages)

`tools/bench.sh` with `KSY=1` (and `NERKA_BIN` for a build elsewhere). Native numbers are unchanged by the change (`origin/main` → branch, median of 5: DLT `apid == NAV AND level >= WARN` 29.8 → 29.3 ms, `ecu == ECU1` 37.8 → 37.6 ms, `raw ~ /timeout/` 295 → 290 ms, pcap 28.4 → 28.4 ms, text queries within noise; capture open 333 → 333 ms).

| | native DLT | definition |
|---|---|---|
| Open `big.pcap` (index; definition on 3490, `--dlt-port 1`) | 333 ms | 3.2 s |
| `apid == NAV AND level >= WARN` on `big.pcap` | 29 ms | 62 ms |
| Same messages in one file (`big2.pcap`, both ports), `proto == … AND apid == NAV AND level >= WARN` | 100 ms | 137 ms |
| … `ecu == ECU1` | 114 ms | 138 ms |
| … `level >= ERROR` | 91 ms | 168 ms |
| … `mcnt < 10` | 94 ms | 109 ms |
| … `raw ~ /ECU1 NAV/` (renders every line; the definition's payload is hex, twice as long) | 709 ms | 2030 ms |

Per message, one thread (1 M messages): split 11 ns native / 242 ns definition, `apid` 10 / 37 ns, level 7 / 91 ns. Searching is within 1.2–2.2× of native; opening is ~10× (splitting checks every attribute of every message; native checks two header bytes). What made the difference: memo tables for positions (search 2×), one sequential pass per node when splitting and a per-message cache of stream ends (open 8.1 s → 5.0 s), memoized `if`s and small instances (5.0 → 3.2 s, level 153 → 91 ns), level names mapped at load (no allocation per message).

## Limits on untrusted definitions

Definitions will come from other people (packs, bundles), so loading and
reading never panic: file size, YAML nesting, expression length and depth,
types, nodes and evaluation depth are bounded (instances using each other in
a circle have no value); a read outside the message has no value; arithmetic
wraps. Tests feed damaged definitions and random messages to the reader.
