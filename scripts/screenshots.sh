#!/bin/sh
# Captures every screen of the app from made-up sample data (nothing reads or changes the fans,
# and saved settings are not changed).
#   scripts/screenshots.sh [dir]   light and dark at 85%, 100% and 150% (default: target/screenshots)
#   scripts/screenshots.sh docs    the README pictures in docs/screenshots/
# Keep the Mac awake and leave it alone while this runs: the window comes to the front for each
# picture, because macOS does not redraw a covered window.
set -e
cd "$(dirname "$0")/.."
cargo build

shoot() { # dir theme zoom
  mkdir -p "$1"
  MACSMARTCOOLER_SAMPLE=1 MACSMARTCOOLER_THEME="$2" MACSMARTCOOLER_ZOOM="$3" \
    MACSMARTCOOLER_SNAPSHOT_DIR="$1" ./target/debug/macsmartcooler 2>/dev/null
}

if [ "$1" = docs ]; then
  OUT="docs/screenshots"
  TMP="$(mktemp -d)"
  mkdir -p "$OUT"
  shoot "$TMP/dark" dark 1.0
  shoot "$TMP/light" light 1.0
  cp "$TMP/dark/1-main.png"        "$OUT/main-dark.png"
  cp "$TMP/light/1-main.png"       "$OUT/main-light.png"
  cp "$TMP/dark/3-fan-sensor.png"  "$OUT/custom-fan.png"
  cp "$TMP/dark/5-preferences.png" "$OUT/preferences.png"
  cp "$TMP/dark/6b-processes.png"  "$OUT/processes.png"
  # The captures are Retina-sized (2 pixels per point): bring them to the window's size in
  # points, 1140 x 640, to keep the repository small.
  for f in "$OUT"/*.png; do
    sips -z 640 1140 "$f" >/dev/null
  done
  rm -rf "$TMP"
  echo "Wrote $(ls "$OUT" | wc -l | tr -d ' ') screenshots to $OUT"
  exit 0
fi

OUT="${1:-target/screenshots}"
for theme in light dark; do
  for zoom in 0.85 1.0 1.5; do
    shoot "$OUT/$theme-$zoom" "$theme" "$zoom"
  done
done
echo "Wrote screenshots to $OUT"
