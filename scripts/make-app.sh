#!/usr/bin/env bash
# Wrap the timbre executable in Timbre.app: the name and icon macOS shows in
# System Settings (Full Disk Access) and in Finder.
# Usage: scripts/make-app.sh <timbre executable> <directory to put Timbre.app in>
set -euo pipefail
cd "$(dirname "$0")/.."
BIN="$1"
APP="$2/Timbre.app"
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp Info.plist "$APP/Contents/Info.plist"
printf 'APPL????' > "$APP/Contents/PkgInfo"
cp assets/Timbre.icns "$APP/Contents/Resources/Timbre.icns"
cp "$BIN" "$APP/Contents/MacOS/timbre"
chmod 755 "$APP/Contents/MacOS/timbre"
echo "made $APP"
