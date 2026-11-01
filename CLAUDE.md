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
cargo deny check                             # dependency licences / advisories (deny.toml)
cargo run -p nerka-cli -- filter 'level >= ERROR AND raw ~ /SSL/' app.log
cargo run -p nerka-gui -- app.log --query 'level >= ERROR'
cargo run -p nerka-cli -- --signal 'v=/v=(?<v>\d+)/' filter '@v > 100' app.log   # signals (@name)
cargo run -p nerka-cli -- --fibex ecu1.xml filter 'apid == NAV' trace.dlt        # DLT non-verbose via FIBEX
cargo run -p nerka-cli -- --someip-port 30501 filter 'proto == SOMEIP AND type == ERROR' trace.pcap   # protocols by UDP port
cargo run -p nerka-cli -- --records 'start=/\d{4}-\d\d-\d\d/' filter 'level >= ERROR' app.log   # multi-line records
cargo run -p nerka-cli -- --pack packs/ expand -l ota -p cam=CAM1 --parts   # pack picks -> NFD
cargo run -p nerka-cli -- --pack packs/ search --in 'level >= WARN' -l ota app.log
cargo run --release -p nerka-core --example gen_log -- big.log 1024   # 1 GiB test log
cargo run --release -p nerka-cli -- search -c --stats ERROR big.log  # benchmark
cargo run --release -p nerka-core --example gen_dlt -- big.dlt 1024   # 1 GiB DLT test file
cargo run --release -p nerka-cli -- filter -c --stats 'apid == NAV AND level >= WARN' big.dlt
cargo run --release -p nerka-core --example dlt_to_pcap -- big.dlt big.pcap   # the same DLT as a pcap (VLAN, UDP, fragments)
cargo run --release -p nerka-cli -- filter -c --stats 'apid == NAV AND level >= WARN' big.pcap
cargo run --release -p nerka-core --example dlt_to_mf4 -- big.dlt big_z.mf4 --deflate   # the same DLT as an MF4 (DZ blocks; --one-block, or no --deflate)
cargo run --release -p nerka-cli -- --unpack-dir cache filter -c --stats 'apid == NAV AND level >= WARN' big_z.mf4  # 1st run unpacks, then cached
cargo run --release -p nerka-core --example probe_capture -- trace.pcapng log.mf4   # structure of real captures, no payload
cargo run -p nerka-core --example gen_captures -- target/samples   # syslog.pcap, dlt-someip.pcap (SOME/IP services on 30501)
powershell -ExecutionPolicy Bypass -File tools/screenshot.ps1 -Out shot.png -AppArgs app.log  # GUI screenshot (Windows)
```

CI (`.github/workflows/ci.yml`) runs fmt, clippy and tests on Linux and Windows, `cargo deny`, and a DCO sign-off check on pull requests (`CONTRIBUTING.md`; commit with `git commit -s`); `release.yml`
builds Linux/Windows archives and publishes a pre-release on a `v*` tag.

`doc/status.md` records the current state and the owner's GUI decisions; read it
before changing the UI, and update it when the state changes.

## Rules
- Line numbers are 0-based `LineNo` (u64) internally; add 1 only for display.
- Never lose or modify the original line (`raw`); normalized fields are lazy.
- New input formats implement `input::LogSource`; the engine must not care about formats.
  A new protocol inside captures implements `input::proto::Protocol` and gets
  a UDP port list in `CaptureOptions`; DLT is one of them.
- Long operations take `&Progress` and check `is_cancelled()` per chunk.
- Search changes: every engine test runs the fast path against the naive
  per-line path (`run_filter` in `engine/search.rs`). Keep that invariant and
  add a test for every bug.
- DSL changes: update the grammar comment in `dsl/mod.rs` and add parser tests,
  including error positions.
- Performance-sensitive changes: report before/after numbers from the `--stats` benchmark above.
- Keep dependencies few and justified in the PR description (spec §3.1).
