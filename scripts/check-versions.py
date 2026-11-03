"""Check that Cargo.toml, packaging/manifest.json and Info.plist agree on the
version (and match the expected one, if given as the first argument)."""
import json
import plistlib
import re
import sys
from pathlib import Path

root = Path(__file__).resolve().parent.parent
cargo = re.search(r'^version\s*=\s*"([^"]+)"', (root / "Cargo.toml").read_text(), re.M).group(1)
manifest = json.loads((root / "packaging/manifest.json").read_text())["version"]
plist = plistlib.loads((root / "Info.plist").read_bytes())
found = {
    "Cargo.toml": cargo,
    "manifest.json": manifest,
    "Info.plist CFBundleShortVersionString": plist["CFBundleShortVersionString"],
    "Info.plist CFBundleVersion": plist["CFBundleVersion"],
}
expected = sys.argv[1] if len(sys.argv) > 1 else cargo
bad = {k: v for k, v in found.items() if v != expected}
if bad:
    print(f"version mismatch, expected {expected}: {bad}")
    sys.exit(1)
print(f"all versions are {expected}")
