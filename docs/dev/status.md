# Status & next steps

Snapshot of where the project stands. Update this file whenever the state changes.

Kept short, so that it reads in one go (review 2026-11-09, R4: the read stopped before "Decisions pending"). When the state changes, update it here and add a dated line at the top of `history.md`.

_Last update: 2026-11-12 (0.2.0, a release: lifecycles, checks, protocol definitions, several files as one view, settings bundles, windows sharing a configuration since 0.1.0; today Settings → Theme, a tab's Reload, Sort by time for one file, Move to window, the plot window's name and close; before: 2026-11-11, 0.2.0-alpha.4, a pre-release: review findings fixed, `nerka-gui --lifecycle`, library query names, the user guide in `docs/user/` and the ledger in `docs/dev/`; text profiles designed, not built: `text-profiles.md`)._

**Where the rest is:**
- `docs/user/` (home `docs/index.md`): the user guide, shipped and on GitHub Pages; this folder (`docs/dev/`) is the ledger. What goes where: CLAUDE.md → "Documentation".
- `docs/dev/gui-decisions.md`: the owner's GUI decisions ("GUI as of now"), deliberate choices to keep. **Read it before changing the UI.**
- `docs/dev/features.md`: each built feature as it stands, with the owner's decisions about it (DLT and captures, signals, FIBEX, protocol definitions, search manager).
- `docs/dev/history.md`: the dated log of updates and the earlier "Next steps", newest first, append-only.
- Design: `sources-and-signals.md`, `search-manager.md`, `lifecycles.md`, `checks.md`, `protocol-definitions.md`, `bundles.md`, `text-profiles.md` (not built); reviews: `docs/dev/review/`.

## Decisions pending with the owner

Don't settle these silently: ask.

1. **Level detection and text profiles** (review 2026-11-09, R11; owner, 2026-11-11: a profile per pattern): only upper-case tokens (`ERROR`, `[WARN]`, `level=INFO`) in the first 160 bytes are recognised; JSON without a level key falls back to that scan. The design, measurements and open questions are in `text-profiles.md`: built together with adjusting a file's time (zone, kind, an offset or an anchor; any file, not only text); a profile (records, level rule, time; other values stay signals) chosen by forced choice (remembered in `records`), file name, first line, or the default; JSON has no level without a key; protocol level maps are a choice.
2. **Bookmark storage:** currently a sidecar `<log>.nerka-marks` next to the log. The alternative is a central app-data folder when the log's directory is read-only.
3. **DLT column widths:** resizable per tab but not saved. Save them (globally or per file)?
4. **Releases / downloads:** `release.yml` (Linux + Windows pre-releases on `v*` tags, archives with the README and both licence files) is in place; open: macOS builds (unsigned binaries warn; 10x runner minutes).
5. **Trying Linux locally:** CI builds and tests Linux, but this Windows PC has no WSL, Docker or zig, so the Linux GUI is untried. WSL would also run it.
6. **Copying multi-line messages (owner, 2026-10-19: action item, rethink later):** a decoded message's own line breaks and tabs show as spaces in the views and are kept only in the line window (Enter). Ctrl+C / Copy in the views copy the display text (formatting dropped); the line window's Copy message / Copy line keep it. The owner isn't fully happy with this, but neither single rule is good: always copying with the original formatting breaks the one-row-per-line, tab-separated paste (spreadsheets, grep), while always copying the display text loses stack traces and JSON. Revisit (e.g. a copy mode, or a second shortcut).
7. **The CLI's open options in the GUI** (review 2026-11-09, R7 e; owner, 2026-11-11: `--lifecycle` only, done): the other 12 (`--protocol`, `--route`, ports, `--records`, `--merge`, times, `--unpack-dir`) and one parser both binaries share are open; `-q` / `-r` mean different things in the two (a pack query / a filter, `--require` / `--regex`).
8. **Pulling a needed query into the Selection** (review 2026-11-09, U7; owner: needs a discussion first) and **query signals with saved signals' options** (U6: a feature of its own, later).

## State

- Builds and runs on **Windows** (MSVC, debug and release). Clippy `-D warnings`, fmt and all tests pass. The 1 GiB benchmarks have been run on Windows (numbers in `history.md` and in the commit messages).
- Work goes through branches; `main` is pushed to GitHub when the owner says so (never push unasked). Open GitHub issues are the owner's to-do list.
- Developed and used on Windows; Linux builds and tests run in CI and ship in the releases, but the GUI hasn't been tried on Linux. macOS is an open question (see "Decisions pending").
- **CI and releases (owner, 2026-10-18):** `.github/workflows/ci.yml` runs fmt, clippy and the tests on Linux and Windows for pushes to `main` and every PR (macOS not built yet: its runner minutes counted 10x while the repo was private; free on a public repo, so it can be added — the GUI hasn't been tried on macOS). `release.yml` builds `nerka` + `nerka-gui` for Linux (Ubuntu 22.04, glibc 2.35+, `.tar.gz`) and Windows (`.zip`) on a `v*` tag and publishes them as a GitHub pre-release (tags with `-`, e.g. `v0.1.0-alpha.1`); run by hand it only builds artifacts. Version is `0.2.0`, a release (GitHub's Latest); pre-releases have a `-` in their tag (`v0.2.0-alpha.4`) and never become Latest. Each release: `history.md`. Since 2026-10-25 CI also runs `cargo deny check` (`deny.toml`: permissive licences only, egui's fonts excepted, their texts in `licenses/fonts/` and in the release archives; the advisories it ignores all come through eframe, with reasons) and, on pull requests, a DCO sign-off check (the maintainer's commits are exempt). The crates are `publish = false`: nothing goes to crates.io yet.

## Next steps

The open work, in no fixed order; what was done before is in `history.md`, the open review findings in `docs/dev/review/2026-11-09-findings.md`.

- **Benchmark:** 0.2.0 measured on 2026-11-12 (every query within 15 % of 2026-11-09, the same counts; `history.md`). The mixed-view cases are measured by hand and not yet in `tools/bench.sh`.
- **Decisions pending** above: 1 (text profiles, `text-profiles.md`'s open questions), 7, 8.
- **Postponed by the owner (2026-11-10):** the CLI for scripts and agents (`--json`, exit codes, `-m`, context, stdin; review R10, before the MCP server); `ui/src/main.rs`'s size, with other refactoring (11-08 R6).
- **Later, with the MCP server:** local time per session instead of per process (R12); files opened in parallel with one progress total (R18); follow / tail mode (a file truncated while mapped can fault, 11-07 R13); DLT `raw ~` without rendering every message (11-07 R14).

## Known limitations

- The line index uses 8 bytes per line (≈94 MB per 12 M lines); a block-compressed index is planned.
- No follow/tail mode: if a file is truncated while it is mapped, reads may fault on Linux (11-07 R13); Windows refuses to shorten a mapped file, so there the writer's truncation fails instead (tried 2026-11-11).
- Only UTF-8/ASCII text is handled; invalid bytes are shown as U+FFFD.
- The GUI binary is ~19 MB on Linux. The size on Windows hasn't been measured.
- DLT: only files with storage headers; FIBEX only in the common convention (non-verbose messages; no value tables, no FIBEX 4.x specifics yet); arrays/structs/fixed-point arguments shown as hex; time in UTC only; the message index is built single-threaded (~280 ms per GiB).

## Where state lives

Nothing is stored in the cloud.

- **Nerka:** how text files are split into records (`records`), search histories, prefs, query packs (`packs\`), presets (`presets\`) and kept plots (`plots\`) in `%APPDATA%\nerka\` (Linux: `~/.config/nerka/`); bookmarks as `<log>.nerka-marks` next to the log. Copy those files to move them to another machine.
- **Agent sessions** (Claude Code) are local to the machine they ran on. The project's decisions and state belong in this repo (`CLAUDE.md`, this file, `sources-and-signals.md`, `architecture-analysis.md`), so a new session on any machine starts from the repo.

## Licensing (owner, 2026-10-18)

- **Stays `MIT OR Apache-2.0`** for the core and the apps.
- Goal: the core stays OSS; extensions (plugins / DLLs, planned WASM or C ABI) can be sold as closed source. A permissive licence allows that; it also allows others to sell closed forks, which the owner accepts for now.
- Code already published under `MIT OR Apache-2.0` stays available under it.
- **Decided (owner, 2026-10-25), before going public:** the licence stays `MIT OR Apache-2.0`; **no CLA**; contributions need a **DCO** sign-off (`Signed-off-by:` in every commit, `git commit -s`), checked in CI. Contributions come in under the project's licence (inbound = outbound). This rules out relicensing the core later without every contributor's consent, which the owner accepts. Contributor docs: `CONTRIBUTING.md`, `SECURITY.md`, `CODE_OF_CONDUCT.md`, `.github` templates. History audited before going public (2026-10-25): one author (the owner), no local paths, company names, addresses, credentials or binaries in any commit.
- The plugin interface is also the licensing boundary: keep it stable and documented.
- **Licence files** (owner, 2026-10-18): `LICENSE-MIT` (copyright line "Copyright (c) 2026 Łukasz Żołędziewski", as in the About dialog) and `LICENSE-APACHE` (the canonical Apache 2.0 text from apache.org) are in the repo root and ship in the release archives.
