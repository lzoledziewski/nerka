---
title: "AI agents"
description: "Let an AI agent of your choice analyse logs and traces with nerka: on its own with the command line (JSON output in pages, a guide, a skill file), or with you on the files open in nerka-gui through MCP (nerka mcp), its findings left as annotations you accept."
---

# AI agents

nerka ships no model and needs no account: the agent is yours (Claude Code,
GitHub Copilot CLI, Codex, Gemini CLI, or any other). It works with nerka in two ways:

- **On its own, with the command line:** it runs `nerka` as you would, on
  any file, and leaves its findings as bookmarks for you to look at later.
- **With you, on your window ([MCP](#with-you-on-your-window-mcp)):** it
  works on the files you have open in nerka-gui, sees your filter and
  selection, and shows you what it found there: a bookmark, a filter, a
  plot. Every change it makes is listed, and you can undo it.

## What the agent reads first

```
nerka help agent
```

prints a guide written for an agent, followed by your library's queries with their notes (what you know about your logs): what to run first (`open --json`),
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

They are saved next to the file (`trace.dlt.nerka-marks`), like yours, as
**annotations**: the agent's, until you accept them. nerka-gui shows them
with 🤖 and in violet, where your bookmarks have 🔖 and orange: the note
above the line, the mark beside it, the plot's cursors (with a 🤖 checkbox
of their own next to 🔖), the 🔖 Bookmarks panel and its button
(`Bookmarks (3 · 🤖 2)`). Go through them with `b` / `B`; in the panel,
**✔** accepts one (it becomes your bookmark, name and note kept) and **✔
Accept all** accepts them all. A window that already has the file open
takes them in when it gets the focus back (the status bar says how many);
your bookmarks and the agent's are kept together, and where both of you
changed the same one, yours stays.

🤖 comes from your system's emoji font (Segoe UI Emoji on Windows, Apple
Color Emoji on macOS, Noto Color Emoji on most Linux desktops); without one
it shows as a box, and the violet still tells annotations apart.

`nerka marks FILE` lists them all, `--remove LINE` removes one, and
`--by NAME` says who made one (`--by ''`: yours).

## With you, on your window (MCP)

`nerka mcp` is an [MCP](https://modelcontextprotocol.io) server: an AI
client starts it, and its tools work on a nerka window, the one you
started the agent from, else the one you used last. It opens no files of
its own: open the files in nerka-gui first.

**🤖 AI** in the window's top bar opens a terminal (Windows Terminal or a
console window, your desktop's terminal, Terminal on macOS) in the active
file's folder, running your AI command (Settings → **AI command**: Claude
Code, GitHub Copilot CLI, Codex, Gemini CLI or your own; `claude` by
default), set up to work on this window. Claude Code and Copilot get
`nerka mcp` at once (`--mcp-config`, `--additional-mcp-config`); other
clients need it registered once. The command line must be installed:
nerka looks for it first and says how to install it if it isn't (the
Claude desktop app has none on PATH: install Claude Code's command line,
or in the app add `nerka mcp` as an MCP server with the JSON of
`nerka mcp --config`).

```
nerka mcp --install codex        # or claude, gemini: runs the client's own `mcp add`
nerka mcp --config               # the JSON to put in any other client's settings
```

The agent's tools:

| Tool | What it does |
|---|---|
| `window` | your tabs: files, lines, your filter and search, how many lines the filter shows, bookmarks, plotted signals, the selected line |
| `summary`, `help` | what a tab's files are (format, fields, time span, levels, what was skipped); the query language and the tab's fields and signals |
| `count`, `filter`, `lines` | lines by an NFD query, in pages, also only among the lines your filter shows (`in_view`) and around matches by time; a range or the lines around one; each line's text cut at 500 characters (`width`) |
| `patterns`, `compare` | message patterns (the rarest first), and what differs between two sets of lines (`show: same`: also what stayed the same) |
| `queries` | your queries: those picked in the tab's Library (each with its NFD and how many lines it matches) and the library's saved queries with their notes; read-only |
| `bookmarks`, `signals`, `signal`, `lifecycles` | your bookmarks; the tab's signals and a signal's values; its lifecycles |
| `show` | goes to a line in your window |
| `set_filter`, `set_search` | puts a query in the Filter or Search box |
| `set_records` | reads the tab as records (a stack of a dump, an entry over several lines each), as **Records…** does; `summary` hints when that helps |
| `bookmark` | bookmarks a line with a note: an annotation (🤖) you accept or not |
| `plot` | plots signals of the tab |
| `screenshot` | a picture of the window or the plot, for the agent to look at |

What the agent changed is listed in **🤖 Agent** in the top bar (it shows
once the agent has changed something), newest first, each with **↶
Undo**; the status bar says each change as it happens. Its bookmarks are
annotations, as above.

## Pictures

```
nerka-gui trace.dlt --fibex ecu.xml --plot speed --plot temp --screenshot plot.png
```

opens the window, plots the signals, saves the window as a PNG and closes:
the agent can look at the plot it made, or hand it to you.
