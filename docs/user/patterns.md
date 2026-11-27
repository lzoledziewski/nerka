---
title: "How patterns work"
description: "How nerka groups log lines into patterns and compares two sets of them: masking, Drain-style templates per chunk, alignment of templates into patterns, states and values read from the lines, queries, the G² score, and the limits of each step."
---

# How patterns work

*Experimental.* [`nerka patterns`](cli.md#patterns-experimental) and the
[🧩 Patterns window](gui.md#patterns) group lines by the message they
print, whatever values they carry. [`compare`](cli.md#compare-experimental)
and ⚖ Compare do the same with two sets of lines and list what differs.
This page explains how they do it, so you can read a result, tell a real
difference from a side effect, and know which
[setting or rule](cli.md#the-pattern-configuration) changes what.

## What a pattern is

```
connection established peer=10.0.3.7        ┐
connection established peer=10.0.9.12       ├─▶  connection established peer=<*>
connection established peer=172.16.0.4      ┘
gear=D   gear=P   gear=D   gear=N           ──▶  gear={D|P|N}
```

A pattern is a **template**: the words its lines share, and **slots**
where they differ. A slot keeps its **values**, with each one's count and
first and last line. A slot with a few values that repeat (2 to 16, each
with a letter) is a **state**, shown with its values (`{D|P|N}`); one with
more is `<*>`. Every pattern also has an **NFD query** that finds its
lines, so a pattern becomes a filter like any other.

## The steps

```
 lines ─▶ the message ─▶ masked words ─▶ stage 1: templates ─▶ stage 2: patterns ─▶ absorb ─▶ values ─▶ queries
          (field, prefix,  (digits → <*>)  (per chunk of 256 K    (templates aligned      (shape     (read from
           ignore)                          lines, in parallel)    into patterns)          splits)    the lines)
```

Stage 1 sees every line once. Stage 2 works on templates, not lines, so
its cost doesn't grow with the file. The last steps go back to lines only
where they must.

### 1. The message

- **Which text:** a DLT message's `payload`, else the line (`--field`
  chooses another).
- **Context:** lines are only ever grouped with lines of the same context:
  the short text fields of the file, such as DLT's `ecu`, `apid`, `ctid`,
  `type`, `mode`, `proto`, or a text file's `profile`, and a field that
  names the kind of message though it is a number: a non-verbose DLT
  message's id, `mid`. Fields that tell patterns apart print with them
  (`apid=NAV`, `mid=1001`). The file name is not
  context: files opened together are one set of lines.
- **`prefix` rules** cut what isn't the message off its start: a time, a
  port, an uptime (`2026-11-20 10:00:01.12 [ttyUSB_SIPDBG_03] - 0[ 0.137]
  task idle` is grouped as `task idle`). Named groups in the regex become
  fields, and context too.
- **`ignore` rules** leave lines out altogether.

### 2. Masking

The message is split at white space, and each part of a word that holds a
digit is replaced by a mask, `<*>`: numbers, hex, times, addresses and ids
(`req-378bb69b-363b` → `req-<*>-<*>`). A word without digits stays as it
is, so `[req-<*>-adef-<*>]` and `[req-<*>]` differ. Three or more bytes
written as hex in a row, one of them with a letter (`0a ff 3c 07`), are **one** value, whatever their count: a payload no one
described (a non-verbose DLT message without a FIBEX), a dump. `12 34 56`
stay three numbers. Rules adjust this:
`value /REGEX/` masks what holds no digit (host names), `literal /REGEX/`
keeps what does (`ECU1`, `v2`).

### 3. Stage 1: lines into templates

This is Drain (He et al., 2017), run on chunks of 262 144 lines (256 K) in
parallel:

- A line is compared only with templates of its context, its number of
  words and its first literal word.
- It joins the most alike template if it shares at least `similarity` of
  its words (default 0.5) and differs from it in one new word at most
  (`new-wildcards`). The differing word becomes a slot, which keeps the
  values the lines had there. Else the line starts a template of its own.
- **Header and footer:** words most lines of a context carry at the same
  place from the start or the end (`LOG:` … `Elapsed[us]: 412`) don't
  count as alike, so two short messages between the same header and footer
  stay two patterns. Each chunk holds back a context's first lines
  (`header sample`) to find these words before it groups any. A context
  with only a few lines per chunk is sampled across all chunks.
- **Caps:** at most 8 192 templates per chunk and 256 per group. Past those,
  the lines of a length go into one catch-all, shown as *not sorted*.
  Messages longer than 64 words are grouped by their first 63 (`…`).

The chunks' templates are then merged the same way, into one set.

### 4. Stage 2: templates into patterns

Stage 1 only joins messages of one length that differ word for word.
Stage 2 joins the rest, as Spell does (Du & Li, 2016), but on templates:

- Each template is split finer, into **atoms**: runs of letters, digits
  and `_`, single other characters, and masks. `gear=D` is `gear`, `=`,
  `D`.
- Two templates are **aligned** by their longest common subsequence of
  atoms. Where they differ, a slot holds both sides, which can sit inside
  a word (`gear={D|P}`), come first (`{RUNNING|IDLE} state entered`), or
  span different lengths (`throttling {engaged at <*> C|released}`, `∅`
  for nothing).
- They **join** when they share enough words. Words are weighted, so a
  header word that every line carries weighs about nothing. With one place
  differing, the words in common must be half of the smaller template's
  and a third of the larger's; with several places, 60 % of the larger's.
  One atom for another with all else the same (`gear=D`, `gear=P`) always
  joins: that is a state.
- **Limits** keep different messages apart. One merge makes at most
  `new-slots` new slots (default 1), because two slots made at once would
  always change together: `warning the data was read` and `status the data
  is read` stay two patterns until `warning the data is read` shows the
  two places vary on their own, and then all three join in a later pass
  (up to 4 passes). A side longer than `side` tokens (default 4) is a
  different message, not a value. Rules can refuse a merge too: `keep`,
  `apart`, `max-slots`, `no-merge`.
- With `lone-word` (on by default), a message's only word is never a slot:
  `INFO dlt_message_init()` and `INFO dlt_message_free()` share nothing
  but the level, so they stay two.

Last, what all of a slot's values begin or end with moves out of the slot
(`{gear=D|gear=P}` → `gear={D|P}`).

### 5. Absorb

A message can be split into two patterns by the shape of a value:
`proxy.lab.example.edu:<*> open through proxy` beside `<*> open through
proxy`. When a few sample lines of one pattern (its first, its last, the
first of each value) all match another pattern's query, the first joins
the second, however few words they share.

### 6. Values

- A slot that stage 1 made keeps the values of its lines' words, counted
  as the lines went in.
- A slot that stage 2 made or joined is **read from the lines**. Two
  templates of one message can cut it differently: in one chunk a
  template had slots where another chunk's kept the words. Their
  template-side values would then say that the same text is a different
  value on each side, which shows up as a difference at a chunk's end.
  So each of the pattern's lines is matched, masked as in step 2, against
  the final pattern, and the slot takes what the line has there. A
  template with no slot of its own is read from one of its lines, and
  every line of a masked message only once.
- A line the pattern's regex doesn't match gives `(not read)`: never a
  state's value and never compared.
- A slot of more than 16 values keeps the 16 on the most lines (*more not
  kept*) and isn't a state.

### 7. Queries

A pattern's query is its template as a regex, anchored at both ends:
words as written, `\s+` between them, a mask as `[\w.:\-]+`, a slot as
`\S+` (or `.*?` where its values span words), a state as its values. A
`prefix` rule's regex is put back in front, its groups' values written in;
fields that tell the pattern apart are added as NFD (`apid == "NAV" AND
payload ~ /…/`).

A query finds every line of its pattern, and it can find lines of another
pattern too (a template of slots only takes any line of its length). So
the window's ▾ Filter and 🔍 Search use *(the lines grouped) AND (its
query)*.

## Compare

`compare` groups both sets **together**, so a pattern is the same on both
sides, and counts each side's lines per pattern and per value. Then, for
each pattern (against all of its side's lines) and each value (against
its pattern's lines on that side), it asks whether the share changed:

| Change | |
|---|---|
| **new** | lines in B only |
| **gone** | lines in A only |
| **changed** | in both, the score at least `--min-score` (default 10.8) |
| **same** | in both, a lower score (`--show same`) |

The **score** is G², the log-likelihood ratio of a 2×2 table (the lines in
the pattern or not, in A or B; Dunning, 1993). It weighs the evidence, not
the ratio: 1 line against none scores low, 500 against none high. 3.84 is
p = 0.05 and 10.8 is p = 0.001, for one comparison. New and gone rows are
listed whatever their score. A slot with an unknown value, more than 16
values or a single one gives no value rows.

A value can stand out inside a steady pattern: `undervoltage` new in B
while `{voltage=|undervoltage} <*> mV` holds the same count.

## Costs

Measured on one machine (Windows, 20 threads), after the file's index is
built:

| File | Lines | Patterns |
|---|---|---|
| a generated text log, 1 GiB | 12 M | 1.0 s |
| a generated DLT trace, 1 GiB (5 % non-verbose, 1 000 random message ids) | 12.5 M | 2.7 s |
| a generated DLT trace, 200 MiB (70 % non-verbose, no FIBEX) | 4.3 M | 0.4 s |
| LogHub HDFS | 11 M | 1.1 s |
| LogHub BGL (many messages of many shapes in one context) | 4.7 M | 5.4 s |

Memory: 4 bytes a line for each line's template (to read values from the
lines and to list a pattern's lines), on top of the file's index.
`--stats` prints each stage's time and what it kept apart, and why.

## Limits

- **Order:** Drain's templates depend on the order lines come in, so a
  line can join one pattern in one file and another pattern in a file with
  other messages around it, or past a chunk's end (larger chunks split a
  file less, but a log of many kinds of messages in one context groups
  slower). Compare's rows from such
  splits read as a pattern gone and a near-identical one new. Compare
  similar sets, and look at a row's lines before trusting it.
- **Templates of slots only** (`<*> <*> <*>`) take any line of their
  length in their query.
- **Numbers** are masked, so patterns don't see a value grow: `Took <*>
  seconds` is one pattern whether it took 20 s or 50 s. Use a
  [signal](signals.md) and a [check](checks.md) (`value max`) for that.
- **Sequences:** a pattern counts lines, not their order, and nothing
  follows one request or block id across lines.

## Tuning it

Every number above is a [setting](cli.md#the-pattern-configuration), and
rules change them per context. To try them on your lines, use 🛠 Tuning in
the Patterns window: it runs again on each change and says which patterns
were joined or split, and **?** on two rows says why they are apart. A
rule made there starts scoped to the kind of file you tuned it on (its
profile, or its protocol), so it leaves other files alone.

---

*References:* P. He et al., "Drain: An Online Log Parsing Approach with
Fixed Depth Tree", ICWS 2017. M. Du and F. Li, "Spell: Streaming Parsing of
System Event Logs", ICDM 2016. T. Dunning, "Accurate Methods for the
Statistics of Surprise and Coincidence", Computational Linguistics, 1993.
