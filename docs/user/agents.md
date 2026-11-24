---
title: "AI agents"
description: "Let an AI agent of your choice analyse logs and traces with the nerka command line: JSON output in pages, a guide it reads in one go, a skill file, and its findings left as bookmarks you see in nerka-gui."
---

# AI agents

An AI agent that can run commands (Claude Code, Codex, Gemini CLI, or any
other) can analyse a log or trace with the `nerka` command line, as you
would. nerka ships no model and needs no account: the agent is yours.

## What the agent reads first

```
nerka help agent
```

prints a guide written for an agent: what to run first (`open --json`),
how to count before reading (`-c`) and read in pages (`-m`, `--skip`), how
to find what stands out (`patterns`, `compare`), how to read around a
finding (`filter --before / --after`), and how to leave its findings for
you. Tell your agent to read it, or put it where your agent looks:

| Agent | |
|---|---|
| Claude Code | `nerka help skill` is the guide as a skill: save it as `~/.claude/skills/nerka/SKILL.md` (or `.claude/skills/nerka/SKILL.md` in a project) |
| Others | add the output of `nerka help agent` to the agent's instructions file (`AGENTS.md`, `GEMINI.md`, …), or ask it to run `nerka help agent` first |

```
mkdir -p ~/.claude/skills/nerka
nerka help skill > ~/.claude/skills/nerka/SKILL.md
```

Everything the agent uses is in [the command line](cli.md): `--json` on
every command that prints lines or results, pages with `-m` and `--skip`,
and grep's [exit status](cli.md#exit-status) (0 found, 1 nothing found, 2
an error).

## Its findings, in the viewer

The agent bookmarks the lines that explain the problem, each with a note:

```
nerka marks --add 1234 --note 'first watchdog reset: the bus was down 2 s before' trace.dlt
```

They are saved next to the file (`trace.dlt.nerka-marks`), like yours, and
marked as the agent's: `B3 (ai)` in the views and 🤖 in the 🔖 Bookmarks
panel. Open the file in nerka-gui and go through them with `b` / `B`. A
window that already has the file open takes them in when it gets the focus
back (the status bar says how many); your own bookmarks and its are kept
together, and where both of you changed the same one, yours stays.

`nerka marks FILE` lists them all, `--remove LINE` removes one, and
`--by NAME` says who made one (`--by ''`: yours).

## Pictures

```
nerka-gui trace.dlt --fibex ecu.xml --plot speed --plot temp --screenshot plot.png
```

opens the window, plots the signals, saves the window as a PNG and closes:
the agent can look at the plot it made, or hand it to you.
