## What and why

<!-- What changes, and why. Link the issue: "Fixes #…". -->

## Checklist

- [ ] `cargo fmt --all`, `cargo clippy --workspace --all-targets -- -D warnings` and `cargo test --workspace` pass
- [ ] Every commit is signed off (`git commit -s`, see CONTRIBUTING.md)
- [ ] New behaviour has tests; a bug fix has a test that failed before
- [ ] Search engine changes: tests compare the fast path with the naive per-line path
- [ ] Query language changes: grammar comment in `core/src/dsl/mod.rs` and parser tests (with error positions)
- [ ] Performance-sensitive changes: before / after `--stats` numbers on a 1 GiB file below
- [ ] New dependencies: justified below (what, why not our own code, licence)
- [ ] Docs in `docs/dev/` and `README.md` updated where behaviour changed
- [ ] No real logs or captures added (test data is synthetic)

## Numbers / dependencies

<!-- If applicable. -->
