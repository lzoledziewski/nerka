# Review checklist

One list for every review, by perspective. Each item has an ID (findings
cite it), **how to check it**, and whether it **blocks** a merge or release
(`gate`) or is looked at in the periodic review only (`cycle`). Items marked
🤖 are checked by `tools/review-gate.sh`; the rest need a reviewer, a person
or an agent following `.claude/skills/nerka-review/SKILL.md`.

Severity of a finding: **S1** wrong results, data loss, crash on input,
security; **S2** a rule of `CLAUDE.md` broken, a user-visible defect, a
regression over 15 %; **S3** debt that makes the next change harder;
**S4** polish.

## GATE — build gates (gate)

| ID | Check | How |
|---|---|---|
| GATE-1 🤖 | Formatted | `cargo fmt --all --check` |
| GATE-2 🤖 | No clippy warnings | `cargo clippy --workspace --all-targets -- -D warnings` |
| GATE-3 🤖 | Tests pass | `cargo test --workspace` |
| GATE-4 🤖 | Licences, advisories, bans; no yanked crates (warn) | `cargo deny check` |
| MSRV-1 🤖 | The declared `rust-version` builds the workspace | `cargo +<rust-version> check --workspace --all-targets` (`--full`); without that toolchain the script looks for let-chains (1.88+) |

## ARCH — architecture (cycle; ARCH-1 gate)

| ID | Check | How |
|---|---|---|
| ARCH-1 🤖 | The UI doesn't parse or filter logs (spec §4): no regex, no format logic in `ui/` | grep `ui/`; read new UI code for string splitting of log lines |
| ARCH-2 🤖 | No module grows past ~2500 non-test lines without a plan | line counts; `ui/src/main.rs` is the known case |
| ARCH-3 🤖 | `LogSource` stays small; format-specific methods go into optional capabilities | count trait methods; new methods need a reason |
| ARCH-4 🤖 | GUI state that decides *what runs* lives in `core::Session`, not in `Tab` | `Tab` field count; new reruns-on-change logic in `ui/` |
| ARCH-5 🤖 | `unsafe` only where reviewed, each with a `SAFETY:` comment | grep |
| ARCH-6 | New input formats implement `LogSource` / `Protocol`; the engine has no format `match` | read `engine/`, `input/mod.rs::open_with` |
| ARCH-7 | Every long operation takes `&Progress` and checks `is_cancelled()` per chunk | read new loops over lines / bytes |
| ARCH-8 | Public API of `nerka-core` (`lib.rs` re-exports) matches what CLI and GUI need; module docs current | read `core/src/lib.rs` header against the module list |
| ARCH-10 | A source that wraps others (`Concat`, its parts, `Merged`) answers every field as its parts do, for every layout (same kind, other fields, mixed, mixed with equal fields) | a test that compares each line's fields by name with the part's own; expected values, since both paths share the mapping |
| ARCH-9 | Decisions recorded: a change against `doc/` decisions links the owner's decision | `doc/status.md`, `doc/architecture-analysis.md` |

## CODE — correctness and robustness (gate for S1)

| ID | Check | How |
|---|---|---|
| CODE-1 🤖 | Settings, packs, presets, bookmarks are written atomically (temp file + rename), so a crash or a full disk never truncates them | grep `fs::write` outside tests |
| CODE-2 🤖 | Readers never panic on corrupt input (DLT, pcap, pcapng, MF4, JSON, FIBEX, text) | `cargo run --release -p nerka-core --example mutate_open -- SEEDS 3000 SEED` (`--full`) |
| CODE-3 | Every engine test compares the fast path with the naive path (`run_on`); a bug fix adds a test that failed before | read the diff's tests |
| CODE-4 | Empty and zero-width matches (`^$`, `x*`, `\b`) at chunk, part and record boundaries | `engine::search::tests::empty_matches_at_chunk_ends`; extend for new scan loops |
| CODE-5 | Byte offsets vs characters: error columns, carets and slicing at non-ASCII text | try a query with `ż` before the error; `&s[..pos]` must be on a char boundary |
| CODE-6 | Times: code that assumes rows are in time order says so, or handles times going back (concatenated files, clock jumps) | read windows / gaps over `time(row)` (`check.rs`, `lifecycle.rs`, plot) |
| CODE-7 | Integer casts on untrusted lengths (`as usize`, `as u32`) are checked or saturating | grep in `input/` |
| CODE-8 | No `unwrap` / `expect` on data from files or the user outside tests | grep non-test code; each needs an invariant comment |
| CODE-9 | A file changing while mapped (truncated, rotated) doesn't crash the process | known limitation (status.md); track until follow mode |

## PERF — performance (gate for regressions)

| ID | Check | How |
|---|---|---|
| PERF-1 🤖 | The `--stats` benchmark runs and is recorded, a row per case (an empty table fails) | `tools/bench.sh DIR 1024 5 > bench.tsv` (`--bench`); `KSY=1` adds the protocol definition cases |
| PERF-2 🤖 | No query more than 15 % slower than the baseline, match counts identical | `tools/bench.sh compare doc/review/baseline/bench-….tsv bench.tsv` |
| PERF-3 | Index memory per line stays as documented (8 B text, 8 / 16 B DLT, +16 B merged) | `nerka open FILE` → `index mem` |
| PERF-4 | New per-line work in the hot path (allocation, `to_string`, rendering) is avoided or measured | read `scan_*`, `Event`, `field` implementations |
| PERF-5 | GUI: no repaint loop when idle; heavy work off the UI thread | status.md "CPU with software rendering"; `ui/src/job.rs` |

## HUX — human usability (cycle)

| ID | Check | How |
|---|---|---|
| HUX-1 | CLI errors say what is wrong and where (caret under the column) and what would work | try a wrong field, a bad regex, a missing file |
| HUX-2 | CLI help: each option has an example; the `about` line still describes the tool | `nerka --help`, `nerka <cmd> --help` |
| HUX-3 | Owner's GUI decisions in `doc/status.md` are kept (Filter at the top, Search at the bottom, runs on Enter only, …) | read the diff against "GUI as of now" |
| HUX-4 | GUI change checked on screen | `tools/screenshot.ps1` |
| HUX-5 | Messages use the same words as the docs (filter / search / query / pack / signal / lifecycle / check) | read new strings |
| HUX-6 | README's quick start works from a fresh clone | follow it |
| HUX-7 | A setting the user made never loses silently to a default (rules, ports, protocols turned off) | try a user rule on a port a default rule has |
| HUX-8 | What a reader drops is reported: bytes skipped, messages not read, fields past a cap (`net.*` senders) | open a file with garbage in it, and a capture past the cap; `nerka open` and the GUI's status must say so |

## AIX — usability for AI agents and scripts (cycle)

| ID | Check | How |
|---|---|---|
| AIX-1 🤖 | Required reading for an agent fits a budget (CLAUDE.md + the docs it requires: 25k tokens) | byte counts / 4 |
| AIX-2 🤖 | Docs have short lines (≤ 1500 chars) so exact-match edits and diffs work | `awk 'length > 1500'` |
| AIX-3 🤖 | Every path and example command in `CLAUDE.md` exists and runs | DOC-1 in the script; run the examples |
| AIX-4 | CLI results are machine-readable where scripts use them: `--json` (or TSV with a header) for `filter`, `search`, `open`, `marks`, not only `check` / `lifecycles` | run them |
| AIX-5 | Exit status tells "no match" from "error" (grep: 0 / 1 / 2) where a script needs it | `nerka filter … ; echo $?` |
| AIX-6 | A file's fields can be discovered without the GUI | `nerka open FILE` lists `fields:`; kinds too? |
| AIX-7 | Status and decisions are split: current state (short) vs history (append-only) | `doc/status.md` structure |
| AIX-8 | Each file an agent must read fits one read (under 20k tokens), with what must not be missed (pending decisions) near the top | read `doc/status.md` whole with the agent's file tool |

## SEC — security (gate for S1)

| ID | Check | How |
|---|---|---|
| SEC-1 | Untrusted files: no unbounded allocation from a length field; regexes from packs have size limits | read readers; `RegexBuilder::size_limit` |
| SEC-2 | The single-instance listener: loopback only, token compared in constant time, bounded request, no command but `open` | `ui/src/instance.rs` |
| SEC-3 | Unpack cache: names derived by hashing, written to a temp name, never outside the cache folder | `core/src/input/capture/unpack.rs` |
| SEC-4 | No secrets, real logs or captures committed | `git log -p` for new binaries; `.gitignore` |
| SEC-5 | Protocol definitions are untrusted input too: bounded file size, nesting, nodes, list items; no panic on any definition over any message | `core/src/input/proto/ksy` limits; `PROBE_ROUNDS=20000 cargo test --release -p nerka-core --test ksy_dlt mutated` |

## SUP — CI, release, supply chain (cycle)

| ID | Check | How |
|---|---|---|
| SUP-1 🤖 | Workflow actions pinned by commit SHA (the release job has `contents: write`) | grep `uses:` |
| SUP-2 🤖 | Release builds use `--locked` | `release.yml` |
| SUP-3 🤖 | Release assets come with SHA-256 checksums | `release.yml` |
| SUP-4 | CI covers what the release ships (Linux, Windows; macOS when added) | `ci.yml` matrix |

## TEST — test coverage (cycle)

| ID | Check | How |
|---|---|---|
| TEST-1 | The CLI has end-to-end tests (output, exit status) | `cli/tests/` |
| TEST-2 | Each reader has a corrupt-input test, not only the fuzz run | `input/*/tests.rs` |
| TEST-3 | GUI input handling has headless tests where the logic is non-trivial | `view::tests` pattern |
| TEST-4 | Generators exist for every input the tests need (no real data) | `core/examples/gen_*` |

## DOC — documentation (cycle)

| ID | Check | How |
|---|---|---|
| DOC-1 🤖 | `CLAUDE.md` names only paths that exist | the script |
| DOC-2 🤖 | `status.md` has a current date and its "Next steps" match the open work | read it |
| DOC-3 | Module docs (`//!`) match what the module does now | read the changed modules |
| DOC-4 | A user-visible change is in README or `doc/` | diff |
