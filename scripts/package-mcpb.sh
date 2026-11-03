#!/usr/bin/env bash
# Assemble and pack the Claude Desktop extension (.mcpb) from a built,
# signed universal binary. Usage: scripts/package-mcpb.sh [binary]
set -euo pipefail
cd "$(dirname "$0")/.."
BIN="${1:-target/universal2-apple-darwin/release/timbre}"
VERSION=$(python3 -I -c "import json;print(json.load(open('packaging/manifest.json'))['version'])")
STAGE=build/mcpb
rm -rf "$STAGE" && mkdir -p "$STAGE/server"
cp packaging/manifest.json "$STAGE/manifest.json"
cp assets/icon.png "$STAGE/icon.png"
cp LICENSE "$STAGE/LICENSE" 2>/dev/null || true
cp "$BIN" "$STAGE/server/timbre" && chmod 755 "$STAGE/server/timbre"
mkdir -p dist
mcpb validate "$STAGE/manifest.json"
mcpb pack "$STAGE" "dist/timbre-$VERSION.mcpb"
