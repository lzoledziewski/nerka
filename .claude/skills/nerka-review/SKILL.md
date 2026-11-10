---
name: nerka-review
description: Run nerka's review, either as a gate (before a merge or release) or as the periodic deep review from every perspective (architecture, code, human and AI usability, performance, security, CI, tests, docs), following doc/review/. Use when asked to review the repository, run the review gate, or repeat the last review.
---

# nerka review

The procedure is in `doc/review/README.md`; the items are in
`doc/review/checklist.md`. Read both first. Then read the latest
`doc/review/*-findings.md`, which holds the previous metrics and the open findings.

## Gate (asked to "run the gate", or before a merge or tag)

1. `bash tools/review-gate.sh --full`. Add `--bench` with
   `BENCH_BASE=<latest doc/review/baseline/*.tsv>` if the change touches
   `core/src/engine`, `core/src/input`, `core/src/dsl` or `core/src/event.rs`.
2. Read the diff against the `gate` items it touches (ARCH-1, CODE-3, CODE-4, SEC-*).
3. Report PASS or FAIL with the failing IDs. WARN lines are listed, not blocking.

## Cycle (asked for a review, a deep review, or "repeat the review")

1. Never review in the owner's working copy, since a feature may be under way
   there. Make a worktree from the commit to review:
   `git worktree add -b claude/review-YYYY-MM-DD ../nerka-review <commit>`,
   and work only in it.
2. Free disk: the 1 GiB benchmark needs ~3 GB; put the files in the scratchpad or `BENCH_DIR`, not in the repo.
3. `bash tools/review-gate.sh --full --bench` (with `BENCH_BASE`). Run the
   benchmark when the machine is otherwise idle, not next to a fuzz run or a build.
4. Each perspective of `checklist.md`, by hand for the items without 🤖.
   Look for bugs with tests: a suspected wrong result becomes a test that
   compares the fast path with the naive one (`run_on`) before it is called
   a finding. Try the CLI's error paths yourself.
5. Re-check every open finding of the last review.
6. Generate missing inputs (synthetic only; see "Inputs" in the last findings).
7. Write `doc/review/YYYY-MM-DD-findings.md` in the same format as the last one, with the metrics side by side.
8. Fix S1 findings with a regression test. Leave everything else as findings.
   Don't change the owner's GUI decisions (`doc/gui-decisions.md`) or open issues unasked.
9. Commit on the review branch. Ask before pushing (the owner's rule).

## Rules that apply here

- CLAUDE.md's rules hold: fast vs naive tests, grammar comment for DSL changes, before / after numbers for performance changes.
- Findings are concrete: input → wrong output, with `file:line` and a test or command that shows it. "Plausible" ones say so.
- Don't mark something fixed that wasn't verified by a test or a run.
