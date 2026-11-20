#!/usr/bin/env bash
# The user guide's screenshots of the demo trace, made again:
#   docs/user/img/plot.png     the plot in its own window
#   docs/user/img/library.png  the Library, picks with line colours, a search
#   docs/user/img/nerka.png    the README's: picks, a query's editor open (its
#                              NFD), a plot, a search (Windows only: a click
#                              opens the editor, tools/screenshot.ps1)
#
#   bash tools/docs-shots.sh [OUT_DIR]     (default docs/user/img)
#
# A fresh config folder (times in UTC, nothing of yours), the demo trace from
# `gen_demo`, the queries from examples/packs/demo.nerka-pack. Windows, Linux
# and macOS (any OS with a display; `xvfb-run` on a headless Linux).
set -euo pipefail
root=$(cd "$(dirname "$0")/.." && pwd)
out=${1:-$root/docs/user/img}
work=$root/target/docs-shots
cargo build -q -p nerka-gui
rm -rf "$work" && mkdir -p "$work/cfg/nerka" "$out"
cargo run -q -p nerka-core --example gen_demo -- "$work/demo.dlt"
printf 'local_time=false\n' > "$work/cfg/nerka/prefs"
gui=$root/target/debug/nerka-gui
pack=$root/examples/packs/demo.nerka-pack
# The config folder: APPDATA on Windows, XDG_CONFIG_HOME elsewhere.
cfg=$work/cfg
if command -v cygpath > /dev/null; then cfg=$(cygpath -w "$cfg"); fi
run() { APPDATA="$cfg" XDG_CONFIG_HOME="$cfg" "$gui" "$work/demo.dlt" --new-window --pack "$pack" "$@"; }

lifecycle='ecu1: lines ecu == ECU1; reset uptime; planned payload ~ /shutdown requested/; crash payload ~ /panic/'
run --signal-pick demo.speed --signal-pick demo.temperature --signal-pick demo.voltage \
    --signal-pick demo.gear --signal-pick demo.ota --lifecycle "$lifecycle" \
    --plot ecu1 --plot speed --plot temp --plot gear --plot ota --plot voltage \
    --marker 'temp=90:throttling' --marker 'speed=130:limit' \
    --cursor '2025-10-18 10:09:20' --cursor '2025-10-18 10:12:40' \
    --plot-window --screenshot "$work/plot.png"
cp "$work/plot-plot.png" "$out/plot.png"

# A tab's selection as a preset (read once, then deleted).
cat > "$work/library.nerka-preset" << 'EOF'
preset demo
node include lit query demo.warnings
node include lit query demo.timeouts
node signal lit query demo.ota
param ecu = ECU1
param ecu += ECU2
filter selection
search regex throttl(ing|ed)
EOF
run --library --preset-file "$work/library.nerka-preset" --screenshot "$out/library.png"
echo "saved $out/plot.png and $out/library.png"

# The README's: the same picks with speed and temperature plotted, and the
# timeouts query's editor open (a click on its ✎ in the Selection).
if ! command -v cygpath > /dev/null; then
    echo "nerka.png: Windows only (tools/screenshot.ps1); left as it is"
    exit 0
fi
cat > "$work/hero.nerka-preset" << 'EOF'
preset demo
node include lit query demo.warnings
node include lit query demo.timeouts
node signal query demo.speed
node signal query demo.temperature
param ecu = ECU1
param ecu += ECU2
filter selection
search regex throttl(ing|ed)
EOF
w() { cygpath -w "$1"; }
cat > "$work/hero.ps1" << EOF
\$env:APPDATA = '$cfg'
& '$(w "$root/tools/screenshot.ps1")' -Exe '$(w "$gui.exe")' -Out '$(w "$out/nerka.png")' -Wait 4 -Clicks '956,233' -Client -AppArgs @('$(w "$work/demo.dlt")', '--new-window', '--library', '--pack', '$(w "$pack")', '--preset-file', '$(w "$work/hero.nerka-preset")', '--plot', 'speed', '--plot', 'temp', '--marker', 'temp=90:throttling')
EOF
powershell -ExecutionPolicy Bypass -File "$(w "$work/hero.ps1")"
