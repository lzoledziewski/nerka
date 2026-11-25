//! `nerka mcp`: an MCP server over stdio (newline-delimited JSON-RPC 2.0,
//! logs on stderr) that forwards an agent's tool calls to a nerka window
//! (`docs/dev/ai.md`, workflow B). It opens no files of its own (owner,
//! 2026-11-24): the window does the work, on the tabs the user has open.
//!
//! The window is the one in `--window PID` or `NERKA_WINDOW` (set by the
//! window's 🤖 AI terminal), else the one focused last. The link is made at
//! the first call and made again if the window went away.

use anyhow::Result;
use nerka_core::json::{self, Value};
use nerka_core::link::AgentLink;
use std::io::{BufRead, Write};

/// Protocol versions this server speaks, newest first.
const VERSIONS: &[&str] = &["2025-11-25", "2025-06-18", "2025-03-26", "2024-11-05"];

const INSTRUCTIONS: &str = "nerka is a log viewer; these tools work on the files the user has open in \
its window (the tabs), and change what the user sees there. Lines are numbered from 1 in their own file; \
`time` is UTC (RFC 3339), while `raw` is the line as the window shows it (its time column may be local time). Queries are NFD (help gives the grammar): `level >= ERROR AND raw ~ /timeout/i`. \
Start with window, then summary; count before filter; read in pages (max, skip). patterns --rare and \
compare find what stands out. Leave findings with bookmark (they show as the agent's annotations, the user \
accepts them), show the user a line with show, a signal with plot, and look at the result with screenshot. \
Every change is listed in the window's Agent panel and can be undone. For files that aren't open, use the \
nerka command line (`nerka help agent`).";

/// A tool: its name, what it does, and its arguments' JSON schema.
fn tools() -> Value {
    let p = |t: &str, d: &str| Value::object([("type", Value::from(t)), ("description", d.into())]);
    let tab = || p("string", "The tab: its number from 1 (as a string) or part of its title; default: the active tab");
    let file = || {
        p(
            "string",
            "A file of the tab (name or part of it), for line numbers in a tab of several files; default: the first",
        )
    };
    let query = |d: &str| p("string", d);
    let schema = |props: Vec<(&str, Value)>, required: &[&str]| {
        let mut v = Value::object([("type", Value::from("object"))]);
        v.push("properties", Value::object(props));
        if !required.is_empty() {
            v.push("required", required.iter().map(|r| Value::from(*r)).collect::<Vec<_>>());
        }
        v
    };
    let in_view = || p("boolean", "Only among the lines the user's filter shows in the tab");
    let list = [
        (
            "window",
            "The window's tabs: files, lines, the user's filter and search, how many lines the filter shows, bookmarks, plotted signals, the selected line. Call first.",
            schema(vec![], &[]),
        ),
        (
            "summary",
            "What a tab's files are: format, lines, fields (name, kind, costly), first and last time, lines per level, notes (what the reader skipped or dropped).",
            schema(vec![("tab", tab())], &[]),
        ),
        ("help", "The NFD query grammar, the tab's fields and its signals.", schema(vec![("tab", tab())], &[])),
        (
            "count",
            "How many lines an NFD query keeps (no query: all lines). Cheap: call before filter.",
            schema(vec![("tab", tab()), ("query", query("NFD, e.g. `level >= ERROR`")), ("in_view", in_view())], &[]),
        ),
        (
            "filter",
            "The lines an NFD query keeps, a page at a time: {total, lines: [{file, line, time, level, raw}], more, skip}. With before / after (durations: 30s, 500ms): also the lines around each match, by time, matches marked.",
            schema(
                vec![
                    ("tab", tab()),
                    ("query", query("NFD")),
                    ("max", p("integer", "Lines at most (default 50, at most 1000)")),
                    ("skip", p("integer", "Lines left out first: the next page")),
                    ("fields", p("string", "Fields to add, typed: `apid,ctid`")),
                    ("before", p("string", "Also lines up to this long before each match")),
                    ("after", p("string", "Also lines up to this long after each match")),
                    ("in_view", in_view()),
                ],
                &["query"],
            ),
        ),
        (
            "lines",
            "Lines around a line (line, before, after: counts) or a range (from, to).",
            schema(
                vec![
                    ("tab", tab()),
                    ("file", file()),
                    ("line", p("integer", "The line, from 1")),
                    ("before", p("integer", "Lines before (default 10)")),
                    ("after", p("integer", "Lines after (default 10)")),
                    ("from", p("integer", "First line of a range")),
                    ("to", p("integer", "Last line of a range")),
                    ("fields", p("string", "Fields to add: `apid,ctid`")),
                ],
                &[],
            ),
        ),
        (
            "patterns",
            "Message patterns (templates with <*> where values differ), most lines first or the rarest (rare), each with its count, first line and an NFD query for its lines.",
            schema(
                vec![
                    ("tab", tab()),
                    ("in", query("Only lines this NFD query keeps")),
                    ("rare", p("boolean", "The rarest first")),
                    ("top", p("integer", "Patterns at most (default 30, 0: all)")),
                    ("values", p("boolean", "Each slot's values")),
                    ("lines", p("integer", "Each pattern's first N lines")),
                    ("in_view", in_view()),
                ],
                &[],
            ),
        ),
        (
            "compare",
            "What differs between two sets of lines of a tab (two NFD queries, e.g. two lifecycles, before and after): patterns and values new in B, gone from B, more or less often, scored.",
            schema(
                vec![
                    ("tab", tab()),
                    ("a", query("NFD for set A")),
                    ("b", query("NFD for set B")),
                    ("in", query("Both sets: only lines this keeps")),
                    ("top", p("integer", "Differences at most (default 30)")),
                    ("min_score", p("number", "Changes at least this sure (default 10.8)")),
                ],
                &["a", "b"],
            ),
        ),
        (
            "bookmarks",
            "The tab's bookmarks: line, name, note, author (null: the user's; else an agent's annotation).",
            schema(vec![("tab", tab())], &[]),
        ),
        (
            "signals",
            "The tab's signals (values taken from lines: @speed, lifecycles, FIBEX arguments) and which are plotted.",
            schema(vec![("tab", tab())], &[]),
        ),
        (
            "signal",
            "A signal's values: events, first, last, min, max (numbers), states and changes (enums), and points spread over a line range.",
            schema(
                vec![
                    ("tab", tab()),
                    ("name", p("string", "The signal (signals lists them)")),
                    ("file", file()),
                    ("from", p("integer", "First line")),
                    ("to", p("integer", "Last line")),
                    ("points", p("integer", "Points at most (default 100)")),
                ],
                &["name"],
            ),
        ),
        (
            "lifecycles",
            "The tab's lifecycles (an ECU's boots, a service's runs): each run's lines, start, end, how it ended, and findings.",
            schema(vec![("tab", tab()), ("name", p("string", "One lifecycle"))], &[]),
        ),
        (
            "show",
            "Show the user a line: the window goes to it and selects it.",
            schema(vec![("tab", tab()), ("file", file()), ("line", p("integer", "The line, from 1"))], &["line"]),
        ),
        (
            "set_filter",
            "Set the tab's Filter box to an NFD query (empty: none): the user sees only those lines. Undoable.",
            schema(vec![("tab", tab()), ("query", query("NFD; empty clears the filter"))], &["query"]),
        ),
        (
            "set_search",
            "Set the tab's Search box to an NFD query: its hits are highlighted and listed. Undoable.",
            schema(vec![("tab", tab()), ("query", query("NFD; empty clears the search"))], &["query"]),
        ),
        (
            "bookmark",
            "Bookmark a line with a note: the user sees it as your annotation (🤖) and can accept it. Use for findings: the first error, the cause, the change.",
            schema(
                vec![
                    ("tab", tab()),
                    ("file", file()),
                    ("line", p("integer", "The line, from 1")),
                    ("note", p("string", "What the line shows and why it matters")),
                    ("name", p("string", "A short name (default B1, B2…)")),
                ],
                &["line", "note"],
            ),
        ),
        (
            "plot",
            "Plot signals of the tab (signals lists them) in the window's plot.",
            schema(
                vec![
                    ("tab", tab()),
                    ("signals", p("string", "Signal names, comma-separated")),
                    ("only", p("boolean", "Only these: take the other lanes out")),
                ],
                &["signals"],
            ),
        ),
        (
            "screenshot",
            "A picture of the window (of: window) or the plot (of: plot), to look at what the user sees.",
            schema(vec![("of", p("string", "window or plot"))], &[]),
        ),
    ];
    let tools = list.into_iter().map(|(name, description, input)| {
        let read_only = !matches!(name, "show" | "set_filter" | "set_search" | "bookmark" | "plot");
        Value::object([
            ("name", Value::from(name)),
            ("description", description.into()),
            ("inputSchema", input),
            ("annotations", Value::object([("readOnlyHint", Value::from(read_only)), ("openWorldHint", false.into())])),
        ])
    });
    Value::Array(tools.collect())
}

/// MCP's arguments are JSON; a window takes `tab` as a number or a title.
fn window_args(mut args: Value) -> Value {
    if let Value::Object(pairs) = &mut args {
        for (k, v) in pairs.iter_mut() {
            if k == "tab"
                && let Some(n) = v.as_str().and_then(|s| s.trim().parse::<u64>().ok())
            {
                *v = Value::from(n);
            }
        }
    }
    args
}

/// A tool call's MCP result: the window's JSON as text and as structured
/// content; a screenshot as an image; an error as an error result.
fn tool_result(tool: &str, r: Result<Value, String>) -> Value {
    match r {
        Ok(v) if tool == "screenshot" => {
            let data = v.get("data").and_then(Value::as_str).unwrap_or_default().to_owned();
            let image = Value::object([
                ("type", Value::from("image")),
                ("data", data.into()),
                ("mimeType", "image/png".into()),
            ]);
            Value::object([("content", Value::Array(vec![image])), ("isError", false.into())])
        }
        Ok(v) => {
            let text = Value::object([("type", Value::from("text")), ("text", v.to_string().into())]);
            let mut out = Value::object([("content", Value::Array(vec![text]))]);
            if matches!(v, Value::Object(_)) {
                out.push("structuredContent", v);
            }
            out.push("isError", false);
            out
        }
        Err(e) => {
            let text = Value::object([("type", Value::from("text")), ("text", e.into())]);
            Value::object([("content", Value::Array(vec![text])), ("isError", true.into())])
        }
    }
}

/// The server: JSON-RPC on stdin / stdout until stdin ends.
pub fn serve(window: Option<u32>) -> Result<()> {
    let window = window.or_else(|| std::env::var("NERKA_WINDOW").ok()?.trim().parse().ok());
    let mut link: Option<AgentLink> = None;
    let stdin = std::io::stdin();
    let mut out = std::io::stdout().lock();
    for line in stdin.lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let reply = match json::parse(&line) {
            Err(e) => Some(error(Value::Null, -32700, &format!("parse error: {e}"))),
            Ok(msg) => handle(&msg, window, &mut link),
        };
        if let Some(r) = reply {
            writeln!(out, "{r}")?;
            out.flush()?;
        }
    }
    Ok(())
}

fn error(id: Value, code: i64, msg: &str) -> Value {
    let e = Value::object([("code", Value::from(code)), ("message", msg.into())]);
    Value::object([("jsonrpc", Value::from("2.0")), ("id", id), ("error", e)])
}

fn ok(id: Value, result: Value) -> Value {
    Value::object([("jsonrpc", Value::from("2.0")), ("id", id), ("result", result)])
}

/// One message; `None` for a notification.
fn handle(msg: &Value, window: Option<u32>, link: &mut Option<AgentLink>) -> Option<Value> {
    let id = msg.get("id").cloned()?;
    let method = msg.get("method").and_then(Value::as_str).unwrap_or_default();
    let params = msg.get("params").cloned().unwrap_or_else(Value::new_object);
    Some(match method {
        "initialize" => {
            let asked = params.get("protocolVersion").and_then(Value::as_str).unwrap_or(VERSIONS[0]);
            let version = VERSIONS.iter().find(|v| **v == asked).copied().unwrap_or(VERSIONS[0]);
            let info = Value::object([("name", Value::from("nerka")), ("version", env!("CARGO_PKG_VERSION").into())]);
            let caps = Value::object([("tools", Value::object([("listChanged", Value::from(false))]))]);
            ok(
                id,
                Value::object([
                    ("protocolVersion", Value::from(version)),
                    ("capabilities", caps),
                    ("serverInfo", info),
                    ("instructions", INSTRUCTIONS.into()),
                ]),
            )
        }
        "ping" => ok(id, Value::new_object()),
        "tools/list" => ok(id, Value::object([("tools", tools())])),
        "tools/call" => {
            let Some(tool) = params.get("name").and_then(Value::as_str) else {
                return Some(error(id, -32602, "tools/call needs a name"));
            };
            let args = window_args(params.get("arguments").cloned().unwrap_or_else(Value::new_object));
            ok(id, tool_result(tool, call(window, link, tool, args)))
        }
        _ => error(id, -32601, &format!("no method {method}")),
    })
}

/// Forward a call, (re)linking to the window as needed.
fn call(window: Option<u32>, link: &mut Option<AgentLink>, tool: &str, args: Value) -> Result<Value, String> {
    for attempt in 0..2 {
        if link.is_none() {
            *link = Some(AgentLink::connect(window).map_err(|e| {
                format!(
                    "{e}: start nerka-gui with the files (nerka-gui FILE), or use the command line (nerka help agent)"
                )
            })?);
        }
        match link.as_mut().expect("linked").call(tool, args.clone()) {
            Err(e) if e.starts_with("the window went away") && attempt == 0 => *link = None,
            r => return r,
        }
    }
    Err("the window went away".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initialize_lists_and_errors() {
        let mut link = None;
        let msg = |s: &str| json::parse(s).unwrap();
        let r = handle(
            &msg(r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18"}}"#),
            None,
            &mut link,
        )
        .unwrap();
        assert_eq!(r.get("result").and_then(|r| r.get("protocolVersion")).and_then(Value::as_str), Some("2025-06-18"));
        assert!(handle(&msg(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#), None, &mut link).is_none());
        let r = handle(&msg(r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#), None, &mut link).unwrap();
        let tools = r.get("result").and_then(|r| r.get("tools")).and_then(Value::as_array).unwrap();
        assert!(tools.len() >= 15 && tools.iter().all(|t| t.get("inputSchema").is_some()));
        let r = handle(&msg(r#"{"jsonrpc":"2.0","id":3,"method":"nope"}"#), None, &mut link).unwrap();
        assert_eq!(r.get("error").and_then(|e| e.get("code")).and_then(Value::as_f64), Some(-32601.0));
    }
}
