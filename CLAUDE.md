# Working on Nerka (guide for AI agents and contributors)

Read `doc/nerka_architecture_implementation_spec_v_0.md` (requirements) and
`doc/architecture-analysis.md` (decisions, deviations, roadmap) first.

## Layout
- `core/` — `nerka-core`, the UI-free engine. **All** filtering, parsing and
  format logic lives here.
- `cli/` — `nerka` binary. Thin; prints results from core.
- `ui/` — `nerka-gui` (egui). Displays results and sends query strings; must not filter or parse logs itself (spec §4).

## Commands
```
cargo test --workspace                       # all tests
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all
cargo run -p nerka-cli -- filter 'level >= ERROR AND raw ~ /SSL/' app.log
cargo run -p nerka-gui -- app.log --query 'level >= ERROR'
cargo run --release -p nerka-core --example gen_log -- big.log 1024   # 1 GiB test log
cargo run --release -p nerka-cli -- search -c --stats ERROR big.log  # benchmark
powershell -ExecutionPolicy Bypass -File tools/screenshot.ps1 -Out shot.png -AppArgs app.log  # GUI screenshot (Windows)
```

`doc/status.md` records the current state and the owner's GUI decisions; read it
before changing the UI, and update it when the state changes.

## Rules
- Line numbers are 0-based `LineNo` (u64) internally; add 1 only for display.
- Never lose or modify the original line (`raw`); normalized fields are lazy.
- New input formats implement `input::LogSource`; the engine must not care about formats.
- Long operations take `&Progress` and check `is_cancelled()` per chunk.
- Search changes: every engine test runs the fast path against the naive
  per-line path (`run_filter` in `engine/search.rs`). Keep that invariant and
  add a test for every bug.
- DSL changes: update the grammar comment in `dsl/mod.rs` and add parser tests,
  including error positions.
- Performance-sensitive changes: report before/after numbers from the `--stats` benchmark above.
- Keep dependencies few and justified in the PR description (spec §3.1).
