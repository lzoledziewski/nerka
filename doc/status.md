# Status & next steps

Snapshot of where the project stands. Update this file whenever the state changes.

_Last update: 2026-10-17 (initial scaffold, generated in a claude.ai session)._

## State

- The scaffold was written and tested on **Linux only**: build, 23 tests, clippy `-D warnings`, fmt, and the UI rendered under Xvfb. It has **never been built on Windows**; that is the first thing to do.
- Nothing is committed yet. The initial commit should contain only sources: no `target/`, no `*.log`, no `*.nerka-marks` (see `.gitignore`).
- Two config files from the scaffold are **not on disk yet**, because the transfer tool couldn't write to these folders. Recreate them; the contents are in the appendix below:
  - `.github/workflows/ci.yml`
  - `.vscode/extensions.json`

## Immediate next steps (in order)

1. Recreate the two files above. Make the initial commit (sources only, **do not push** until the owner says so).
2. `cargo build --release --workspace` on Windows (MSVC). Fix anything Windows-specific.
3. `cargo test --workspace` and `cargo clippy --workspace --all-targets -- -D warnings`.
4. Smoke test: `cargo run --release -p nerka-core --example gen_log -- big.log 1024`, then `target\release\nerka filter -c --stats "level >= ERROR" big.log` and `target\release\nerka-gui big.log`. Compare the numbers with `architecture-analysis.md` §5.
5. Continue with Phase 1.5 of the roadmap (`architecture-analysis.md` §8). Follow/tail mode and highlight rules come first.

## Decisions pending with the owner

Don't settle these silently: ask.

1. **UI framework:** egui was chosen instead of the spec's Tauri, because the owner asked for "not heavy" (they originally considered Qt/C++). Not yet confirmed.
2. **Level detection:** only upper-case tokens (`ERROR`, `[WARN]`, `level=INFO`) in the first 160 bytes are recognised. This needs real log samples from the owner.
3. **Bookmark storage:** currently a sidecar `<log>.nerka-marks` next to the log. The alternative is a central app-data folder when the log's directory is read-only.

## Known limitations

- The line index uses 8 bytes per line (≈94 MB per 12 M lines); a block-compressed index is planned.
- No follow/tail mode: if a file is truncated while it is mapped, reads may fault.
- Only UTF-8/ASCII text is handled; invalid bytes are shown as U+FFFD.
- The GUI binary is ~19 MB on Linux. The size on Windows hasn't been measured.

## Appendix: files to recreate

### `.github/workflows/ci.yml`

```yaml
name: CI

on:
  push:
    branches: [main]
  pull_request:

env:
  CARGO_TERM_COLOR: always

jobs:
  check:
    name: ${{ matrix.os }}
    runs-on: ${{ matrix.os }}
    strategy:
      fail-fast: false
      matrix:
        os: [windows-latest, ubuntu-latest, macos-latest]
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
        with:
          components: rustfmt, clippy
      - uses: Swatinem/rust-cache@v2
      - name: Format
        run: cargo fmt --all --check
      - name: Clippy
        run: cargo clippy --workspace --all-targets -- -D warnings
      - name: Test
        run: cargo test --workspace
      - name: Build release
        run: cargo build --release --workspace
      - name: Upload binaries
        uses: actions/upload-artifact@v4
        with:
          name: nerka-${{ matrix.os }}
          path: |
            target/release/nerka
            target/release/nerka-gui
            target/release/nerka.exe
            target/release/nerka-gui.exe
          if-no-files-found: ignore
```

### `.vscode/extensions.json`

```json
{
  "recommendations": ["rust-lang.rust-analyzer", "tamasfe.even-better-toml", "vadimcn.vscode-lldb"]
}
```
