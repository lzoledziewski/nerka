# Nerka — Architecture & Implementation Spec (v0.1)

> The original requirements, kept as written. Where the code deviates (egui instead of Tauri, memory-mapped index instead of pure streaming, …) and why: `architecture-analysis.md`.

## Purpose
This document defines the initial architecture, constraints, and implementation plan for **Nerka**, a lightweight, modular log analysis tool focused on fast troubleshooting and root-cause analysis.

It is designed to be:
- actionable for developers
- usable by AI agents
- extensible without rewrites

---

# 1. Core Principles

1. Local-first (no mandatory backend)
2. Modular, not monolithic
3. Human-first (AI assists, not replaces)
4. Streaming-first (handle large logs)
5. DSL-driven filtering
6. Single binary core

---

# 2. High-Level Architecture

```
UI (Tauri / CLI)
    ↓ commands
Rust Core Engine
    ↓ processing
Input Adapters → Stream → Filter Engine → Output
```

---

# 3. Components

## 3.1 Core Engine (Rust)

Responsibilities:
- File streaming
- DSL parsing
- Filter execution
- Regex matching
- Event normalization
- Result streaming

Constraints:
- Must compile to single binary
- No heavy dependencies
- Deterministic behavior

---

## 3.2 Input Layer

Supported inputs (v1):
- Plain text files
- Rotated logs

Future:
- PCAP
- DLT
- STDIN streams

Interface:

```rust
trait InputAdapter {
    fn open(path: &str) -> Stream<Event>;
}
```

---

## 3.3 Event Model

```json
{
  "timestamp": "optional",
  "level": "optional",
  "service": "optional",
  "raw": "string",
  "source": "file"
}
```

Rules:
- raw is mandatory
- parsing is optional
- never lose original line

---

## 3.4 Filter DSL (NFD)

Example:

```
level >= ERROR
AND raw ~ /SSL|CERT/
AND NOT raw ~ /healthcheck/
```

Supported v1:
- ==
- >= (levels)
- regex (~)
- AND
- NOT

---

## 3.5 DSL Execution

Pipeline:

```
Event → Evaluate conditions → Pass/Fail → Stream result
```

Performance rules:
1. Cheap checks first
2. Regex last
3. Stream, do not batch

---

## 3.6 Core API (Tauri Commands)

### open_file
```
input: path
output: success
```

### apply_filter
```
input: DSL string
output: streamed results
```

### add_bookmark
```
input: line_id
output: success
```

---

## 3.7 Event Streaming

Rust → UI events:

- log_chunk
- filtered_chunk
- stats_update

Example:

```json
{
  "type": "log_chunk",
  "lines": ["..."]
}
```

---

# 4. UI Layer (Tauri)

Responsibilities:
- Display logs
- Accept DSL input
- Highlight matches
- Manage bookmarks

Must NOT:
- implement filtering logic
- parse logs deeply

---

# 5. CLI Interface

Example:

```
nerka open app.log
nerka filter 'level >= ERROR AND raw ~ /SSL/'
```

CLI uses same core engine.

---

# 6. Data Flow

```
User → UI → invoke(command)
Rust → process → emit(events)
UI → render
```

---

# 7. Phase 1 Scope (MVP)

Must implement:
- open file
- stream lines
- DSL filtering
- regex highlighting
- bookmarks

Nice to have:
- saved filters
- simple stats

---

# 8. Non-Goals (v1)

- AI features
- distributed processing
- collaboration backend
- advanced parsing

---

# 9. Future Extensions

- pattern detection
- anomaly detection
- filter suggestions
- shared annotations

---

# 10. Repository Structure

```
nerka/
  core/
    src/
      dsl/
      engine/
      input/
  cli/
  ui/
  docs/
```

---

# 11. Development Guidelines

- Keep functions pure where possible
- Avoid hidden state
- Prefer streaming over buffering
- Log internal decisions for debugging

---

# 12. Definition of Done (Phase 1)

- Can open 1GB log file
- Can filter with DSL under 100ms latency (per chunk)
- UI remains responsive
- Works on Windows

---

# 13. Open Questions

- DSL grammar format (PEG vs handwritten)
- Indexing strategy
- UI framework final choice details

---

# End of Spec

