#!/usr/bin/env bash
# The --stats benchmark of CLAUDE.md as one repeatable run: generates the
# test files once (1 GiB each by default), runs every query RUNS times and
# prints the median search time and match count per query as TSV, so two
# runs (before / after a change) can be compared with `diff` or
# `tools/bench.sh compare old.tsv new.tsv`.
#
#   tools/bench.sh [DIR] [MIB] [RUNS] > bench.tsv     (DIR: where the big files go)
#   tools/bench.sh compare before.tsv after.tsv       (flags changes over 15 %)
#
# Run on an idle machine with a warm page cache (the first run of each file
# warms it and is dropped). Needs a release build: cargo build --release
# -p nerka-cli -p nerka-core --bins --examples (NERKA_BIN: another folder of it).
#
# KSY=1 adds the protocol definition cases (docs/dev/protocol-definitions.md):
# big2.pcap has every datagram on ports 3490 and 3491; native DLT reads one,
# examples/protocols/dlt.ksy (DLTK) the other. Each query runs per protocol
# (`proto == DLT AND …`, `proto == DLTK AND …`), so both readers read the
# same messages in the same file.
#
# Signals, plot and memory (review 2026-11-14; SIGNALS=0 leaves them out): a
# FIBEX trace of MIB (gen_fibex_dlt, 40 messages) as a pcap, 30 of its
# arguments as signals (perf_signals): the open, collecting them, building
# the plot's lanes, a frame at full zoom, the peak heap (MiB, a run of its
# own with --heap). GUI=1 (the default) also runs the GUI's own code headless
# (cargo test, ignored tests): a frame of the whole window with the signals
# plotted, a plot repaint at full zoom, a signal added until the plot shows.
# A change is flagged when it is over 15 % and over 1 (ms or MiB), so
# sub-millisecond frames don't flag noise.
set -euo pipefail

if [ "${1:-}" = compare ]; then
  join -t $'\t' <(tail -n +2 "$2" | sort) <(tail -n +2 "$3" | sort) |
    awk -F'\t' 'BEGIN { OFS = "\t"; print "query", "before_ms", "after_ms", "change", "matches" }
      { ch = ($2 > 0) ? ($4 - $2) / $2 * 100 : 0
        big = ($4 - $2 > 1 || $2 - $4 > 1)
        flag = (ch > 15 && big) ? "  SLOWER" : (ch < -15 && big ? "  faster" : "")
        if ($3 != $5) flag = flag "  MATCHES DIFFER (" $3 " vs " $5 ")"
        printf "%s\t%s\t%s\t%+.0f%%%s\t%s\n", $1, $2, $4, ch, flag, $5 }'
  exit 0
fi

DIR=${1:-target/bench}
MIB=${2:-1024}
RUNS=${3:-5}
ROOT=$(cd "$(dirname "$0")/.." && pwd)
BIN=${NERKA_BIN:-$ROOT/target/release}
EX=$BIN/examples
for b in "$BIN/nerka" "$EX/gen_log" "$EX/gen_dlt" "$EX/dlt_to_pcap"; do
  [ -x "$b" ] || [ -x "$b.exe" ] || { echo "bench: no $b (cargo build --release -p nerka-cli -p nerka-core --bins --examples)" >&2; exit 1; }
done
mkdir -p "$DIR"

[ -f "$DIR/big.log" ] || "$EX/gen_log" "$DIR/big.log" "$MIB" >&2
[ -f "$DIR/big.dlt" ] || "$EX/gen_dlt" "$DIR/big.dlt" "$MIB" >&2
[ -f "$DIR/big.pcap" ] || "$EX/dlt_to_pcap" "$DIR/big.dlt" "$DIR/big.pcap" >&2

# file<TAB>kind<TAB>pattern-or-query
CASES=$(cat <<'EOF'
big.log	search	ERROR
big.log	search-v	ERROR
big.log	filter	level >= ERROR
big.log	filter	level >= WARN AND raw ~ /timeout/
big.log	filter	level == INFO OR raw ~ /disk/
big.dlt	filter	apid == NAV AND level >= WARN
big.dlt	filter	ecu == ECU1
big.dlt	filter	level >= ERROR AND raw ~ /timeout/
big.dlt	filter	raw ~ /timeout/
big.pcap	filter	apid == NAV AND level >= WARN
EOF
)

if [ "${KSY:-0}" = 1 ]; then
  [ -f "$DIR/big2.pcap" ] || "$EX/dlt_to_pcap" "$DIR/big.dlt" "$DIR/big2.pcap" --ports 3490,3491 --copy >&2
  for p in DLT DLTK; do
    for q in 'apid == NAV AND level >= WARN' 'ecu == ECU1' 'level >= ERROR' 'mcnt < 10' 'raw ~ /ECU1 NAV/' 'payload ~ /timeout/'; do
      CASES+=$'\n'"big2.pcap"$'\t'"filter-ksy"$'\t'"proto == $p AND $q"
    done
  done
fi

# One run: "<ms> <matches>" from the --stats line on stderr.
one() {
  local file=$1 kind=$2 q=$3 err
  case $kind in
    search) err=$("$BIN/nerka" search -c --stats "$q" "$DIR/$file" 2>&1 >/dev/null) ;;
    search-v) err=$("$BIN/nerka" search -v -c --stats "$q" "$DIR/$file" 2>&1 >/dev/null) ;;
    filter) err=$("$BIN/nerka" filter -c --stats "$q" "$DIR/$file" 2>&1 >/dev/null) ;;
    filter-ksy) err=$("$BIN/nerka" --protocol "$ROOT/examples/protocols/dlt.ksy" --protocol-port DLTK=3491 \
      filter -c --stats "$q" "$DIR/$file" 2>&1 >/dev/null) ;;
  esac || { echo "bench: $kind '$q' on $file failed: $err" >&2; exit 1; }
  # "N matches in M lines | index X | search 12.3ms (...)"
  local matches t
  matches=$(sed -E 's/^([0-9]+) matches.*/\1/' <<<"$err")
  t=$(sed -E 's/.*\| search ([0-9.]+)(ms|s|µs).*/\1 \2/' <<<"$err")
  awk -v t="$t" -v m="$matches" 'BEGIN { split(t, a, " "); v = a[1];
    if (a[2] == "s") v *= 1000; else if (a[2] == "µs") v /= 1000; printf "%.1f %s\n", v, m }'
}

printf 'query\tmedian_ms\tmatches\n'
while IFS=$'\t' read -r file kind q; do
  one "$file" "$kind" "$q" >/dev/null # warm the page cache
  times=()
  for _ in $(seq "$RUNS"); do
    read -r ms m < <(one "$file" "$kind" "$q")
    times+=("$ms")
  done
  med=$(printf '%s\n' "${times[@]}" | sort -n | awk '{ a[NR] = $1 } END { print a[int((NR + 1) / 2)] }')
  printf '%s %s: %s\t%s\t%s\n' "$file" "$kind" "$q" "$med" "$m"
done <<<"$CASES"

[ "${SIGNALS:-1}" = 1 ] || exit 0
for b in "$EX/gen_fibex_dlt" "$EX/perf_signals"; do
  [ -x "$b" ] || [ -x "$b.exe" ] || { echo "bench: no $b (cargo build --release -p nerka-core --examples)" >&2; exit 1; }
done
[ -f "$DIR/nv.dlt" ] || "$EX/gen_fibex_dlt" "$DIR/nv.dlt" "$DIR/nv.xml" "$MIB" 40 >&2
[ -f "$DIR/nv.pcap" ] || "$EX/dlt_to_pcap" "$DIR/nv.dlt" "$DIR/nv.pcap" >&2
median() { sort -n | awk '{ a[NR] = $1 } END { print a[int((NR + 1) / 2)] }'; }
# A perf_signals line's value and the number in its third column.
field() { awk -F'\t' -v k="$1" '$1 == k { v = $2; c = $3 } END { sub(/ .*/, "", v); sub(/ .*/, "", c); print v, c }'; }
signals() { "$EX/perf_signals" "$DIR/nv.pcap" "$DIR/nv.xml" 30 --skip-filters "$@"; }
signals >/dev/null # warm the page cache
declare -A got count
for _ in $(seq "$RUNS"); do
  out=$(signals)
  for k in open "series total" "plot build" "frame (zoom 1)"; do
    read -r v c < <(field "$k" <<<"$out")
    got[$k]+="$v"$'\n'
    count[$k]=$c
  done
done
row() { printf '%s\t%s\t%s\n' "$1" "$(printf '%s' "$2" | median)" "$3"; }
row "nv.pcap signals: open" "${got[open]}" "${count[open]}"
row "nv.pcap signals: 30 collected" "${got[series total]}" "${count[series total]}"
row "nv.pcap signals: plot lanes built" "${got[plot build]}" "${count[series total]}"
row "nv.pcap signals: plot frame at full zoom" "${got[frame (zoom 1)]}" "${count[frame (zoom 1)]}"
read -r heap _ < <(signals --heap | field "peak heap")
printf '%s\t%s\t%s\n' "nv.pcap signals: peak heap MiB" "$heap" "-"

[ "${GUI:-1}" = 1 ] || exit 0
# The GUI's own code, headless (ui/src/plot.rs, ui/src/main.rs): absolute
# paths, since cargo runs the tests in the crate's folder.
abs=$(cd "$DIR" && { pwd -W 2>/dev/null || pwd; })
gui() {
  NERKA_PERF_FILE="$abs/nv.pcap" NERKA_PERF_FIBEX="$abs/nv.xml" \
    cargo test --release -q --manifest-path "$ROOT/Cargo.toml" -p nerka-gui "$1" -- --ignored --nocapture 2>&1
}
gui perf_big_app >/dev/null # build and warm
frames=""
added=""
repaint=""
for _ in $(seq "$RUNS"); do
  out=$(gui perf_big_app)
  frames+=$(grep "with signals and plot, scroll the log:" <<<"$out" | sed -E 's/.*: ([0-9.]+) ms.*/\1/' | median)$'\n'
  out=$(gui perf_big_plot)
  added+=$(grep -m1 "=> plot again after" <<<"$out" | sed -E 's/.* ([0-9.]+) ms.*/\1/')$'\n'
  repaint+=$(grep "pointer moved, full zoom" <<<"$out" | tail -1 | sed -E 's/.*ui ([0-9.]+) ms.*/\1/')$'\n'
done
row "nv.pcap gui: window frame, signals plotted" "$frames" "-"
row "nv.pcap gui: plot repaint at full zoom" "$repaint" "-"
row "nv.pcap gui: signal added until plotted" "$added" "-"
