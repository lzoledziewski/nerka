---
title: "Signals and plots"
description: "Signals and plots in nerka: values captured from log lines with regex groups, events and states, time signals, and plots with cursors, markers and bookmarks."
---

# Signals and plots

A **signal** is a named value that lines carry: a speed, a gear, a state, a
latency, the time. It is taken from the lines with a regex's named group,
used in queries as `@name`, and plotted over time.

## Defining a signal

```
speed=/speed=(?<speed>\d+)/
gear(g):enum=apid == NAV AND payload ~ /gear (?<g>\w)/
t:time=/^(?<t>\S+ \S+)/
ota(s):enum:per(ecu1):default(idle)=ecu == ECU1 AND /OTA state=(?<s>\w+)/
```

`NAME[(GROUP)][:PART]…=QUERY`: a line carries a value when the whole
**query** matches it and the regex group captured something. The group
defaults to the signal's name. Parts:

| Part | |
|---|---|
| `num` (default), `enum`, `time` | the kind: a number (decimal, float, `0x` hex), a text value, or a time |
| `all` (default), `filtered` | where events come from: every line, or only the lines the filter keeps (the panel and plot; a filter can't use such a signal) |
| `per(LIFECYCLE)` | reset at each start of a [lifecycle](lifecycles.md) instead of each file's start |
| `default(VALUE)` | the value before the first event (per lifecycle with `per`) |

`off ` in front keeps a saved signal but doesn't compute it.

**Where signals come from:**

- signals you define: the 📈 **Signals** panel's form (saved for every tab
  in `%APPDATA%\nerka\signals`), `--signal SPEC` (CLI; GUI: for the session);
- the **named groups of the queries in the selection** (Include or Signal
  role, [Query packs](packs.md)): their kind is guessed from the first
  values, a pack can pin it (`signals: t:time`), the panel can change it;
- the file: `@time` for files that know their lines' times (DLT,
  captures);
- [lifecycles](lifecycles.md) (`@ecu1`, the lifecycle's number) and
  [checks](checks.md) (`@storm`, a count of what the check found).

## Events and states

A line that carries a value is an **event**; the **state** is the value the
signal holds from one event until the next (already the new value on the
event's line). In a view of several files, a state doesn't carry over into
the next file (unless the signal is `per` a lifecycle).

```
@speed > 100            # state
@gear.event == D        # a line that carries D
@gear.changed           # a line where the value changed
@time >= "2025-10-18 10:00:00"
@t < "00:00:30" AND level >= ERROR
```

A `time` signal reads dates in many forms (`2025-10-17 10:00:01`,
`17 Oct 2025`, `Oct 17, 2025`, `17/Oct/2025:10:00:01 +0200`, `20251017T100001`),
times of day (`10:00:01,250`, `9:05 PM`; a day is added at each midnight),
zones (`Z`, `UTC`, `+02:00`) and plain numbers (seconds since 1970, or ms,
µs, ns). It is the clock lines are plotted at in a text file.

## The Signals panel

📈 **Signals** lists the tab's signals with their kind and their state on
the current line: `• event` when the line itself carries it, else `↑ N`
(the line of the event that set it; click to go there). ✎ edits, − removes,
the checkbox switches a saved signal on or off, **New signal or lifecycle**
adds one. The line window (`Enter`) lists the line's own signals.

## Plots

Tick a signal's plot box (or `--plot NAME`) to plot it under the main view:
one **lane** per signal on the time axis. A number is a step line, an enum
coloured bands, and an **events** box ticks where values arrived. The
current line is a cursor.

- **Navigate:** the wheel zooms, drag pans, double-click shows all; hover
  shows a value; click goes to the line.
- **Time cursors:** right-click → place cursor A, B, …; Δt between them is
  shown (like a logic analyser). Drag a cursor to move it.
- **Value markers:** right-click a number lane's name → add a horizontal line
  at a value (a limit, a threshold); the same menu sets the y axis.
- **Bookmarks** show as red lines with their notes and take part in Δt; the
  plot's menu bookmarks the nearest event with a note.
- **Kept plots:** ⋮ → **Keep this plot as…** (this session, `•`) or **Save**;
  the list at the left switches between plots; a saved plot opens on other
  files with all its lanes. Changes to a kept plot are kept only with
  **Keep changes** / **Save**.
- **⧉ Own window** moves the plot into a window of its own
  (`--plot-window`); 📷 in its header copies it as an image.
- [Lifecycles](lifecycles.md) show as labelled spans on the plot.

```
nerka-gui app.log --signal 't:time=/^(?<t>\S+ \S+)/' --signal 'lat=/latency_us=(?<lat>\d+)/' --plot lat
nerka --signal 'lat=/latency_us=(?<lat>\d+)/' filter '@lat > 9000' app.log
```

## FIBEX arguments

A DLT non-verbose message described by FIBEX has named arguments; they are
signals too (the line window lists them), and can be plotted
(`--plot NAME` with `--fibex FILE`).
