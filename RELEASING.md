# Releasing Timbre

Releases are signed with the maintainer's Developer ID and notarized by
Apple, on his Mac, with **Timbre Signer** (`tools/signer`): the private key
never leaves that Mac, and no signing secret is stored on GitHub.

1. Bump the version in `Cargo.toml`, `packaging/manifest.json` and
   `Info.plist` (`scripts/check-versions.py` checks them), and date the
   CHANGELOG entry.
2. Build the universal binary and the package:
   `scripts/build-macos-from-linux.sh` (or `cargo build` for both Apple
   targets and `lipo` on a Mac), then `scripts/package-mcpb.sh`. Since 0.5
   the package holds `server/Timbre.app` (built by `scripts/make-app.sh`
   from `Info.plist` and `assets/Timbre.icns`), so System Settings and
   Finder show Timbre's name and icon. `scripts/make-icons.py` redraws the
   icons from one description.
3. Put `timbre-X.Y.Z.mcpb` in the maintainer's Downloads and call the
   `timbre_signing` tool with `{"action": "sign", "file": "timbre-X.Y.Z.mcpb"}`,
   then `status` until it reports `done`. It signs Timbre.app with the
   hardened runtime and the Apple Events entitlement, notarizes it, staples
   the ticket, and writes `timbre-X.Y.Z-signed.mcpb` (Timbre Signer 0.2 or
   later; 0.1 only signs the bare executable of earlier packages).
4. Check the signature (`codesign -dv --verbose=2`, or `rcodesign
   print-signature-info`), tag `vX.Y.Z`, and publish the release with the
   signed package.

Setting up Timbre Signer on a new Mac: install `timbre-signer-*.mcpb`, call
`prepare` (it prints a certificate signing request), create a *Developer ID
Application* certificate from it at developer.apple.com, and call `install`
with the certificate (base64). For notarization, put an App Store Connect
*team* API key (`AuthKey_*.p8`, Developer role) and a `timbre-notary.txt`
with `KEY_ID=…` and `ISSUER=…` lines in Downloads, then call `install` again.

The Release workflow can instead sign in CI if the repository has the
secrets listed at the top of `.github/workflows/release.yml`
(`scripts/setup-release-signing.sh` sets them); without them it does
nothing.
