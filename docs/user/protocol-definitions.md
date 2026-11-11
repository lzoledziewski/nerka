---
title: "Protocol definitions"
description: "Describe your own protocol in a Kaitai Struct (.ksy) file so nerka decodes it from UDP datagrams in pcap, pcapng and MF4 captures."
---

# Protocol definitions

A protocol nerka doesn't know can be described in a `.ksy` file: a
[Kaitai Struct](https://kaitai.io) structure with a `-nerka:` section that
names the fields, columns, level and which datagrams in captures are the
protocol's. Load one with `nerka --protocol FILE` (and `--protocol-port
NAME=PORT` or `--route`), or in the GUI: ⚙ Settings → Protocols. Two
examples ship in `examples/protocols/`: `dlt.ksy` (DLT, checked against
nerka's own DLT reader) and `sensor.ksy` (a small sensor datagram).

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

<!-- {% raw %} -->
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
<!-- {% endraw %} -->

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

The same facts are fields of every message in a capture, in the words of the rules: `net.src`, `net.dst` (addresses as
text), `net.sport`, `net.port` (destination), `net.vlan` (the outer tag; no
value when untagged). They aren't columns and aren't in `raw`; the line
window lists them. Each sender (addresses, ports, VLAN) is kept once, and a
message stores its number in the bits of its index entry, so they cost no
memory per message (an offset in a file is now at most 1 TiB). At most
65,536 senders are kept: a capture of spoofed or garbled addresses can't
grow the table further, and its later senders' messages have no `net.*`
values (`CaptureStats::senders_not_kept`). The table's hash is std's
randomly seeded one, so a crafted capture can't make its lookups collide.

## Limits on untrusted definitions

Definitions will come from other people (packs, bundles), so loading and
reading never panic: file size, YAML nesting, expression and template length
and depth, types, nodes, list lengths and evaluation depth are bounded
(instances using each other in a circle have no value); a read outside the
message has no value; arithmetic wraps. Tests feed damaged definitions and
random messages to the reader.
