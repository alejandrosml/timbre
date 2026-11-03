# Releasing Timbre

Releases are signed with the maintainer's Developer ID and notarized by
Apple, on his Mac, with **Timbre Signer** (`tools/signer`): the private key
never leaves that Mac, and no signing secret is stored on GitHub.

1. Bump the version in `Cargo.toml`, `packaging/manifest.json` and
   `Info.plist` (`scripts/check-versions.py` checks them), and date the
   CHANGELOG entry.
2. Build the universal binary and the package:
   `scripts/build-macos-from-linux.sh` (or `cargo build` for both Apple
   targets and `lipo` on a Mac), then `scripts/package-mcpb.sh`.
3. Put `timbre-X.Y.Z.mcpb` in the maintainer's Downloads and call the
   `timbre_signing` tool with `{"action": "sign", "file": "timbre-X.Y.Z.mcpb"}`,
   then `status` until it reports `done`. It signs with the hardened runtime
   and the Apple Events entitlement, notarizes, and writes
   `timbre-X.Y.Z-signed.mcpb`.
4. Check the signature (`codesign -dv --verbose=2`, or `rcodesign
   print-signature-info`), tag `vX.Y.Z`, and publish the release with the
   signed package, the signed binary and `SHA256SUMS`.

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
