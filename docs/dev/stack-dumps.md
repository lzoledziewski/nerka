# Stack dumps (design; not built)

Status: **proposed** for the owner's review (2026-11-25, "plan the
feature"). Nothing built; the questions are at the end.

## Why

In the evaluation (`ai-evaluation.md`), every agent that read a Go
goroutine dump asked for the same thing, and spent most of its calls doing
it by hand:

- **Grouping identical stacks** (panicparse): 64 goroutines are ~20
  kinds, and the few odd ones are the story. Records plus `patterns` got
  close, but a stack's template keeps arguments, offsets and `fp=` /
  `sp=` / `pc=`: a dump of 96 goroutines stayed 56 groups.
- **Who waits on whom:** a deadlock was found by matching hex
  addresses across 15 goroutines (`semacquire1(0xc000011bb0…)`,
  `chansend(0xc0143402a0…)`), and who created whom (`created by … in
  goroutine 74`).
- **The frame that matters:** the first frame outside the runtime and
  the standard library, under 20 lines of `runtime.gopark`.

People reading dumps want the same: nerka has no reader for them.

## Proposal: a dump is records with fields

A **record split that knows the dump's format**, beside `start=`,
`sep=`, `blank` and `json`: `go` (goroutine dumps and panics) and `java`
(thread dumps and exception traces; Python tracebacks a third, later).
Each record is one goroutine (one thread, one exception); the split gives
the records **fields**, as `json` gives a record's keys:

| Field (`go`) | From | Example |
|---|---|---|
| `gid` | the header `goroutine N` | `750266` |
| `state` | the header's `[…]`, without the time | `chan send`, `IO wait`, `sync.Mutex.Lock`, `running` |
| `wait` | the header's minutes | `34` |
| `created_by` | `created by … in goroutine N` | `74` |
| `top` | the first frame outside `runtime.` and the standard library | `march.(*March).makeListDir.func2` |
| `waits_on` | the address in the top runtime frame's call (`chansend`, `chanrecv`, `semacquire1`, `lockSlow`, …) | `0xc000011bb0` |
| `stack` | the **signature**: the frames' function names, one a line, without arguments, offsets, `fp=` / `sp=` / `pc=` or file lines | `runtime.gopark` … `march.(*March).makeListDir.func2` |

| Field (`java`) | Example |
|---|---|
| `thread`, `state` (thread dumps) | `"main"`, `WAITING` |
| `exception`, `message` | `java.lang.IllegalStateException`, `connection closed` |
| `top` | the first frame not in `java.` / `javax.` / `sun.` |
| `cause` | the last `Caused by:`'s exception |
| `waits_on` | `- waiting to lock <0x…>` / `- parking to wait for <0x…>` |
| `stack` | the signature |

With them, that deadlock is three queries, the same in the CLI, the window and
MCP:

```
state == "chan send"                       # the two holders
waits_on == "0xc000011bb0"                 # who waits on the source side's mutex
created_by == 74                           # job 74's goroutines
```

and `patterns field=stack` (`--field stack`) groups identical stacks
whatever their arguments: the panicparse view, in the existing pattern
engine, its configuration and `compare` (two dumps of one hang, taken a
minute apart: which goroutines didn't move).

**A log with a dump in it** (debug lines, then the dump): lines
before the first goroutine stay rows (built,
`ai-evaluation.md`), so the log and the dump are one tab; the fields are
empty on the log's rows.

## Where

- **Core:** `input/records.rs`: `RecordSplit::Go` / `Java` next to
  `Json` (the start of a record is the format's own: `^goroutine \d+`,
  `^"…" #` / `^\S` for Java); a `DumpFields` beside `JsonFields`, parsed
  once per record when the file is split (a dump is small: thousands of
  records). The signature is a field like any other, so `patterns
  --field stack` needs nothing new.
- **Hints:** the stack-dump hints (built) suggest `go` / `java` instead
  of a `start=` regex.
- **CLI:** `--records go`, `--records java`.
- **Window:** Records… offers *Go goroutine dump* and *Java stack
  traces*; a tab read so gets the fields as columns (`state`, `wait`,
  `top`), the stack folded in each row as records are now.
- **MCP:** `set_records go`; `help` lists the fields.
- **Docs:** `formats.md` (records), `queries.md` (the fields).

## Cost

A dump of 100 goroutines and 2,000 lines parses in microseconds; the
fields are kept per record (a few strings each). Nothing changes for
other files.

## Questions for the owner

1. **A record split, or a profile?** Proposed: a split (`--records go`),
   since the fields come from the format, like `json`'s keys. A profile
   (`text-profiles.md`) would let users define their own, but needs
   multi-line captures and a signature transform that profiles don't
   have.
2. **Formats first:** Go only (the evaluation's), or Go and Java
   together? Python tracebacks?
3. **Saved choices:** a file's records choice is remembered
   (`<config>/nerka/records`); a new spec (`go`) read by an older nerka is
   an error there. A format step (`bundles.md`) or accepted as an
   addition?
4. **A wait-for view** (who holds what, cycles), beyond the fields?
   Proposed: not now; the fields make it three queries.
