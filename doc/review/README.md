# Reviews: the gate and the cycle

How nerka is reviewed, so that a review can be repeated by a person or an
agent and its results compared over time.

| File | What |
|---|---|
| `checklist.md` | Every item to check, by perspective, with an ID, how to check it, and whether it blocks (`gate`) or is reviewed periodically (`cycle`) |
| `YYYY-MM-DD-findings.md` | One review's findings, metrics and open items (newest: `2026-11-09-findings.md`) |
| `YYYY-MM-DD-handoff.md` | The findings as work packages with suggested changes, for whoever picks them up (since 2026-11-09) |
| `baseline/bench-YYYY-MM-DD.tsv` | Benchmark baselines (`tools/bench.sh`) |
| `../../tools/review-gate.sh` | The automated items (🤖 in the checklist) |
| `../../tools/bench.sh` | The 1 GiB `--stats` benchmark; `compare` against a baseline |
| `../../core/examples/mutate_open.rs` | Mutation fuzzing of every reader |
| `../../.claude/skills/nerka-review/SKILL.md` | The procedure for an agent |

## Two modes

**Gate**: before a merge to `main` and before a release tag. Takes minutes.

```bash
bash tools/review-gate.sh --full
```

It must end in `review gate: passed`. Also check, by hand, the `gate` items
the change touches (ARCH-1, CODE-3, CODE-4, SEC-*). A performance-sensitive
change adds `--bench` with `BENCH_BASE` set to the latest baseline, and puts
the compare table in the PR (CONTRIBUTING.md asks for before / after numbers).

**Cycle**: a deep review every release, or every ~2 weeks of feature work,
whichever comes first. It covers every perspective in `checklist.md`:

1. Work in a separate worktree, so feature branches stay untouched:
   `git worktree add -b review/YYYY-MM-DD ../nerka-review <commit>`.
2. Run `tools/review-gate.sh --full --bench` with `BENCH_BASE` set to the latest baseline.
3. Go through every checklist item that has no 🤖, perspective by perspective.
   Re-check every **open** finding of the last review: fixed, still open, or no longer relevant.
4. Inputs a check needs but the repo lacks (a generator, a sample, a test
   harness) are generated, never real data, and kept for the next review.
5. Write `YYYY-MM-DD-findings.md` in the format below. Copy the metrics table and fill in the new numbers next to the old ones.
6. Save the new benchmark as `baseline/bench-YYYY-MM-DD.tsv` if the machine is the usual one.
7. Fix S1 findings in the review branch, each with a test that failed before. Everything else goes to the owner as findings, or as GitHub issues if the owner says so.
8. Add new checklist items for any class of problem found that the checklist didn't cover. That way the checklist learns.

## Findings format

Each finding gets an ID (`R<n>`, numbered per review), the checklist item,
severity, what is wrong (concrete: inputs → wrong output), the evidence (a
test, a command, `file:line`), a recommendation, and a status (`open`,
`fixed`, `won't fix (owner, date)`). Rank by severity, most severe first.

| Severity | Meaning | Gate? |
|---|---|---|
| S1 | Wrong results, data loss, crash on input, security | blocks |
| S2 | A rule of CLAUDE.md broken, a user-visible defect, a regression over 15 % | blocks a release unless the owner accepts it |
| S3 | Debt that makes the next change harder | no |
| S4 | Polish | no |

## Thresholds

These are what the script warns at. Change them here and in the script together.

| Metric | Warn at |
|---|---|
| Non-test lines in one `.rs` file | > 2500 |
| `LogSource` methods | > 20 |
| `Tab` fields | > 40 |
| Required agent reading (CLAUDE.md + spec + analysis + status) | > 25k tokens |
| A doc line | > 1500 characters |
| Benchmark query vs baseline | > 15 % slower (blocks if match counts differ) |
