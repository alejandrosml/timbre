# Contributing

Thanks for helping. Timbre is a small Rust program; the usual checks apply.

```sh
cargo fmt --check
cargo clippy --all-targets --locked -- -D warnings
cargo test --locked
cargo build && python3 -I tests/e2e_mcp.py target/debug/timbre
```

- Tests run against a synthetic Mail store (`src/fixture.rs`). **Never add real
  mail to the repository**, even redacted.
- Performance changes: measure before and after. A debug build can generate a
  large synthetic store: `timbre dev-corpus /tmp/corpus 20000`, then point
  `TIMBRE_MAIL_ROOT`, `TIMBRE_ACCOUNTS_DB` and `TIMBRE_DATA_DIR` at it and run
  `timbre index --rebuild` and `timbre bench` from a release build.
- Keep tool descriptions short: every tool is listed in Claude's context.
- Versions live in `Cargo.toml`, `packaging/manifest.json` and `Info.plist`;
  `scripts/check-versions.py` checks they agree.
