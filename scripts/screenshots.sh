#!/bin/sh
# Captures every screen of the app in light and dark at 85%, 100% and 150% zoom, from made-up
# sample data (nothing reads or changes the fans, and saved settings are not changed).
# Usage: scripts/screenshots.sh [output dir]   (default: target/screenshots)
# Keep the Mac awake and leave it alone while this runs (about two minutes): the window comes
# to the front for each picture, because macOS does not redraw a covered window.
set -e
cd "$(dirname "$0")/.."
cargo build
OUT="${1:-target/screenshots}"
mkdir -p "$OUT"
for theme in light dark; do
  for zoom in 0.85 1.0 1.5; do
    mkdir -p "$OUT/$theme-$zoom"
    MACSMARTCOOLER_SAMPLE=1 MACSMARTCOOLER_THEME="$theme" MACSMARTCOOLER_ZOOM="$zoom" \
      MACSMARTCOOLER_SNAPSHOT_DIR="$OUT/$theme-$zoom" ./target/debug/macsmartcooler 2>/dev/null
  done
done
echo "Wrote screenshots to $OUT"
