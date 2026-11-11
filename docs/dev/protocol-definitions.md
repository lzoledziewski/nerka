# Protocol definitions — design

_Status: built (2026-11-08, in 0.2.0-alpha.1): the language subset with lists, switch-on types, floats, enums, text templates and partial decoding; the `-nerka:` section; rules over ports, addresses and VLANs saying which datagrams are whose, for every protocol; CLI `--protocol`, `--route`, `--disable-protocol`; GUI Settings → Captures: protocols, and per capture in a tab's Files window. DLT as a definition (`examples/protocols/dlt.ksy`), payload included, gives the same results as the native reader on every test; numbers below. Not yet: packs and bundles carrying definitions, containers (binary log files). Current state: `status.md` → "Protocol definitions"._

## Goal

Protocols inside captures that Nerka has no native reader for, described in a
file instead of in code: a header with fields at fixed or flag-dependent
places, a length field, a payload of typed values. A definition is loaded at
runtime, routed by rules over UDP ports, addresses and VLANs like the native
protocols, and gives a source columns, fields and a level, so NFD, signals,
checks and the table views work on it unchanged.

Native readers stay (DLT, SOME/IP, syslog). The stress test of the language is
DLT itself, headers and verbose payloads, compared with the native reader
message by message, field by field, and benchmarked against it. Speed is best
effort: as close to native as is reasonable, not a requirement.

## Decisions (owner, 2026-11-08)

| Topic | Decision |
|---|---|
| Language | **Declarative**, no scripts. The best fit for describing protocols; an open standard if one fits: **Kaitai Struct** (`.ksy`, YAML). NFD is not extended for it. |
| What Nerka adds | Keys starting with `-`, which Kaitai's compiler ignores, so the structure stays a standard `.ksy`: a `-nerka:` section (name, framing, the fields NFD sees, columns, level, uptime, default rules), `-nerka-text` templates on types, `-nerka-partial` on attributes. Not every field of the structure is exposed: only the ones `-nerka:` names. |
| Payload text | **Templates** (owner chose them over native payload hooks): a type says how its values read as text; the format hints are Kaitai Web IDE's (`:dec`, `:hex`, `:str`, `:sep=`). |
| Matching | **Rules for every protocol**, native ones included: a port alone, or conditions on source / destination address, ports and VLAN; anything not given matches anything. A definition may carry default rules; the user's rules for a protocol replace them. |
| GUI | Settings holds the defaults (protocols on and off, their rules); **a tab's Files window** chooses the protocols of each capture, any time (owner: one trace of irrelevant traffic opened with others of valid DLT). |
| Scope now | **Protocols** (messages inside captures). Containers (binary log files of their own) later; the same description language should serve them. |
| Execution | **Compiled at load time into an intermediate form**, not into Rust code (definitions arrive at runtime, from files, later packs and bundles). |
| Comparison with native | **Not a feature of the released core.** An integration test in `core/tests/` and a benchmark run of the normal CLI; no comparison mode, flag or API. |
| Dependencies | None. Nerka reads the YAML subset `.ksy` files use itself (as it reads JSON). |

## The language: a Kaitai Struct subset

What is read; anything else is an error naming the key, at its line and column:

- `meta`: `id`, `endian` (`be`, `le`, or `switch-on` + `cases`), `bit-endian: be`; `title`, `doc`, `ks-version`, `license`, `application`, `file-extension`, `xref`, `encoding`, `tags` are accepted and ignored.
- `seq` attributes: `id`, `type`, `size`, `size-eos`, `if`, `repeat` (`expr` + `repeat-expr`, `eos`, `until` + `repeat-until`), `enum`, `contents`, `valid` (a value, or `eq` / `min` / `max` / `any-of` / `expr`), `terminator`, `encoding`, `consume: true`, `doc`.
  - Types: `u1`…`u8`, `s1`…`s8`, `f4` / `f8` (with `be` / `le` suffixes), `b1`…`b64` (bit fields, most significant bit first; `b1` is a boolean), `str` / `strz`, a user type (sized or not), a `switch-on` type (`cases`, `_` for the default), or none (bytes). A sized `str` / bytes with a `terminator` is cut at it (NUL-padded ids).
- `enums`: per type, `value: label` (or `value: { id: label }`); found like types (the type's own, then its parents').
- `instances`: `value`, or `pos` with `type` / `size` (read from the node's stream); `if`.
- `types`: nested, each with `meta` (`endian`, also switched at runtime), `seq`, `instances`, `types`, `enums`; a type name is looked up in the type's own types, then its parents'.
- Expressions: integers (decimal, `0x`, `0b`, `0o`, `_` separators), floats, `true` / `false`, strings, `enum::label`; `_root`, `_parent`, `_io.size`, `_` (in `valid` and `repeat-until`); `a.b`, `a[i]` (bytes and lists), `.length` / `.size`, `.to_i`; `not`, `and`, `or`, `?:`, comparisons, `|` `^` `&` `<<` `>>` `+` `-` `*` `/` `%`, unary `-` `~`. Precedence as in Kaitai (Python's: comparisons bind more loosely than bit operations, so `htyp & 4 != 0` is `(htyp & 4) != 0`). Integers wrap; `/` rounds down and `%` is never negative (Kaitai's rule); dividing by zero has no value. A user type used as a value is its object (its text, or its bytes as hex). Inside a list, an element's fields are reached through the list (`args[0].value`).
- Not yet: `params`, `process`, `imports`, `io`, `pad-right` / `include`, `eos-error`, `_io.pos` / `_io.eof`, methods (`to_s`, …), little-endian bit fields, recursive types, switch-on types on instances, lists of instances.

A field that isn't there (its `if` is false) has no value. `==` and the
orderings with it are false and `!=` is true, as in NFD, where a missing
field only satisfies `!=` and `!~`; arithmetic with it has no value.

## Text: templates and partial decoding

`-nerka-text` on a type says how its objects read as text:

```text
template := piece*
piece    := TEXT | '{' EXPR (':' FORMAT)? '}' | '[' template ('|' template)* ']'
FORMAT   := dec | sdec | hex | str | name | sep=TEXT
```

`{expr}` is an expression in the type's context, shown as its value reads:
numbers in decimal, a `str` as text, bytes as hex (`1f 02`), an enum by its
label, an object by its own template (else its bytes as hex), a list joined by
spaces. Formats: `dec` / `sdec` (bytes as one unsigned / signed number in the
type's byte order: 128-bit values), `hex` (numbers as `0x1f`), `str` (a `str`
decoded as UTF-8, bytes as they are; one trailing NUL dropped), `name` (an enum
label, or nothing), `sep=` (a list's separator). `[a|b]` shows the first
alternative whose values are all there and not empty, or nothing; `[[`, `]]`,
`{{`, `}}`, `||` are the characters, and in a run of `]` inside a group the
group closes first.

`-nerka-partial: TEMPLATE` on an attribute: it never makes its message
invalid. A list keeps the elements read before one that can't be, and shows
the template for the bytes after them (`_rest`) when any are left; any other
attribute that can't be read shows the template with its bytes. DLT's
payload: `<undecoded> {_rest:hex}` after the arguments read, as native DLT.

## The `-nerka:` section

```yaml
-nerka:
  name: DLTK                     # the `proto` field and column; not a native protocol's name
  framing: { length: len }       # each message says its size; or `datagram` (one per UDP payload)
  match: [ 3491, { port: 3490, src: 10.0.0.0/8 } ]   # default rules (optional)
  fields:                        # what NFD sees, in this order
    ecu:  { from: body.ecu, else: origin.channel }
    apid: body.ext.apid
    type: { from: body.ext.mstp, map: { 0: log, 1: app_trace } }
    mode: "body.verbose ? 'verbose' : 'non-verbose'"
    payload: { from: body.payload, costly: true }   # its template's text
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
numbers and booleans `u64`, floats `f64`, the rest text) and `costly`. Field
names are lower case and not NFD's own (`raw`, `level`, `bookmark`, `proto`,
`file`, `nfd`, and `net.` names, which are the capture's own). `level` maps to level names (checked when the file loads).
`origin.channel`, `origin.secs` and `origin.usecs` are what the container
knows about the message. A column names a field (with an optional `width`), or
defines its own value with `name` and `from`. Field values are as read; cells
show control characters as spaces, and the detailed view keeps line breaks and
tabs (as native DLT shows payloads).

## Matching: which datagrams are whose

A **rule** names what it cares about; the rest matches anything. As text
(CLI, settings): `3490`, `30000-30100`, or
`port=3490,src=10.0.0.0/8,dst=fe80::1,sport=50000,vlan=5`; `*` matches every
datagram. Keys: `port` (destination), `sport`, `src`, `dst` (IPv4 / IPv6
address or network), `vlan` (any of the datagram's tags). A datagram goes to
the protocol of the **most specific** matching rule (more conditions, then
longer address prefixes; a port range counts as one condition, as a single
port does). Of equally specific rules, a user's rule wins over a default one
(`--route DLTK:3490` takes port 3490 from native DLT's default), then the
protocol listed first (DLT, SOME/IP, syslog, then loaded definitions).

A protocol's rules are the user's (`--route NAME:RULE`, Settings) if any, else
its defaults: a native protocol's port list (`--dlt-port`, …), a definition's
`match:`. A protocol can be turned off (`--disable-protocol NAME`, Settings),
for every capture or for one capture of a tab (its Files window). The
transport (`input/capture/net.rs`) hands the router the addresses, ports and
VLAN tags of each datagram, reassembled ones included; MF4 `DLT_Frame`
channels still go straight to native DLT.

The same facts are fields of every message in a capture (owner,
2026-11-08), in the words of the rules: `net.src`, `net.dst` (addresses as
text), `net.sport`, `net.port` (destination), `net.vlan` (the outer tag; no
value when untagged). They aren't columns and aren't in `raw`; the line
window lists them. Each sender (addresses, ports, VLAN) is kept once, and a
message stores its number in the bits of its index entry, so they cost no
memory per message (an offset in a file is now at most 1 TiB). At most
65,536 senders are kept: a capture of spoofed or garbled addresses can't
grow the table further, and its later senders' messages have no `net.*`
values (`CaptureStats::senders_not_kept`). The table's hash is std's
randomly seeded one, so a crafted capture can't make its lookups collide.

## Compiled form

Loading a definition (`core/src/input/proto/ksy/`):

1. **YAML** (`yaml.rs`): block and flow collections (flow ones over several lines too), quoted and plain scalars, `|` / `>` blocks, comments; positions kept for errors. No anchors, aliases, tags or several documents. At most 1 MiB, nesting 64 deep.
2. **Expressions** (`expr.rs`) and **templates** (`template.rs`): parse trees, errors at their character.
3. **Model** (`model.rs`): keys checked, types and enums resolved, every name bound. Every attribute of every nested type has one static place: a tree of **nodes** (the root, each attribute of a user type, each case of a switch-on type, each positional instance; at most 1024). `body.ext.apid` becomes a node and an attribute number when the file loads; constant arithmetic is folded. Nodes inside a list are **dynamic**: they have a place per element.
4. **Plans** (`plan.rs`): each type used inside a list (and the types inside it) becomes a straight decoder: its attributes in order, each a concrete read (number, bits, bytes of a size, nested object, case table), and its conditions, sizes, instances and template as expressions over "field `i` of this object, or of one above or inside it". A type a plan can't express (checks, lists inside it, positional instances) is read by the general reader instead; the results are the same.
5. **Runtime** (`run.rs`): reading a header field walks to its node and attribute and reads the bytes in place; nothing else is parsed. Where an attribute starts, where a node starts, whether an attribute is there and small value instances are **memoized in 256-entry tables keyed by one byte of the message**, when computing them read only that byte, which is what flag-dependent headers do (DLT: `htyp`). The tables fill themselves as messages are read, each entry once, from any thread; a computation that reads more (a length, the message's size) is evaluated each time. A list element is **decoded once into a record** by its plan; templates render from records. What one message keeps (records, list scans, object layouts, frames) lives in a scratch area reused from message to message on a thread, so reading a payload doesn't allocate once it has grown.
6. **Splitting** checks each message in one sequential pass per node (`valid`, `contents`, every attribute inside its stream; partial attributes never fail, and a last one isn't read at all); a message that was split is known to fit, so reading its fields later checks only the buffer's bounds.

## Integration

- `Protocol` (`input/proto/mod.rs`) borrows its name, columns and schema from `&self` instead of `'static`; `ColumnDef::name` is a `Cow`.
- Captures: natives first (DLT 0, SOME/IP 1, syslog 2), loaded definitions after them (`CaptureOptions::protocols`, `LoadedProtocol` with its default rules), up to 16 in all; `CaptureOptions::routes` (user rules by protocol name) and `disabled`; `OpenOptions::disabled_for` per capture path.
- FIBEX goes to protocol 0 only (native DLT), never to a definition, whatever its name.
- CLI: `--protocol FILE`, `--route NAME:RULE`, `--protocol-port NAME=PORT` (short for a port rule), `--disable-protocol NAME` (all repeatable).
- GUI: definitions are files in `<config>/nerka/protocols` (Settings → Captures: protocols → Add definition…, ✖, Reload; a broken file shows its error); each protocol has an on / off box and **Rules ▸**, a table of rules with a field per condition (empty: any; a wrong field turns red); prefs keep `route=NAME:RULE` lines and `disabled_protocols`. A tab's **Files…** window shows each capture's protocols as toggles; Apply reads the files again with them.
- Not yet: packs and bundles carrying definitions (a saved format: it gets a version and a `bundle::STEPS` entry from its first breaking change), MF4 channels routed to a definition, definitions for whole files (containers).

## Checking against native DLT

`core/tests/ksy_dlt.rs` with `examples/protocols/dlt.ksy` (public API only):

- 20,000 random messages, alone and with more bytes behind them: every flag combination, message type and type info; verbose arguments of every kind (strings, trace info, raw data, booleans, integers of every length including 128-bit, floats, names and units, text that isn't UTF-8, arrays and structs native can't read either), control messages (known and unknown services, responses with and without status, version strings), non-verbose ids; NOAR off by one, payloads cut short, junk behind. Every field (the payload's text included), the level, the uptime, the whole line and the detailed view's payload are the same as native DLT's; the test checks each kind of payload was there.
- Every `htyp` value (other versions too) with `LEN` a little off, and 20,000 damaged datagrams (bytes changed, cut, junk added): both readers split them the same way, and the messages found agree.
- One capture with every datagram on two ports, native on 3490, the definition on 3491: 15 queries find the same messages for `proto == DLT` and `proto == DLTK`. Another shares one port by source address and VLAN: the more specific rule wins, user rules replace defaults, a protocol turned off hands its datagrams to the next rule.

Found on the way (native, not changed here): native DLT writes control characters of an ECU / APID / CTID id into its columns as they are, so a damaged message's tab shifts the columns and a line break splits the line; the definition writes spaces, as `Protocol::render` asks. The test leaves those messages' lines out.

## Benchmark (2026-11-08, Windows, release)

1 GiB `gen_dlt` (12.5 M messages) as a pcap (`examples/dlt_to_pcap`, 870 MB),
read by native DLT and, in separate runs, by `dlt.ksy` on the same port
(`--dlt-port 1 --protocol dlt.ksy --route DLTK:3490`); median of 3 after a
warm-up. Both readers find the same matches in every query.

| | native DLT | definition | |
|---|---|---|---|
| Open (index) | 340 ms | 4.1 s | 12× |
| `apid == NAV AND level >= WARN` | 29 ms | 89 ms | 3.0× |
| `ecu == ECU1` | 38 ms | 73 ms | 1.9× |
| `level >= ERROR` | 31 ms | 133 ms | 4.2× |
| `mcnt < 10` | 27 ms | 51 ms | 1.9× |
| `mode == non-verbose` | 26 ms | 105 ms | 4.0× |
| `payload ~ /timeout/` (decodes every payload) | 250 ms | 2.28 s | 9.1× |
| `level >= ERROR AND payload ~ /timeout/` | 51 ms | 531 ms | 10× |
| `search timeout` (every line as text) | 285 ms | 3.19 s | 11× |

Per message, one thread (`examples/ksy_cells`): split 7 ns native / 302 ns
definition; header fields 50–110 ns; a verbose payload's text 2.3 µs (native
renders it in a few hundred ns). Payload text went 30 µs → 2.3 µs over the
work: values inside list elements kept per message instead of recomputed
(730 → 195 ms on a 1.7 MB capture's full-text search), no allocation per
message (allocating costs 110–180 ns on Windows: → 160 ms), object layouts
as slot arrays (→ 139 ms), list elements decoded once by compiled plans (→ 77
ms). Header filters stay 2–4× native; opening is ~12× (splitting checks every
attribute of every header; native checks two bytes). Open: splitting was 242
ns per message before payload decoding came in, 302 ns now.

## Limits on untrusted definitions

Definitions will come from other people (packs, bundles), so loading and
reading never panic: file size, YAML nesting, expression and template length
and depth, types, nodes, list lengths and evaluation depth are bounded
(instances using each other in a circle have no value); a read outside the
message has no value; arithmetic wraps. Tests feed damaged definitions and
random messages to the reader.
