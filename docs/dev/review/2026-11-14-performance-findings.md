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
Every number grows linearly with the messages (a 4 GiB pcap: about ×1.9), except the MF4 open before P4,
which grew with messages × blocks.

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
