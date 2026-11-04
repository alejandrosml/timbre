#!/usr/bin/env bash
# Assemble and pack the Claude Desktop extension (.mcpb) around Timbre.app.
# Usage: scripts/package-mcpb.sh [Timbre.app | timbre executable]
#   An executable (by default the universal release build) is first wrapped
#   in a new, unsigned Timbre.app; an app (signed) is packed as it is.
set -euo pipefail
cd "$(dirname "$0")/.."
SRC="${1:-target/universal2-apple-darwin/release/timbre}"
VERSION=$(python3 -I -c "import json;print(json.load(open('packaging/manifest.json'))['version'])")
STAGE=build/mcpb
rm -rf "$STAGE" && mkdir -p "$STAGE/server"
if [ -d "$SRC" ]; then
  cp -Rp "$SRC" "$STAGE/server/Timbre.app"
else
  scripts/make-app.sh "$SRC" "$STAGE/server" > /dev/null
fi
chmod 755 "$STAGE/server/Timbre.app/Contents/MacOS/timbre"
cp packaging/manifest.json "$STAGE/manifest.json"
cp assets/icon.png "$STAGE/icon.png"
cp LICENSE "$STAGE/LICENSE" 2>/dev/null || true
mkdir -p dist
mcpb validate "$STAGE/manifest.json"
mcpb pack "$STAGE" "dist/timbre-$VERSION.mcpb"
