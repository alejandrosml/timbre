#!/usr/bin/env bash
# Timbre — sets the GitHub secrets that let the Release workflow sign and
# notarize Timbre with your Apple Developer account.
# Copyright (c) 2026 Alejandro San Martin Lamas — https://github.com/alejandrosml (MIT)
#
# Run it on your Mac:  bash setup-release-signing.sh
#
# You need:
#   1. Your "Developer ID Application" certificate exported as a .p12 file
#      (Keychain Access → My Certificates → right-click → Export…).
#   2. An App Store Connect *team* API key (.p8) and its Issuer ID
#      (App Store Connect → Users and Access → Integrations → Team Keys).
#   3. The GitHub CLI: brew install gh   (this script logs you in if needed).
#
# The files and passwords stay on this Mac: they are checked locally, then
# sent only to GitHub's encrypted secret store with `gh secret set`.
set -euo pipefail

REPO="alejandrosml/timbre"
say() { printf '\n\033[1m%s\033[0m\n' "$*"; }
fail() { printf '\n\033[31m%s\033[0m\n' "$*" >&2; exit 1; }
# Paths dragged into Terminal may be quoted or have escaped spaces.
clean_path() { local p="$1"; p="${p%\"}"; p="${p#\"}"; p="${p%\'}"; p="${p#\'}"; p="${p//\\ / }"; p="${p%"${p##*[![:space:]]}"}"; printf '%s' "$p"; }

command -v gh >/dev/null || fail "GitHub CLI missing. Install it with:  brew install gh"
command -v xcrun >/dev/null || fail "Xcode command-line tools missing. Install them with:  xcode-select --install"
if ! gh auth status >/dev/null 2>&1; then
  say "Log in to GitHub (a browser window opens)…"
  gh auth login --web --git-protocol https
fi

say "1/3  Developer ID certificate"
read -r -p "Drag the .p12 file here, then press Enter: " P12
P12="$(clean_path "$P12")"
[ -f "$P12" ] || fail "File not found: $P12"
read -r -s -p "Password chosen when exporting the .p12: " P12_PASSWORD; echo

# Open the .p12 in a throw-away keychain to check the password and contents.
TMPDIR_SIGN="$(mktemp -d)"
CHECK_KC="$TMPDIR_SIGN/check.keychain-db"
cleanup() { security delete-keychain "$CHECK_KC" >/dev/null 2>&1 || true; rm -rf "$TMPDIR_SIGN"; }
trap cleanup EXIT
security create-keychain -p timbre-check "$CHECK_KC" >/dev/null
security import "$P12" -P "$P12_PASSWORD" -k "$CHECK_KC" -t cert -f pkcs12 >/dev/null 2>&1 \
  || fail "This .p12 does not open with that password."
IDENTITY="$(security find-identity -p codesigning "$CHECK_KC" | grep -o '"Developer ID Application: [^"]*"' | head -1 || true)"
[ -n "$IDENTITY" ] || fail "No \"Developer ID Application\" certificate with its private key in this .p12. Export that certificate (not \"Apple Development\") from Keychain Access."
echo "OK: $IDENTITY"

say "2/3  App Store Connect API key (for notarization)"
read -r -p "Drag the AuthKey_XXXXXXXXXX.p8 file here, then press Enter: " P8
P8="$(clean_path "$P8")"
[ -f "$P8" ] || fail "File not found: $P8"
KEY_ID="$(basename "$P8" | sed -n 's/^AuthKey_\([A-Z0-9]*\)\.p8$/\1/p')"
if [ -z "$KEY_ID" ]; then read -r -p "Key ID (10 characters, shown next to the key): " KEY_ID; fi
read -r -p "Issuer ID (shown above the list of team keys): " ISSUER
echo "Checking the key with Apple…"
xcrun notarytool history --key "$P8" --key-id "$KEY_ID" --issuer "$ISSUER" >/dev/null 2>&1 \
  || fail "Apple refused this key. Use a *team* key (not an individual key), with the Key ID and Issuer ID shown in App Store Connect."
echo "OK: Apple accepts the key."

say "3/3  Saving the secrets in GitHub ($REPO)"
base64 -i "$P12" | gh secret set MACOS_CERTIFICATE -R "$REPO"
printf '%s' "$P12_PASSWORD" | gh secret set MACOS_CERTIFICATE_PASSWORD -R "$REPO"
base64 -i "$P8" | gh secret set APPLE_API_KEY -R "$REPO"
printf '%s' "$KEY_ID" | gh secret set APPLE_API_KEY_ID -R "$REPO"
printf '%s' "$ISSUER" | gh secret set APPLE_API_ISSUER -R "$REPO"
gh secret list -R "$REPO"

say "Done. Releases tagged vX.Y.Z will now be signed with $IDENTITY and notarized by Apple."
echo "You can delete the .p12 and .p8 copies you exported, or keep them somewhere safe."
