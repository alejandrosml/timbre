#!/bin/bash
# Timbre Signer — Developer ID signing and notarization of Timbre, on this Mac.
# Copyright (c) 2026 Alejandro San Martin Lamas — https://github.com/alejandrosml (MIT)
#
#   status                 what is set up, and the end of the last signing log
#   prepare                create the private key (once) and print the CSR (PEM)
#   install                stdin: base64 DER certificate (optional); also takes
#                          Downloads/AuthKey_*.p8 + Downloads/timbre-notary.txt
#   sign-run <file.mcpb>   sign, notarize and repackage (run in the background)
#
# The private key, the signing keychain and the notarization key stay in
# ~/Library/Application Support/Timbre/signing (mode 700). Nothing is sent
# anywhere except Apple's timestamp and notarization services.
set -uo pipefail
export PATH=/usr/bin:/bin:/usr/sbin:/sbin
umask 077

DL="$HOME/Downloads"
DIR="$HOME/Library/Application Support/Timbre/signing"
KEY="$DIR/developer-id.key"
KC="$DIR/timbre-signing.keychain-db"
KCPASS="$DIR/keychain.pass"
NOTARY="$DIR/notary.conf"
LOG="$DIR/last-sign.log"
G2_URL="https://www.apple.com/certificateauthority/DeveloperIDG2CA.cer"
mkdir -p "$DIR" && chmod 700 "$DIR"

die() { echo "error: $*" >&2; exit 1; }
unlock() { [ -f "$KC" ] && security unlock-keychain -p "$(cat "$KCPASS")" "$KC" 2>/dev/null; }
# Not -v: trust is evaluated without this keychain's intermediate certificate.
identity() { unlock && security find-identity -p codesigning "$KC" 2>/dev/null | awk '/Developer ID Application/ {print $2; exit}'; }
identity_name() { security find-identity -p codesigning "$KC" 2>/dev/null | grep -o '"Developer ID Application: [^"]*"' | head -1 | tr -d '"'; }
conf() { sed -n "s/^$1=//p" "$NOTARY" 2>/dev/null | head -1; }

# codesign looks for intermediate certificates in the keychain search list:
# add the signing keychain for the duration of a signature, then restore.
SAVED=()
search_add() {
  local line
  SAVED=()
  while IFS= read -r line; do
    line="${line#"${line%%[![:space:]]*}"}"; line="${line#\"}"; line="${line%\"}"
    [ -n "$line" ] && SAVED+=("$line")
  done < <(security list-keychains -d user)
  security list-keychains -d user -s "$KC" ${SAVED[@]+"${SAVED[@]}"}
}
search_restore() {
  [ ${#SAVED[@]} -gt 0 ] && security list-keychains -d user -s "${SAVED[@]}"
  SAVED=()
}

status() {
  local id; id="$(identity || true)"
  if [ -n "$id" ]; then echo "signing identity: $(identity_name)"
  elif [ -f "$KEY" ]; then echo "signing identity: waiting for the certificate (run install)"
  else echo "signing identity: none (run prepare)"; fi
  if [ -f "$NOTARY" ]; then echo "notarization: key $(conf KEY_ID)"; else echo "notarization: not set up"; fi
  if [ -f "$LOG" ]; then echo "--- last signing log ---"; tail -n 12 "$LOG"; fi
}

prepare() {
  [ -n "$(identity || true)" ] && { echo "Already set up: $(identity_name)"; return 0; }
  if [ ! -f "$KEY" ]; then
    openssl genrsa -out "$KEY" 2048 2>/dev/null || die "cannot create the private key"
    chmod 600 "$KEY"
  fi
  openssl req -new -key "$KEY" -subj "/CN=Alejandro San Martin Lamas/O=Timbre" 2>/dev/null \
    || die "cannot create the certificate signing request"
}

install_certificate() {
  local b64="$1" tmp
  tmp="$(mktemp -d)"
  printf '%s' "$b64" | base64 -D > "$tmp/cert.cer" 2>/dev/null || die "certificate is not valid base64"
  openssl x509 -inform DER -in "$tmp/cert.cer" -out "$tmp/cert.pem" 2>/dev/null || die "not a DER certificate"
  openssl x509 -noout -subject -in "$tmp/cert.pem" | grep -q "Developer ID Application" \
    || die "this is not a Developer ID Application certificate"
  [ -f "$KEY" ] || die "no private key here: run prepare first"
  [ "$(openssl x509 -noout -modulus -in "$tmp/cert.pem" | openssl md5)" = "$(openssl rsa -noout -modulus -in "$KEY" 2>/dev/null | openssl md5)" ] \
    || die "this certificate was not issued for the request made on this Mac"
  local p12pass; p12pass="$(openssl rand -hex 16)"
  openssl pkcs12 -export -inkey "$KEY" -in "$tmp/cert.pem" -name "Timbre Developer ID" \
    -out "$tmp/id.p12" -passout "pass:$p12pass" 2>/dev/null || die "cannot assemble the identity"
  if [ ! -f "$KC" ]; then
    [ -f "$KCPASS" ] || { openssl rand -hex 24 > "$KCPASS"; chmod 600 "$KCPASS"; }
    security create-keychain -p "$(cat "$KCPASS")" "$KC" || die "cannot create the signing keychain"
  fi
  security set-keychain-settings "$KC"   # no automatic locking
  unlock || die "cannot unlock the signing keychain"
  security import "$tmp/id.p12" -k "$KC" -P "$p12pass" -f pkcs12 -T /usr/bin/codesign >/dev/null \
    || die "cannot import the identity"
  security set-key-partition-list -S apple-tool:,apple:,codesign: -s -k "$(cat "$KCPASS")" "$KC" >/dev/null \
    || die "cannot allow codesign to use the key"
  if curl -fsSL --max-time 30 "$G2_URL" -o "$tmp/g2.cer"; then
    security import "$tmp/g2.cer" -k "$KC" >/dev/null 2>&1 || true
  fi
  cp "$tmp/cert.cer" "$DIR/developer-id.cer"
  rm -rf "$tmp"
  echo "Installed: $(identity_name)"
}

install_notary() {
  local p8 ids key_id issuer
  p8="$(ls -t "$DL"/AuthKey_*.p8 2>/dev/null | head -1 || true)"
  ids="$DL/timbre-notary.txt"
  [ -n "$p8" ] && [ -f "$ids" ] || return 0
  key_id="$(sed -n 's/^KEY_ID=\([A-Z0-9]\{8,12\}\)$/\1/p' "$ids" | head -1)"
  issuer="$(sed -n 's/^ISSUER=\([0-9a-f-]\{36\}\)$/\1/p' "$ids" | head -1)"
  [ -n "$key_id" ] && [ -n "$issuer" ] || die "timbre-notary.txt needs KEY_ID=… and ISSUER=… lines"
  local dest; dest="$DIR/$(basename "$p8")"
  mv "$p8" "$dest" && chmod 600 "$dest" || die "cannot move the API key"
  rm -f "$ids"
  printf 'KEY_ID=%s\nISSUER=%s\nKEY_FILE=%s\n' "$key_id" "$issuer" "$dest" > "$NOTARY"
  if xcrun notarytool history --key "$dest" --key-id "$key_id" --issuer "$issuer" >/dev/null 2>&1; then
    echo "Notarization: Apple accepts key $key_id."
  else
    echo "Notarization: Apple refused key $key_id (a team key with the Developer role is needed)." >&2
  fi
}

install() {
  local b64; b64="$(cat)"
  if [ -n "$b64" ]; then install_certificate "$b64"; fi
  install_notary
  status
}

sign_run() {
  local file="$1" tmp out version id
  exec >"$LOG" 2>&1
  echo "started $(date '+%Y-%m-%d %H:%M:%S') — $file"
  [[ "$file" =~ ^timbre-[0-9]+\.[0-9]+\.[0-9]+\.mcpb$ ]] || die "unexpected file name"
  [ -f "$DL/$file" ] || die "$file is not in Downloads"
  id="$(identity || true)"
  [ -n "$id" ] || die "no signing identity: run prepare and install first"
  # TIMBRE_SIGNER_TEST=1 (CI only, with a throw-away identity): no notarization.
  local testing="${TIMBRE_SIGNER_TEST:-0}"
  [ "$testing" = 1 ] || [ -f "$NOTARY" ] || die "notarization is not set up"
  tmp="$(mktemp -d)"
  trap 'search_restore; rm -rf "$tmp"' EXIT
  ditto -x -k "$DL/$file" "$tmp/x" || die "cannot unpack $file"
  grep -q '"name": *"timbre"' "$tmp/x/manifest.json" || die "$file is not a Timbre package"
  version="$(sed -n 's/.*"version": *"\([0-9.]*\)".*/\1/p' "$tmp/x/manifest.json" | head -1)"
  cat > "$tmp/entitlements.plist" <<'PLIST'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>com.apple.security.automation.apple-events</key>
    <true/>
</dict>
</plist>
PLIST
  echo "signing with $(identity_name)"
  search_add
  codesign --force --timestamp --options runtime --identifier io.github.alejandrosml.timbre \
    --entitlements "$tmp/entitlements.plist" --keychain "$KC" --sign "$id" "$tmp/x/server/timbre" \
    || die "codesign failed"
  search_restore
  codesign --verify --strict --verbose=2 "$tmp/x/server/timbre" || die "the signature does not verify"
  if [ "$testing" = 1 ]; then
    out="$DL/timbre-$version-test.mcpb"
    (cd "$tmp/x" && zip -qr -X "$tmp/signed.mcpb" .) && mv "$tmp/signed.mcpb" "$out"
    echo "done (test, not notarized): $out"
    return 0
  fi
  echo "notarizing (Apple usually answers within minutes)…"
  ditto -c -k "$tmp/x/server/timbre" "$tmp/notarize.zip"
  xcrun notarytool submit "$tmp/notarize.zip" --key "$(conf KEY_FILE)" --key-id "$(conf KEY_ID)" \
    --issuer "$(conf ISSUER)" --wait --timeout 30m --output-format json > "$tmp/notary.json" 2>&1
  cat "$tmp/notary.json"; echo
  if ! grep -q '"status" *: *"Accepted"' "$tmp/notary.json"; then
    local sub; sub="$(sed -n 's/.*"id" *: *"\([0-9a-f-]*\)".*/\1/p' "$tmp/notary.json" | head -1)"
    [ -n "$sub" ] && xcrun notarytool log "$sub" --key "$(conf KEY_FILE)" --key-id "$(conf KEY_ID)" --issuer "$(conf ISSUER)"
    die "notarization was not accepted"
  fi
  out="$DL/timbre-$version-signed.mcpb"
  (cd "$tmp/x" && zip -qr -X "$tmp/signed.mcpb" .) || die "cannot repackage"
  mv "$tmp/signed.mcpb" "$out"
  shasum -a 256 "$out"
  echo "done: $out"
}

case "${1:-status}" in
  status) status ;;
  prepare) prepare ;;
  install) install ;;
  sign-run) sign_run "${2:-}" ;;
  *) die "unknown action ${1:-}" ;;
esac
