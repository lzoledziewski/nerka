# AI preparation (design settled; A1 built)

Status: **settled** with the owner (2026-11-24, "Settled" below). Built:
step A1 (2026-11-24, branch `claude/ai-prep`: `core/src/json.rs`,
`core/src/agent.rs`, the CLI; user guide `docs/user/cli.md#for-scripts-and-agents`).
Next: step A2.

## What the owner asked for (2026-11-24)

- An **MCP server**.
- **Ease of use over the CLI** and **structured output**.
- A **terminal with an AI of the user's choice**, working **on the same
  session** as the user: the AI draws conclusions from what is open,
  **bookmarks and annotates** its findings, and can **use the plot** to show
  the problem.
- **Two workflows** (owner, 2026-11-24): **(A)** the AI uses the `nerka`
  CLI to do an analysis; **(B)** assisted analysis, where MCP comes in.

Spec §1.3 still holds: human first, the AI assists. Everything the AI does in
a window is visible there, and the user can undo it.

## Principle

```
 (A) the AI does the analysis          (B) assisted analysis
 ─────────────────────────────         ─────────────────────────────
 AI CLI (Claude Code, Codex, …)        AI CLI in a terminal opened
   │ shell                             │ from the window (🤖)
   ▼                                   │ MCP, JSON-RPC over stdio
 nerka <cmd> --json                    ▼
   │ one shot, files of its own        nerka mcp   (a thin bridge)
   ▼                                   │ loopback channel of instance.rs
 results; bookmarks in the             ▼
 sidecar, plots as PNGs                nerka-gui window: the user's tabs,
                                       filter, bookmarks, plot
```

- **The core does the work** (spec §4): each operation is a core function
  that returns a structured result (`core/src/agent/`, proposed). The CLI's
  `--json` (A) and the window's MCP handlers (B) only serialise it. An
  answer the AI got in B can be reproduced with `nerka … --json`, and the
  other way round.
- **A needs no MCP:** an AI that can run commands uses the CLI, as a person
  would. It is also how the AI works on files the user hasn't opened, on a
  machine without a desktop, or in CI.
- **B is MCP, and always on a window:** the AI reads the tabs the user has
  open (the same index, filter, signals, lifecycles) and changes them.
  `nerka mcp` doesn't open files of its own; it forwards calls to a window.
- **The AI of the user's choice:** nerka ships no model, keys or provider.
  A works with any AI that runs commands, B with any MCP client.

## Workflow A: the AI does the analysis with the CLI

Review R10 (2026-11-09) holds the details, taken over with the changes
below (bounded output: one version of each, with what 0.3.0-alpha.2
already has):

- `nerka open --json FILE`: format, size, lines, columns, fields with kind
  and cost, first and last time, level counts, protocols, notes.
- `--json` on `filter`, `search`, `lines`, `marks`, `patterns`, `compare`
  (already on `check`, `lifecycles`): JSON Lines, one object per line
  (`line` 1-based for display, `file`, `time`, `level`, `raw`; with
  `--fields a,b` those fields, typed).
- **Bounded output** (owner, 2026-11-24: where nerka has it already, that
  one version, no second):
  - **Counts first:** `-c` (exists, `OutputArgs`), so an AI checks a
    query's size before reading its lines.
  - **At most N:** `-m N` (new, in `OutputArgs`, so on every command that
    prints lines).
  - **Paging:** `--skip N` (new). Not `--offset`: that is the global
    "move a file's times" option.
  - **Context: by time only,** as built in 0.3.0-alpha.2: NFD
    `before(DUR, X)`, `after(DUR, X)`, `around(DUR, DUR, X)` and
    `filter --before DUR --after DUR` (matches marked `>`, `--` between
    windows). R10's `-A / -B / -C N` (lines) is dropped. In JSON a
    context line has `"match": false`. A source without times has no
    context window (an error naming it); lines around a line number come
    from `nerka lines FILE FROM TO`, which exists.
- `nerka help nfd`: the grammar from `dsl/mod.rs` (one `include_str!`
  source so it can't drift) plus examples per source kind; `nerka help
  fields FILE` = the fields of `open --json`.
- stdin (`-`) for text.
- **Exit status: grep's** (owner, 2026-11-24), the default: 0 lines
  matched, 1 none, 2 an error. A breaking change while at 0.x: the user
  guide's CLI page and the release notes say so;
  `cli/tests/cli.rs::no_match_is_success_with_no_output` becomes a test of
  exit 1.

Added for the AI, beyond R10:

- **`nerka help agent`**: how to analyse with nerka in one read (open →
  counts by level → `patterns --rare` → filter around what stands out →
  lifecycles and checks → bookmark the findings), with each command's JSON
  shape. It ships as a skill file too (`SKILL.md` for clients that read
  skills), generated from the same text.
- **Findings left for the user:** `nerka marks --add LINE --note TEXT FILE`
  writes the sidecar `.nerka-marks` with the AI as author, so the bookmarks
  are there when the user opens the file in the GUI.
- **Pictures:** `nerka-gui FILE --plot … --screenshot shot.png` exists
  already. The AI can make a plot and look at it without a window open.
- **Times** in JSON are UTC with offset (RFC 3339) whatever the local-time
  switch says.
- **Shared code:** the JSON value and writer in core (`core/src/json.rs`,
  next to the log-record reader `input/json.rs`), and each command's work as
  a core operation, not in `cli/src/main.rs` (1,740 lines).

## Workflow B: assisted analysis over MCP

### The bridge

`nerka mcp`, a subcommand of the CLI, speaks MCP over **stdio** (newline-delimited JSON-RPC 2.0, logs
on stderr): `initialize`, `tools/list`, `tools/call`,
`notifications/progress` (fed from the window's `Progress`),
`notifications/cancelled` (mapped to `Progress::cancel`). No resources or
prompts at first.

It connects to a window through `instance.rs`'s loopback port and token,
the way `nerka-gui FILE` hands files over today: the window in
`NERKA_WINDOW` (set by 🤖 AI terminal), else the window focused last; with
none open, each tool says so. A new request kind keeps the connection
open, with one JSON operation a line, answered by the window. The work runs
in the window's process, on its open tabs.

### Tools

Each answer has `structuredContent` plus a short text form.

| Tool | What it does |
|---|---|
| `window` | Tabs, the active one, its filter, search, selection, bookmarks, plot, the line selected |
| `help` | NFD grammar, a tab's fields, examples |
| `count` | Lines a filter matches in a tab, the boxes left as they are |
| `filter` | Lines a filter matches: `max`, `offset`, `fields`, context |
| `lines` | A line range, or lines around one (the user's selected line by default) |
| `patterns` / `compare` | As the CLI's, on a tab or two lifecycles |
| `lifecycles` / `check` | As the CLI's, with the tab's lifecycles and checks |
| `signal` | A signal's values in a range: count, min, max, changes, a thinned series |
| `show` | Go to a line in the window (scrolls and selects it) |
| `set_filter` / `set_search` | Put NFD in the tab's boxes, as if typed |
| `bookmark` | Add or edit a bookmark with a note, the AI as author |
| `plot` | Open the plot with signals, a time range, markers and cursors |
| `screenshot` | The window or the plot as a PNG, returned as an MCP image |

`count`, `filter`, `lines` and the rest read without changing the window;
`show`, `set_*`, `bookmark` and `plot` change it. `screenshot` lets the AI
see the plot it made and check it shows the problem (`shot.rs` already
captures the window and the plot).

**Context budget.** Every answer is bounded: `max` lines (default 100),
`raw` cut at a length with `cut: true`, and a `more` with the next offset.
A tool that would go over the budget says so instead of cutting silently
(review 2026-11-09's theme: no silence).

**Visible and undoable** (owner, 2026-11-24): the AI's changes are applied
at once, not proposed for confirmation. Each one is listed in the window (a
status line and an activity list), bookmarks it made are marked as the
AI's, and the user can undo them.

### The AI terminal

**Settled (owner, 2026-11-24): an external terminal.** A toolbar entry
**🤖 AI terminal** opens the user's own terminal (Windows Terminal or `cmd`
on Windows, `$TERMINAL` / the desktop's terminal on Linux, Terminal on
macOS) running the AI command the user chose in Settings (`claude`,
`codex`, `gemini`, …), in the folder of the active tab's file, with
`NERKA_WINDOW=<pid>` set. With `nerka mcp` registered in that client, the
AI is on this window at once; the same AI can use the CLI too (workflow A)
for files the window doesn't have open. nerka writes the client's MCP
configuration when asked (`nerka mcp --install claude`), or the user guide
gives a snippet to copy.

No new dependency, and any AI CLI works. Not chosen: a terminal pane
inside the window (a PTY plus a terminal emulator, several dependencies,
ConPTY on Windows) and a built-in chat calling a provider's API (keys and a
provider in nerka).

## Order

| Step | Gives | Depends on |
|---|---|---|
| A1 | `--json`, `-m`, `--skip`, `help nfd`, exit status (R10) | — |
| A2 | `help agent` / skill file, `marks --add` with author | A1, the marks format |
| B1 | `nerka mcp` and the window's read tools | A1 (operations, JSON) |
| B2 | The window's changing tools, activity list, undo, screenshot | B1 |
| B3 | 🤖 AI terminal, `nerka mcp --install` | B1 |

Each step is a pre-release on its own branch. A1 closes R10; B1 gets the
window's handlers a module of their own (`ui/src/agent.rs`), not more lines
in `ui/src/main.rs` (6,218 non-test lines).

## Settled (owner, 2026-11-24)

- **Two workflows:** the CLI for an analysis the AI does (A), MCP for
  assisted analysis on the user's window (B).
- **JSON: hand-written** in core (`core/src/json.rs`): a value, a parser
  and a writer, no `serde` (spec §3.1: no heavy dependencies), fuzzed by
  the review gate. The CLI's `--json` and the MCP bridge use the same one.
- **Bounded output: one version of each** (A1): `-c`, `-m N`, `--skip N`,
  and context by time with what 0.3.0-alpha.2 built (`before` / `after` /
  `around`, `filter --before / --after`); no line-count context.
- **Exit status:** grep's 0 / 1 / 2 by default (A1).
- **The bridge: stdio, in the CLI.** The AI client starts `nerka mcp`
  (the same configuration for every window and machine); it reaches the
  window through `instance.rs`'s loopback port and token. Not chosen: MCP
  over HTTP served by the window (a port per window, an HTTP server in the
  GUI, an Origin check and a token against other local processes and
  browser pages), a bridge in `nerka-gui` (no console in a Windows release
  build) or a binary of its own.
- **No MCP without a window:** `nerka mcp` doesn't open files of its own;
  with no window open, each tool says so. An AI without a window uses the
  CLI (A). Headless MCP only if a client that can't run commands needs it.
- **The terminal:** the user's own terminal, started from the window with
  the AI command chosen in Settings and `NERKA_WINDOW` set.
- **What the AI may change:** directly, listed and undoable.
- **Annotations:** bookmarks with a note, plus **who made them** (the user
  or the AI). A change of `.nerka-marks` (`line<TAB>note` today): old files
  stay readable; if the format can't be extended compatibly, a migration
  step in `bundle::STEPS` with a test on an old sample
  (`docs/dev/bundles.md`).
