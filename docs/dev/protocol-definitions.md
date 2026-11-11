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

## For users

The language, text templates, the `-nerka:` section, matching rules and
the limits on untrusted definitions are documented for users in
[`docs/user/protocol-definitions.md`](../user/protocol-definitions.md): keep the
language subset current there. This file keeps the design.

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

