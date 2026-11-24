#!/bin/bash
# Builds "dist/Mac Smart Cooler.app" and "dist/MacSmartCooler.dmg".
set -euo pipefail
cd "$(dirname "$0")/.."

# Sign with a real identity if one exists (e.g. the free "Apple Development" cert Xcode
# creates for your Apple ID); otherwise ad-hoc. Override with SIGN_ID=...
SIGN_ID="${SIGN_ID:-$(security find-identity -v -p codesigning | awk -F'"' '/Developer ID Application|Apple Development/{print $2; exit}')}"
# The app's team ID is built in: the root install step refuses an app copy that isn't
# signed by this team, so a swapped binary can't ride on the password prompt.
TEAM=""
if [ -n "$SIGN_ID" ]; then
  TEAM=$(security find-certificate -c "$SIGN_ID" -p 2>/dev/null | openssl x509 -noout -subject -nameopt multiline 2>/dev/null | awk -F' = ' '/organizationalUnitName/{print $2; exit}')
fi
if [ -n "$TEAM" ]; then
  echo "Team ID: $TEAM"
elif [ -n "${CI:-}" ]; then
  # A release without a team would skip the helper install's signature check entirely.
  echo "error: no signing team found; refusing to build a release that can't verify itself" >&2
  exit 1
else
  echo "warning: no signing team; install step can't verify the app's signature"
fi

# Check that the signing key is usable before the long build. Seeing the identity is not
# enough: a process in a different security session (e.g. a GitHub runner LaunchAgent with
# SessionCreate) sees the certificate, but codesign can't use the locked private key and
# fails with errSecInternalComponent.
if [ -n "$SIGN_ID" ]; then
  probe=$(mktemp -d)
  cp /usr/bin/true "$probe/probe"
  if ! codesign --force --sign "$SIGN_ID" "$probe/probe" 2>"$probe/err"; then
    cat "$probe/err" >&2
    echo "error: can't sign with \"$SIGN_ID\": its private key isn't usable from this process." >&2
    echo "  Unlock the login keychain. On a self-hosted runner, don't let launchd start it in a" >&2
    echo "  new security session (SessionCreate); see \"Release runner\" in README.md." >&2
    rm -rf "$probe"
    exit 1
  fi
  rm -rf "$probe"
fi

MACSMARTCOOLER_TEAM_ID="$TEAM" cargo build --release

rm -rf dist
APP="dist/Mac Smart Cooler.app"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources" dist/AppIcon.iconset dist/dmg
cp target/release/macsmartcooler "$APP/Contents/MacOS/"
cp packaging/Info.plist "$APP/Contents/"
# the version comes from Cargo.toml (release-please bumps it there)
VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
plutil -replace CFBundleShortVersionString -string "$VERSION" "$APP/Contents/Info.plist"
plutil -replace CFBundleVersion -string "$VERSION" "$APP/Contents/Info.plist"

[ -f assets/icon.png ] || python3 scripts/make-icon.py assets/icon.png
cp assets/icon.png dist/icon.png
for s in 16 32 128 256 512; do
  sips -z $s $s dist/icon.png --out "dist/AppIcon.iconset/icon_${s}x${s}.png" >/dev/null
  d=$((s * 2))
  sips -z $d $d dist/icon.png --out "dist/AppIcon.iconset/icon_${s}x${s}@2x.png" >/dev/null
done
iconutil -c icns dist/AppIcon.iconset -o "$APP/Contents/Resources/AppIcon.icns"

codesign --force --deep --sign "${SIGN_ID:--}" "$APP"
codesign -dv "$APP" 2>&1 | grep -E "^Authority=" | head -1 || true

cp -R "$APP" dist/dmg/
ln -s /Applications dist/dmg/Applications
cp "$APP/Contents/Resources/AppIcon.icns" dist/dmg/.VolumeIcon.icns

# writable image first so we can flag the volume as having a custom icon
hdiutil create -volname "Mac Smart Cooler" -srcfolder dist/dmg -ov -format UDRW dist/rw.dmg >/dev/null
MNT=$(hdiutil attach -nobrowse -noautoopen dist/rw.dmg | awk -F'\t' '/\/Volumes\//{print $NF}')
xattr -wx com.apple.FinderInfo 0000000000000000040000000000000000000000000000000000000000000000 "$MNT"
hdiutil detach "$MNT" >/dev/null
hdiutil convert dist/rw.dmg -format UDZO -o dist/MacSmartCooler.dmg -ov >/dev/null
rm -f dist/rw.dmg

# give the .dmg file itself the app icon too
osascript -l JavaScript -e 'ObjC.import("AppKit"); function run(a){ return $.NSWorkspace.sharedWorkspace.setIconForFileOptions($.NSImage.alloc.initWithContentsOfFile(a[0]), a[1], 0) }' \
  "$(pwd)/$APP/Contents/Resources/AppIcon.icns" "$(pwd)/dist/MacSmartCooler.dmg" >/dev/null

rm -rf dist/dmg dist/AppIcon.iconset
echo "Built: $(pwd)/dist/MacSmartCooler.dmg"
