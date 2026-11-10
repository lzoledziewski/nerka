#!/usr/bin/env bash
# The automated part of a review (doc/review/README.md). Every check prints
# PASS / WARN / FAIL with its ID from doc/review/checklist.md; the exit status
# is 1 if any blocking check FAILs. WARN never blocks: it is something for
# the reviewer to look at.
#
#   tools/review-gate.sh            # quick: build gates + metrics (minutes)
#   tools/review-gate.sh --full     # + fuzz 3000 rounds, + MSRV if installed
#   BENCH_BASE=doc/review/baseline/bench-YYYY-MM-DD.tsv tools/review-gate.sh --full --bench
#
# Run from anywhere inside the repository; Git Bash on Windows, bash on Linux.
set -uo pipefail
cd "$(git rev-parse --show-toplevel)"

FULL=0; BENCH=0
for a in "$@"; do
  case $a in --full) FULL=1 ;; --bench) BENCH=1 ;; *) echo "unknown option $a" >&2; exit 2 ;; esac
done

fails=0
OUT=${REVIEW_OUT:-target/review}
mkdir -p "$OUT"
say() { printf '%-5s %-9s %s\n' "$1" "$2" "$3"; }
pass() { say PASS "$1" "$2"; }
warn() { say WARN "$1" "$2"; }
fail() { say FAIL "$1" "$2"; fails=$((fails + 1)); }
# run ID "description" cmd...: blocking; output kept in $OUT/ID.log
run() {
  local id=$1 what=$2; shift 2
  if "$@" >"$OUT/$id.log" 2>&1; then pass "$id" "$what"; else fail "$id" "$what (see $OUT/$id.log)"; fi
}

echo "== build gates"
run GATE-1 "cargo fmt --check" cargo fmt --all --check
run GATE-2 "clippy -D warnings" cargo clippy --workspace --all-targets -- -D warnings
run GATE-3 "tests" cargo test --workspace
if command -v cargo-deny >/dev/null 2>&1; then
  run GATE-4 "cargo deny" cargo deny check
  grep -q "yanked" "$OUT/GATE-4.log" && warn GATE-4 "a yanked crate in Cargo.lock (cargo update -p …)"
else
  warn GATE-4 "cargo-deny not installed (cargo install cargo-deny)"
fi

echo "== MSRV"
msrv=$(sed -nE 's/^rust-version *= *"([0-9.]+)".*/\1/p' Cargo.toml)
ui_msrv=$(sed -nE 's/^rust-version *= *"([0-9.]+)".*/\1/p' ui/Cargo.toml)
# MSRV-2: no dependency of a crate needs a newer Rust than the crate declares.
if command -v python >/dev/null 2>&1; then
  cargo metadata --format-version 1 >"$OUT/metadata.json" 2>/dev/null
  for c in nerka-cli nerka-gui; do
    cargo tree -p "$c" -e normal,build --prefix none 2>/dev/null | awk '{print $1}' | sort -u >"$OUT/deps-$c.txt"
    over=$(python -X utf8 - "$OUT/metadata.json" "$OUT/deps-$c.txt" "$c" <<'EOF'
import json, sys
meta, deps, crate = sys.argv[1:4]
names = set(open(deps, encoding="utf-8").read().split())
pk = {p["name"]: p for p in json.load(open(meta, encoding="utf-8"))["packages"]}
v = lambda s: tuple(int(x) for x in s.split("."))
own = pk[crate].get("rust_version") or "0"
print(" ".join(f'{n}:{pk[n]["rust_version"]}' for n in sorted(names)
               if n in pk and pk[n].get("rust_version") and v(pk[n]["rust_version"]) > v(own)))
EOF
)
    if [ -n "$over" ]; then fail MSRV-2 "$c declares a lower rust-version than its dependencies need: $over"
    else pass MSRV-2 "$c: no dependency needs a newer Rust than it declares"; fi
  done
else
  warn MSRV-2 "python not found: dependencies' rust-version not compared"
fi
if [ "$FULL" = 1 ] && rustup toolchain list 2>/dev/null | grep -q "^$msrv"; then
  run MSRV-1 "core and CLI build on rust-version $msrv" cargo "+$msrv" check -p nerka-core -p nerka-cli --all-targets
  if [ -n "$ui_msrv" ] && rustup toolchain list 2>/dev/null | grep -q "^$ui_msrv"; then
    run MSRV-1 "GUI builds on rust-version $ui_msrv" cargo "+$ui_msrv" check -p nerka-gui --all-targets
  fi
else
  # let-chains (`if let … && let …`) need Rust 1.88.
  if [ -n "$msrv" ] && [ "$(printf '%s\n1.88\n' "$msrv" | sort -V | head -1)" != 1.88 ] &&
     grep -rqE '&& let (Some|Ok|Err|None)' --include='*.rs' core cli ui; then
    fail MSRV-1 "rust-version is $msrv but the code uses let-chains (Rust 1.88+); raise it or test it (rustup install $msrv)"
  else
    warn MSRV-1 "rust-version $msrv not tested (rustup install $msrv, then --full)"
  fi
fi

echo "== architecture metrics"
# ARCH-1: the UI must not parse or filter: no regex crate, no Regex in ui/.
if grep -q '^regex' ui/Cargo.toml || grep -rqE 'regex::|Regex::new' ui/src; then
  fail ARCH-1 "ui/ uses regex directly: parsing / filtering belongs in core"
else pass ARCH-1 "ui/ has no regex of its own"; fi
# ARCH-2: big files. WARN over 2500 lines of non-test code.
big=$(for f in $(git ls-files '*.rs'); do
  n=$(awk '/^#\[cfg\(test\)\]/{exit} {n++} END{print n+0}' "$f"); [ "$n" -gt 2500 ] && echo "$f:$n"; done)
if [ -n "$big" ]; then warn ARCH-2 "files over 2500 non-test lines: $(echo $big)"; else pass ARCH-2 "no file over 2500 non-test lines"; fi
# ARCH-3: LogSource surface.
m=$(awk '/^pub trait LogSource/{f=1} f&&/^    fn /{n++} f&&/^}/{print n; exit}' core/src/input/mod.rs)
if [ "${m:-0}" -gt 20 ]; then warn ARCH-3 "LogSource has $m methods (split into capabilities, status.md)"; else pass ARCH-3 "LogSource has $m methods"; fi
# ARCH-4: GUI Tab size.
t=$(awk '/^(pub )?struct Tab \{/{f=1;next} f&&/^\}/{print c; exit} f&&/^    (pub )?[a-z_]+:/{c++}' ui/src/main.rs)
if [ "${t:-0}" -gt 40 ]; then warn ARCH-4 "Tab has $t fields (move state into core::Session)"; else pass ARCH-4 "Tab has ${t:-?} fields"; fi
# ARCH-5: unsafe stays where it is reviewed.
u=$(grep -rlE '\bunsafe\b' --include='*.rs' core cli ui | sort | tr '\n' ' ')
pass ARCH-5 "files with unsafe: ${u:-none} (each needs a SAFETY comment; review new ones)"
grep -rnE 'unsafe \{' --include='*.rs' core cli ui | while IFS=: read -r f l _; do
  sed -n "$((l > 3 ? l - 3 : 1)),${l}p" "$f" | grep -q 'SAFETY' || echo "  no SAFETY comment: $f:$l"
done

echo "== robustness"
# CODE-1: persistence written in place (a crash mid-write loses the file).
# Counted only above a file's `#[cfg(test)]`, and not in test files.
w=$(for f in $(git ls-files 'core/src/*.rs' 'ui/src/*.rs' | grep -vE '(^|/)(tests|[a-z0-9_]+_tests)\.rs$'); do
  awk '/^#\[cfg\(test\)\]/{exit} /fs::write\(/ && !/^\s*\/\//{n++} END{print n+0}' "$f"; done | awk '{s+=$1} END{print s+0}')
[ "${w:-0}" -gt 0 ] && warn CODE-1 "$w non-atomic fs::write call sites (write a temp file, then rename)" || pass CODE-1 "writes are atomic"
if [ "$FULL" = 1 ]; then
  seeds=$OUT/seeds; mkdir -p "$seeds"
  cargo build --release -p nerka-core --examples >"$OUT/build-examples.log" 2>&1
  ex=target/release/examples
  [ -f "$seeds/a.dlt" ] || { "$ex/gen_dlt" "$seeds/a.dlt" 1 && "$ex/gen_log" "$seeds/b.log" 1 &&
    "$ex/gen_captures" "$seeds" && "$ex/dlt_to_pcap" "$seeds/a.dlt" "$seeds/c.pcap" &&
    "$ex/dlt_to_mf4" "$seeds/a.dlt" "$seeds/d.mf4" && "$ex/dlt_to_mf4" "$seeds/a.dlt" "$seeds/e_z.mf4" --deflate &&
    head -c 65536 "$seeds/a.dlt" >"$seeds/a_small.dlt" &&
    printf '{"ts":"2026-11-07T10:00:00Z","level":"error","ctx":{"user":"u1","n":[1,2]},"msg":"x \\u00e9","lat":1.5}\n{"level":30,"msg":"y"}\n' >"$seeds/f.jsonl"
  } >"$OUT/seeds.log" 2>&1
  run CODE-2 "fuzz: 3000 mutated inputs, no panic" "$ex/mutate_open" "$seeds" 3000 "${FUZZ_SEED:-$RANDOM}"
fi

echo "== docs and AI context"
# DOC-1: paths the agent guide names exist.
missing=$(grep -oE '`[a-z_./-]+/[a-zA-Z0-9_./-]*`' CLAUDE.md | tr -d '`' | sort -u | while read -r p; do
  [ -e "$p" ] || [ -e "core/src/$p" ] || echo "$p"; done | tr '\n' ' ')
[ -n "$missing" ] && warn DOC-1 "CLAUDE.md names paths that don't exist: $missing" || pass DOC-1 "CLAUDE.md paths exist"
grep -oE -- '--pack [a-z_]+/' CLAUDE.md | awk '{print $2}' | sort -u | while read -r d; do
  [ -e "$d" ] || warn DOC-1 "CLAUDE.md examples use --pack $d, which isn't in the repo"; done
# DOC-2: context an agent must read before work (CLAUDE.md + the docs it requires).
# ~3.3 bytes per token for these docs (code names, punctuation, Polish names):
# status.md's 75 KB read as 28.5k tokens on 2026-11-07.
req=$(wc -c < CLAUDE.md); for f in doc/nerka_architecture_implementation_spec_v_0.md doc/architecture-analysis.md doc/status.md; do
  req=$((req + $(wc -c <"$f"))); done
tok=$((req * 10 / 33))
if [ $tok -gt 25000 ]; then warn DOC-2 "required reading is ~${tok} tokens (budget 25000): split status.md"; else pass DOC-2 "required reading ~${tok} tokens"; fi
# DOC-3: very long lines make docs hard to diff and to edit by exact match.
long=$(awk 'length($0) > 1500 { n++ } END { print n + 0 }' doc/*.md)
[ "$long" -gt 0 ] && warn DOC-3 "$long doc lines over 1500 characters" || pass DOC-3 "no doc line over 1500 characters"
# AIX-8: each doc an agent reads fits one read (~4 bytes a token; 20k tokens).
for f in doc/status.md doc/gui-decisions.md doc/features.md doc/history.md; do
  [ -f "$f" ] || continue
  t=$(( $(wc -c < "$f") / 4 ))
  if [ $t -gt 20000 ]; then warn AIX-8 "$f is ~${t} tokens: more than one read"; else pass AIX-8 "$f ~${t} tokens"; fi
done
# DOC-4: status.md is dated.
d=$(sed -nE 's/^_Last update: ([0-9-]{10}).*/\1/p' doc/status.md)
pass DOC-4 "status.md last update: ${d:-none}"

echo "== CI and supply chain"
unp=$(grep -hE '^\s*- uses: ' .github/workflows/*.yml | grep -vcE '@[0-9a-f]{40}')
[ "$unp" -gt 0 ] && warn SUP-1 "$unp workflow actions pinned by tag, not by commit SHA" || pass SUP-1 "actions pinned by SHA"
grep -q -- '--locked' .github/workflows/release.yml && pass SUP-2 "release builds --locked" || warn SUP-2 "release builds without --locked"
grep -qiE 'sha256|checksum|attest' .github/workflows/release.yml && pass SUP-3 "release publishes checksums" || warn SUP-3 "release assets have no checksums"

if [ "$BENCH" = 1 ]; then
  echo "== performance"
  cargo build --release -p nerka-cli -p nerka-core --bins --examples >"$OUT/build-bench.log" 2>&1
  # An empty table must not pass: every case has to have run.
  if tools/bench.sh "${BENCH_DIR:-target/bench}" "${BENCH_MIB:-1024}" "${BENCH_RUNS:-5}" >"$OUT/bench.tsv" 2>"$OUT/bench.log" &&
    [ "$(wc -l <"$OUT/bench.tsv")" -gt 1 ]; then
    pass PERF-1 "benchmark written to $OUT/bench.tsv"
  else
    fail PERF-1 "benchmark did not run (see $OUT/bench.log)"
  fi
  if [ -n "${BENCH_BASE:-}" ] && [ "$(wc -l <"$OUT/bench.tsv")" -gt 1 ]; then
    tools/bench.sh compare "$BENCH_BASE" "$OUT/bench.tsv" | tee "$OUT/bench-compare.tsv"
    if grep -q "MATCHES DIFFER" "$OUT/bench-compare.tsv"; then fail PERF-2 "match counts changed against $BENCH_BASE"
    elif grep -q SLOWER "$OUT/bench-compare.tsv"; then warn PERF-2 "slower than $BENCH_BASE by over 15 % (rerun on an idle machine before trusting it)"
    else pass PERF-2 "no query more than 15 % slower than $BENCH_BASE"; fi
  fi
fi

echo
if [ $fails -gt 0 ]; then echo "review gate: $fails blocking check(s) failed"; exit 1; fi
echo "review gate: passed (WARN lines are for the reviewer)"
