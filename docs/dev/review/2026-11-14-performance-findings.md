# Performance analysis 2026-11-14: big captures, FIBEX, 30 plotted signals

- **Commit:** `47c8acf` (0.2.0), plus the two MF4 fixes of P4 (committed with these findings).
- **Asked for (owner, 2026-11-14):** measure 2–4 GB pcap / MF4 files with a FIBEX of 30–50 messages and a plot of 30 signals; find what costs the most.
- **Machine:** i7-14700KF (20 threads), 32 GB RAM, NVMe, Windows 11, release build, warm page cache.
- **Not covered:** real captures (the synthetic trace is regular), SOME/IP and protocol definitions, enum and filtered-scope signals, Linux, the GUI on screen (the plot was timed headless through `plot_panel`).

## Test set

`gen_fibex_dlt` (new example) writes a FIBEX of 40 non-verbose messages with 100 arguments, and a trace of
them: 70 % non-verbose messages of ECU1 without extended header (APID, CTID and level from the catalog), rates
falling off as 1/(k+1) with the message's id (a few cyclic messages dominate, as in real traces), 30 % verbose
text of two ECUs.

| File | Size | Messages |
|---|---|---|
| `big.dlt` | 3.0 GiB | 65.9 M |
| `big.pcap` (`dlt_to_pcap`) | 2.1 GiB | 65.9 M |
| `big_z.mf4` (`dlt_to_mf4 --deflate`) | 1.0 GiB (3.5 GiB unpacked) | 65.9 M |

The plot is 30 numeric arguments of the 14 most frequent messages, made as the Signals panel makes them
(`mid == M AND ecu == "ECU1"`, argument i): 67.5 M events in all (m0 alone 10.8 M).

```bash
cargo run --release -p nerka-core --example gen_fibex_dlt -- big.dlt big.xml 3072 40
cargo run --release -p nerka-core --example dlt_to_pcap -- big.dlt big.pcap
cargo run --release -p nerka-core --example dlt_to_mf4 -- big.dlt big_z.mf4 --deflate
cargo run --release -p nerka-core --example perf_signals -- big.pcap big.xml 30          # each step, as the GUI does it
NERKA_PERF_FILE=$PWD/big.pcap NERKA_PERF_FIBEX=$PWD/big.xml cargo test --release -p nerka-gui perf_big_plot -- --ignored --nocapture
```

## Measurements

| Step | DLT | pcap | MF4 |
|---|---|---|---|
| Open (index) | 0.86 s | 1.05 s | first 18.3 s → **11.1 s**; cached 11.2 s → **3.8 s** (P4) |
| `mid == 1000` (10.8 M hits) | 113 ms | 99 ms | 111 ms |
| `apid == ENG AND level >= WARN` | 234 ms | 236 ms | 277 ms |
| `payload ~ /message 3:/` | 1.03 s | 1.06 s | 1.06 s |
| 30 signals' events, one after another | 4.27 s | 3.95 s | 4.30 s |
| the same, 30 at once (`--par`) | | 4.12 s | |
| Plot lanes built (first frame, UI thread) | 3.1 s¹ | **2.6 s** | 2.2 s¹ |
| A repaint at full zoom | | **182 ms** | 164 ms¹ |
| A repaint zoomed to 1 % / 0.01 % | | 14 ms / 12 ms | |
| Peak private memory: open only | 520 MiB | 1,049 MiB | |
| Peak private memory: open + 30 signals + plot | 3,193 MiB | 3,701 MiB | |

¹ `perf_signals`, a copy of the GUI's steps; the others come from `perf_big_plot`, which runs the GUI's own
`plot_panel` headless (tessellation took 5 ms of the frame).

### One change after the plot is up

The plot of 30 lanes is built, then one thing changes; the time is until the plot shows again (pcap, full
zoom, `perf_big_plot`). The plot builds only while it is shown: changes made while it is closed cost nothing
until it opens, and then one build.

| Change | Until the plot shows again | Of that |
|---|---|---|
| A lane off (the signal stays), any lane | 2.4–2.7 s | a rebuild of every lane |
| The lane on again | 2.7 s | the same |
| A new signal ticked and plotted (FIBEX tree) | **5.5 s** | 2.6 s waiting, its events 90–105 ms, 2.7 s rebuild |
| A signal removed (its lane goes too) | 2.7–2.8 s | a rebuild |
| A filter applied (`ecu == ECU1`, or `@m0_speed > 100`) | 3.1–3.3 s | the filter 245–257 ms, a rebuild |

Every row is a frozen window: each change rebuilds every lane on the UI thread (`PlotState::toggle` drops
the lanes; a new signal set or filter result changes their key), though only one lane changed, or for a filter
none whose signal is unfiltered (scope All).

Two files as one view (pcap + MF4, 132 M rows): opened in 4.9 s (one after the other); `--merge` adds 2.3 s.
Every number grows linearly with the messages, except the MF4 open before P4, which grew with messages ×
blocks.

### Files over 4 GiB

The same generator at 6 GiB (`gen_fibex_dlt big6.dlt big6.xml 6144 40`: 131.7 M messages, its first 3 GiB the
3 GiB trace), converted as above: a 6.0 GiB DLT, a 4.2 GiB pcap (4,523,923,822 bytes) and a 2.1 GiB deflated
MF4 that unpacks to 7.0 GiB. Every file reads all 131,734,210 messages, and every filter gives the same count
in the three. `filter 'mid == 1039'` (539,358 rows, the last near the files' ends) prints the same bytes for the
DLT and the MF4 (`--mf4-utc`), and for the pcap the same but the time column (its messages share their
datagram's capture time). Each step takes about twice its 3 GiB time:

| Step | DLT | pcap | MF4 |
|---|---|---|---|
| Open | 1.75 s | 2.31 s | first 25.5 s (7,149 MiB written); cached 8.0 s |
| `mid == 1000` / `apid == ENG AND level >= WARN` | 235 / 506 ms | 214 / 538 ms | 239 / 496 ms |
| `payload ~ /message 3:/` | 2.05 s | 2.16 s | 2.16 s |
| 30 signals' events (135 M) | 8.7 s | 8.4 s | 8.7 s |
| Plot lanes built | 6.7 s | 4.8 s | 5.4 s |
| A frame at full zoom | 328 ms | 332 ms | 335 ms |
| Peak private memory (open, 30 signals, plot) | 6.3 GiB | 7.3 GiB | 7.3 GiB |

With 32 GB of RAM nothing pages; the private memory (beside up to 7 GiB of mapped file) is what limits the
file size on a smaller machine (P5).

## Findings, by impact

**P1. A plot at full zoom walks every event on every repaint.** `draw_steps` thins the points to a step per
pixel column, but it still reads each point in view: 67.5 M points take 182 ms, so hovering, a cursor drag or
any repaint runs at about 5 frames per second until the view is zoomed in (1 %: 14 ms). Fix: a min / max
summary per lane (e.g. levels of 64 points per bucket, built with the lane), so a frame reads about a bucket
per pixel column at any zoom; or keep the last frame's decimation per (range, width).

**P2. The lanes are built on the UI thread, in one go, again on every change.** `build` runs inside
`plot_panel` whenever the signals' generation or the filter result changes: 2.6 s with the window frozen,
again after ticking one more signal or applying a filter. It is single-threaded, and 90 % of it is a clock lookup per point
(`source.time(row)` → `Records::msg`): a cache miss each, the events being spread over the 1 GiB message index;
the signal's scan read each of those messages already and could keep its time with the event. It copies each
event into 25 bytes (x, y, row, assumed; about
1.6 GiB here, beside the events' own 1.0 GiB). Fix: build lanes in a job, one lane per thread; rebuild only
the lanes whose signal changed (lanes by signal, kept across a toggle; a filter result only touches lanes of
filtered signals); keep x (and y if needed) only, taking rows from the signal's events.

**P2a. A new signal's lane makes the plot build twice, and more while it waits.** `build` builds the lanes in
order and returns "Collecting events…" at the first one whose events aren't cached, throwing away what it
built; the panel asks for a repaint 100 ms later and tries again. With the new signal last, each try costs the
whole 2.6 s, so the plot shows 5.5 s after the click, for events collected in 0.1 s; a signal whose events
take longer than a build (a regex signal, a bigger file) rebuilds and discards again on every try. Fix (also
without P2): check that every lane's events are cached before building any, or draw the lanes that are ready.

**P9. A change to a tab's signals collects every FIBEX and query signal again.** `refresh_signals`
(`ui/src/main.rs`) builds the tab's signals anew whenever what they come from changes (a FIBEX argument ticked
or unticked, the Selection, a kind or scope chosen): FIBEX arguments with `SignalDef::argument`, pack-query
signals with `query_signals`. A new definition has an empty cache (`Cache::default()`; only copies share one),
so every one of them reads the file again: with 30 FIBEX signals, the 4 s of P3 on every change, before the
plot's rebuild (P2, P2a). Saved signals keep theirs (they are copies). Found in the code, not timed:
`perf_big_plot` reuses the definitions, so its 5.5 s for an added signal is a lower bound; in the GUI it is
likely 7–10 s. Fix: a cache keyed by the definition (its fingerprint) and the file, so a definition built
again finds its events.

**P3. Each signal is a full pass of its own.** `SignalDef::series` scans the file once per signal: 30 signals
cost 30 × (a bare scan of about 75 ms, plus decoding their events) ≈ 4 s. Collecting them in parallel gains
nothing (4.1 s): each scan uses every core already. Fix: collect a tab's signals in one pass. For FIBEX
arguments, a map message id → signals, one scan of the messages with those ids, every argument decoded from
the one parse (estimated 1 s here: one 75 ms scan plus decoding 67 M values); for regex signals, a `RegexSet`
pass. Lifecycles and checks that use signals profit too.

**P4. MF4: a walk from the first block for every frame (fixed).** `Stream::vlsd_at` restarted at block 0 for
each frame's signal data, so an open cost frames × blocks (here 66 M frames × some 500 blocks of 4 MiB). Now a binary search over the
blocks' ends: cached 11.2 s → 5.0 s. Two hash lookups per record (`sizes`, `groups`) are skipped when the record
id is the last one's (always, in a sorted group): → 3.8 s. Still open (profiled, below): 3.6× the pcap's
1.05 s for the same messages, all on one thread: finding each frame's signal data is half of it (the binary
search per frame: the offsets only grow, so the current block is almost always the right one), reading the
offset field a sixth (`Field::raw`: a byte loop for any bit position, where an aligned 8-byte field is one
load). The first open spends 63 % unpacking 3.5 GiB on one thread (`inflate` 56 %, writing the cache 6 %): the
DZ blocks are independent and could inflate in parallel.

**P5. Memory per message and per event.** The index takes 8 B per message (DLT) or 16 B (captures), a signal
16 B per event, a plot lane 25 B per point: 3.7 GiB private memory here, besides the mapped file; a 4 GiB pcap
with the same plot needs about 7 GiB. P2's fix halves the lanes; events could keep rows as u32 per file.

**P6. A payload regex renders every message** (1.05 s against 0.1–0.2 s for header fields): known, review
11-07 R14.

**P7. Several files open one after another** (pcap + MF4: 4.9 s, the sum; in parallel about 3.8 s): known,
review 11-09 R18.

**P8. Opening is mostly the kernel's page faults.** An open is one page fault per 4 KiB of the file and of new
memory, at about 0.9 µs each, and that is most of it: DLT 0.89 s kernel time against 0.27 s user (984 k faults
= the file's 786 k pages + the index's 199 k), pcap 0.73 s against 0.59 s (43 % of its user samples land on
the first read of each datagram as well), MF4 cached 1.39 s against 2.75 s. Nothing waits for the disk (the
files come from the file cache; user + kernel time is the wall time). The message index also regrows by
doubling: copies (11 % of the pcap open's samples) and a peak of 1.55 GiB private memory for an index that
keeps 1.05 GiB (DLT 776 MiB for 520 MiB). Fix: reserve the
index from the file's size (a message is at least so many bytes); prefetch the mapping in large pieces
(`PrefetchVirtualMemory` on Windows, `madvise(WILLNEED)` on Linux); the open is single-threaded, while
pcap / DLT records could be found in parallel chunks after a quick resync.

## Profiles

CPU samples (a sampler that suspends each running thread about every millisecond and walks its stack, built
for this review; Windows' own profilers need administrator rights), on a release build with debug info, the
2.1 GiB pcap or the 1.0 GiB MF4. Percentages are of the samples of that step.

| Step | Where the time goes |
|---|---|
| MF4 open, cached (3.8 s, one thread) | finding each frame's signal data 51 % (the binary search 29 %), reading the offset field 17 %, the index (`Collector::index`) 14 %, growing it 3 % |
| MF4 open, first (11.1 s, one thread) | unpacking 63 % (`inflate` 56 %, writing the cache 6 %), then as above |
| pcap open (1.05 s, one thread) | first touch of the mapped file's pages 43 %, the index 12 %, its regrowth 11 %, DLT headers 8 % |
| A signal's scan (×30) | the message by row (`Records::msg`) 20 %, query evaluation (`Expr::eval`, field reads, `?` branches) about 45 %, a virtual `delimited()` call per row 4 % |
| A signal's values | small beside its scan: the message again, the DLT header, the FIBEX read |
| Plot build | `Records::msg` 90 % (a cache miss per point for its time) |
| A frame | the loop over the points in view (P1), nothing else of note |

The whole process (`GetProcessTimes`, `GetProcessIoCounters`, `PageFaultCount`; open only, except the last row):

| Run | Wall | User | Kernel | Page faults | Written |
|---|---|---|---|---|---|
| DLT open | 1.18 s | 0.27 s | 0.89 s | 0.98 M | — |
| pcap open | 1.38 s | 0.59 s | 0.73 s | 1.08 M | — |
| MF4 open, cached | 4.26 s | 2.75 s | 1.39 s | 1.44 M | — |
| MF4 open, first | 11.80 s | 7.97 s | 2.86 s | 1.94 M | 3,574 MiB in 4,143 writes |
| pcap: open, 30 signals, plot | 9.40 s | 67.72 s | 3.52 s | 3.49 M | — |

No run read the disk: the files were in the file cache, so every fault was a soft one. The first MF4 open waits
about 1 s on its cache writes (wall minus CPU). A machine with less free memory than the file would read it
from disk as well; not measured.

A WPR trace (`wpr -start CPU -start FileIO`, elevated) of the first MF4 open was taken, but this kit's
`xperf` can't process traces of this Windows build (`0x80070032` on a kernel event) and `tracerpt` reports
only event counts (8,038 hard faults, 7,280 disk writes, system-wide); the counters above answer the same
questions. WPA (the GUI) can still open the trace.

What isn't a problem: header filters (`mid`, `ecu`, `apid`, `level`) run at 10–30 GB/s on every format;
FIBEX lookups and argument decoding are cheap (a hash per message, the argument read in place); tessellation
is a few ms even at 400 k vertices; zoomed-in plots are fast.

## Fixed (2026-11-14)

In the order proposed below, each step measured (3 GiB set, 30 plotted signals, the GUI's code headless
through `perf_big_plot` and `perf_signals`) and checked (workspace tests, clippy, a test per change that fails
without it).

| Step | What | Measured |
|---|---|---|
| P9 | `Signals::adopt_caches`: a tab's signals built again share the events of unchanged definitions (`refresh_signals`) | a signal added: its events collected 4.1 s → 0.1 s; removed: 4.1 s → 0 |
| P2a | `build` checks every lane's events before building any | a signal added: 5.3 s → 2.7 s until the plot shows |
| R-A (P3) | `Signals::collect` (`core/src/signal/collect.rs`): keyed signals in one pass per file; the GUI's background job uses it | 30 signals: 4.2 s → 1.5 s (pcap), 4.3 s → 1.8 s (MF4) |
| R-B (P1, P2) | `nerka_core::plot::lane`: lanes built in parallel, a min / max summary and each block's first x; the GUI keeps lanes per signal (`LaneKey`) | below |
| P4 | MF4: the current block before a search; an aligned u64 field in one load | cached open 3.73 s → 2.58 s |
| P8 | Mapped pages brought in on every core before a one-thread reader (`input::prefault`); the index sized from its first messages | pcap 1.03 → 0.62 s, MF4 cached 2.58 → 2.20 s, DLT 6 GiB 1.70 → 0.91 s, pcap 6 GiB 2.31 → 1.24 s; peak private memory of a pcap open 1,554 → 1,080 MiB, DLT 6 GiB 1,066 MiB for a 1,005 MiB index |
| P4b | DZ blocks up to 64 MiB unpacked in parallel batches, written in order (the cache file byte for byte the same) | first MF4 open 10.7 → 8.5–9.7 s: now bound by writing 3.5 GiB to the cache (~600 MB/s here) |
| P7 | Several files opened at once (`open_all`), each a part of the progress by its size (`Progress::part`; before, the bar sat at 100 % after the first file) | pcap + MF4: 4.86 → 2.36 s; merged by time 7.17 → 4.68 s |

The plot with 30 lanes, before → after R-B:

| | Before (after P9, P2a, R-A) | After |
|---|---|---|
| A repaint at full zoom | 182 ms | 12 ms |
| The first frame (all lanes built) | 2.66 s | 0.30 s |
| A lane off / on | 2.4–2.7 s | 13–23 ms (the 10.7 M-point lane back on: 148 ms) |
| A signal added / removed, until the plot shows | 2.8 s / 2.8 s (6.9 s / 6.8 s before P9) | 0.13 s / 0.04 s |
| A filter applied | 2.9–3.0 s | 0.23–0.29 s (the filter itself) |

Checked after all steps: the 6 GiB set gives the same output byte for byte as before the fixes
(`filter 'mid == 1039'` on the DLT and the pcap), the 3 GiB pcap and MF4 the same rows; `tools/bench.sh`
(`KSY=1`, 5 runs) within ±9 % of `baseline/bench-2026-11-09.tsv` on every query, the same match counts.

**Still open:** the lanes are built on the UI thread (0.3 s at first, 0.15 s for a 10 M-point lane: a job
when files grow); a merge by time interleaves on one thread (2.3 s for 132 M rows); the MF4 reader (2.2 s
against the pcap's 0.6 s, a frame per message); a lane with defaults (a signal bound to a lifecycle) is still
drawn point by point; an enum that changes on most points walks them all; P5 (memory per event and point),
P6 (payload regexes), a parallel `.dlt` index.

### On Linux: 0.2.0 against the fixes

The same 3 GiB set (generated in the VM, the same bytes), in the local VirtualBox VM (Ubuntu 24.04, 6 CPUs,
16 GB, software rendering only), 0.2.0 (`v0.2.0` with the measuring code alone added) against `09920bb`;
release builds, warm file cache, CLI times the median of 3. Tests pass in both (core 326 / 335, GUI 126 / 128).

| Step | 0.2.0 | Fixed |
|---|---|---|
| Open `.dlt` / pcap | 0.36 / 0.73 s | 0.35 / 0.69 s |
| Open MF4: first (unpacks) / cached | 22.8 / 9.8 s | 6.9 / 1.9 s |
| `mid == 1000` / `apid == ENG AND level >= WARN` / `payload ~ /message 3:/` (pcap) | 0.25 / 0.43 / 2.2 s | 0.19 / 0.40–0.44 / 2.1 s |
| 30 signals collected | 7.8 s | 1.65 s |
| Plot lanes built / a frame at full zoom | 2.46 s / 254 ms | 0.55 s / 20 ms |
| A lane off / on (the GUI's plot) | 2.4–2.8 s | 17–32 ms (183 ms for the 10.7 M-point lane) |
| A signal added / removed, until the plot shows | 10.5 / 10.2 s | 0.25 / 0.05 s |
| A filter applied, until the plot shows | 3.3–3.4 s | 0.4–0.5 s |
| A frame of the whole window, idle / scrolling (pcap) | 85–98 ms | 0.2–0.8 ms |
| Peak private memory: open / open + 30 signals + plot | 1,020 / 3,758 MiB | 1,020 / 4,344 MiB (3,692 after P10) |

Opening and searching are about the same on Linux: page faults cost less there than on Windows (P8 mattered
on Windows). Filters are unchanged by the fixes (a first run of 0.2.0 that showed `apid == ENG AND level >=
WARN` twice as slow ran while the system wrote back the MF4 cache; rerun quietly, both 377 ms on the
`.dlt`). The GUI on the VM's Wayland desktop: the 2.1 GiB pcap, six signals plotted, window saved
(`--screenshot`) 2.6 s after the start.

**P10. Collecting signals together peaked higher (fixed).** With 30 signals, `Signals::collect` peaked at
4,374 MiB private on Linux, one by one 3,712 MiB (0.2.0: 3,758): every signal's pieces per stretch of rows
were held until the end, and glibc's per-thread heaps keep what such small pieces free while the big
arrays fill (`MALLOC_ARENA_MAX=2`: 4,173 MiB). Sizing the joined arrays exactly helped Windows only. Now
the rows go in batches (stretches for every thread four times over), each batch's pieces appended and
freed before the next, the arrays sized once at the first batch's rate: Linux 3,692 MiB in 1.59–1.81 s
(as before); Windows 3,763–3,794 MiB (3,804) and faster, 1.12–1.18 s (1.94–2.09). Tested: batches that
don't line up with the stretches give the same events.

## Fixes

Effort: S hours, M days, L a week or more. Costs from the measurements above (30 plotted signals, 3 GiB).

| # | Fix | Now → expected | Effort |
|---|---|---|---|
| P9 | Signal caches keyed by the definition's fingerprint and the file, not by the definition object | ~4 s per signal change → the new signal's pass (~0.1 s) | S |
| P2a | Build lanes only when every lane's events are cached (or draw the ready ones) | −2.6 s per added signal | S |
| P2 | (a) the time of each event kept with it when collected; (b) lanes cached per signal; (c) built in a job, a lane per thread | 2.4–3.0 s frozen per change → under 0.1 s | M |
| P1 | A min / max summary per lane (enum: one value or mixed) | 182 ms a frame → 1–2 ms | M |
| P3 | One pass for a tab's signals: FIBEX arguments through a message id → signals map, regex signals through a `RegexSet`, other queries a pass of their own | 4 s → about 1 s | M |
| P8 | Reserve the index from the file's size; prefetch the mapping in large pieces; index `.dlt` in parallel chunks | opens about −30–50 %; no peak above the index | S / S / M |
| P4 | `vlsd_at`: the current block first; `Field::raw`: one load for an aligned field | MF4 cached 3.8 s → about 2 s | S |
| P4b | DZ blocks inflated in parallel, written in order | MF4 first open 11 s → about 4 s | M |
| P5 | x only in lanes (with P2a); rows as u32 per file; the block-compressed index (roadmap) | about −1.5 GiB here; the index later | S–L |
| P6 | Payload regexes over decoded arguments, or rendered text cached (R14) | 1 s | L |
| P7 | Files opened in parallel, one progress total (R18) | pcap + MF4 4.9 s → the slowest file | M |

Adding one signal to a plot of 30, the case the owner asked about: likely 7–10 s now (P9, P2a, P2); about
2.8 s after P9 and P2a; under a second after P2.

## How the fixes fit the architecture

Checked against spec §4 (the UI displays and sends queries; core does the processing), the format-neutral,
row-at-a-time `LogSource` (R3), `sources-and-signals.md` (one pass over a container feeds every decoder; a
`SignalStore` of `(time, value)` per signal; subscriptions: only subscribed signals decoded, the rest skipped
after a hash lookup; batches at the plugin boundary) and CLAUDE.md's rules (`&Progress` and cancellation for
long work, every fast path tested against the naive one).

| Fix | Fit | Why |
|---|---|---|
| P2a, P4, P8 (reserve, prefetch) | Fits | Local to a function or a reader; the mapping stays read-only and untouched. |
| P4b | Fits | Inside `unpack.rs`; the cache file stays the same. |
| P7 | Fits | Planned (R18); `open_many` and `Progress` take it. |
| P9 | A small extension | `Signals::fingerprint` exists; caches already live with a definition and are shared by its copies. Only the cache's identity changes. |
| P3 | Against the code, toward the design | Each `SignalDef` scans the file itself, which is not the design: one pass feeding every decoder, subscribed signals decoded after one hash lookup. A collector over a tab's signals is that design. |
| P2 (a) | The same | The design's `SignalStore` holds `(time, value)`; the cache holds `(row, value)` and the plot asks the source for each time. |
| P1, P2 (b, c) | In the wrong layer now | Building the lanes and thinning them per pixel is processing in `ui/src/plot.rs`, against spec §4: not testable without the GUI, not usable by the CLI or a later MCP server, without `Progress`. |
| Row-at-a-time `LogSource` (a field read and a dynamic call per row) | A known limit | The design plans batches at the plugin boundary; the engine is still per row. |

## Refactor or redesign?

**No redesign of the engine or of `LogSource`.** A scan of 66 M messages takes 75 ms (10–30 GB/s) and
profiles as efficient; the time goes to repeating work (30 passes, rebuilding after every change, collecting
again after every edit), not to the per-row API. A batch or columnar engine would be a large change for a small
gain once the repetition is gone; revisit if filters on files over 4 GiB become slow.

**Two refactors, so the fixes land in the right place:**

- **R-A. Signal collection as the design's `SignalStore` (core).** A collector over a tab's signals makes one
  pass per file for all of them (P3: FIBEX arguments through a message id map, regex signals through a
  `RegexSet`, other queries a pass of their own); the store keeps the time with each event (P2 a) and is keyed
  by the definition's fingerprint and the file (P9). `SignalDef::series` / `cached_series` stay the API, so
  filters, lifecycles, checks and the GUI keep working; the invariant to test: events collected together equal
  those collected one by one, per kind of signal.
- **R-B. Plot data in core.** `SignalSeries` → a lane's points on the clock, plus its min / max summary (P1);
  lanes cached one by one and built in a job with `Progress` (P2 b, c). `ui/src/plot.rs` keeps painting and
  interaction. This meets spec §4, makes the plot testable headless, and gives an export or the MCP server the
  same thinned data.

**Not now:**

- Parallel indexing of captures (pcap / MF4): fragment reassembly and routing carry state across the file, so
  chunks are a real design change; P8 takes most of the gain. A parallel `.dlt` index is easy and worth it.
- The compressed index and u32 rows: memory only, no limit changes. There is no 4 GiB limit: text and `.dlt`
  keep u64 offsets and rows; captures pack the offset into 40 bits (1 TiB per file, `MAX_OFFSET`); u32 rows
  would be per file (4 G messages, a fall-back to u64 above). They pay off when the index no longer fits
  beside the file in RAM: at 8–16 B per message about 1 GiB per 4 GiB of small DLT messages. Worth it if files
  of 8 GiB and more are a goal (a question for the owner). Files over 4 GiB work ("Files over 4 GiB" above).
- The full time model (`Timestamp { ns, clock }`) isn't needed for these fixes, but see below.

**With the next features of 0.2:**

- **Profiles** (signals applied by file rules) add many signals per tab: without R-A and P9 every match costs
  a pass per signal, and every change all of them again.
- **A file's time adjusted in a merged view:** R-A's time column must come through the source's time, with
  its adjustment, and be keyed by it; the file's clock offset then belongs to the source (the smallest part of
  the time model).
- **Signal options customized in packs:** definitions change often; with P9 a change costs one signal.

**Order:** P9 and P2a (small; most of the frozen time after a change); R-A (P3, P2 a); R-B (P1, P2); P4 and P8;
P4b and P7; later P5 and P6. R-A before profiles, which build on it.
