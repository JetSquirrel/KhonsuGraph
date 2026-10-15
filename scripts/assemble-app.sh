#!/bin/bash
# Lay out KhonsuGraph.app from release binaries.
#
# Usage: scripts/assemble-app.sh <dir with the release binaries> <out.app> [version]
#
# Contents/MacOS: khonsugraph (the app), khonsu (the CLI) and
# khonsu-plugin-<id> for each official plugin, with its manifest under
# Contents/MacOS/plugins/<id>/ so its declared permissions travel with it.
set -euo pipefail

BIN="$1"
APP="$2"
VERSION="${3:-}"
PLUGINS="csv aws"

rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"

for exe in khonsugraph khonsu; do
  cp "$BIN/$exe" "$APP/Contents/MacOS/$exe"
  chmod +x "$APP/Contents/MacOS/$exe"
done
for id in $PLUGINS; do
  mkdir -p "$APP/Contents/MacOS/plugins/$id"
  cp "$BIN/khonsu-plugin-$id" "$APP/Contents/MacOS/plugins/$id/"
  chmod +x "$APP/Contents/MacOS/plugins/$id/khonsu-plugin-$id"
  cp "plugins/$id/khonsu-plugin.json" "$APP/Contents/MacOS/plugins/$id/"
done

cp app/assets/AppIcon.icns "$APP/Contents/Resources/AppIcon.icns"
cp app/assets/Info.plist "$APP/Contents/Info.plist"
if [ -n "$VERSION" ]; then
  plutil -replace CFBundleShortVersionString -string "$VERSION" "$APP/Contents/Info.plist"
  plutil -replace CFBundleVersion -string "$VERSION" "$APP/Contents/Info.plist"
fi
