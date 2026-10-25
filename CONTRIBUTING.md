# Contributing to nerka

Thanks for helping. nerka is a fast log viewer and analyser (plain text,
DLT, pcap / pcapng, MF4): a UI-free engine (`core/`), a command line tool
(`cli/`) and a desktop GUI (`ui/`, egui).

Before larger changes, open an issue to agree on the approach. Design
decisions are recorded in `doc/` (start with `doc/architecture-analysis.md`
and `doc/status.md`); a change that goes against one needs that discussion
first.

## Building and checking

```
cargo build --workspace
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all
```

On Linux the GUI needs: `pkg-config libxkbcommon-dev libwayland-dev
libxcb-render0-dev libxcb-shape0-dev libxcb-xfixes0-dev libgl1-mesa-dev`.

CI runs the same checks on Linux and Windows, plus a licence and advisory
check of the dependencies (`cargo deny check`) and the sign-off check below.

## Rules of the code base

- `core/` holds **all** filtering, parsing and format logic. The CLI and the
  GUI only show results and send query strings; they never parse logs.
- Line numbers are 0-based `LineNo` (u64) internally; add 1 only for display.
- Never lose or modify the original line (`raw`); normalized fields are lazy.
- New input formats implement `input::LogSource`; the engine must not care
  about formats.
- Long operations take `&Progress` and check `is_cancelled()` per chunk.
- **Search engine changes:** every engine test runs the fast path against
  the naive per-line path (`run_filter` in `core/src/engine/search.rs`).
  Keep that invariant, and add a test for every bug fixed.
- **Query language (NFD) changes:** update the grammar comment in
  `core/src/dsl/mod.rs` and add parser tests, including error positions.
- **Performance-sensitive changes:** report before / after numbers in the
  pull request, measured on a generated 1 GiB file:

  ```
  cargo run --release -p nerka-core --example gen_log -- big.log 1024
  cargo run --release -p nerka-cli -- search -c --stats ERROR big.log
  ```

- **Dependencies:** keep them few. A new one must be justified in the pull
  request (what it does, why not our own code, licence, its own
  dependencies).
- **Test data is synthetic.** Never commit real logs or captures, even
  anonymized ones; generate them (`core/examples/gen_*`) or write them by
  hand.

## Pull requests

- One topic per pull request; describe what changes and why.
- Update the docs in `doc/` (and `README.md`) when behaviour changes.
- Fill in the checklist of the pull request template.

## Sign your commits (DCO)

Every commit must carry a `Signed-off-by:` line, certifying the
[Developer Certificate of Origin](https://developercertificate.org/): that
you wrote the change, or otherwise have the right to submit it under the
project's licence. Add it with `-s`:

```
git commit -s -m "core: fix …"
```

It must match the commit's author. To sign commits you already made:
`git rebase --signoff main`, then force-push your branch. A CI check rejects
pull requests with unsigned commits.

## Licence

nerka is licensed under either of [Apache License 2.0](LICENSE-APACHE) or
[MIT](LICENSE-MIT), at your option. Unless you state otherwise, any
contribution you submit for inclusion is dual-licensed the same way, without
any additional terms or conditions. There is no contributor licence
agreement.
