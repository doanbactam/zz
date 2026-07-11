# Contributing to ZeroZero

Thanks for your interest in ZeroZero! Here's a quick guide to contributing.

## Requirements

- Rust 1.86+ (edition 2024)
- Linux or macOS (sandbox uses Landlock/seccomp on Linux, seatbelt on macOS)

## Development

```bash
# Build
cargo build --workspace

# Test
cargo test --workspace

# Lint — must be clean
cargo fmt --check
cargo clippy --all-targets -- -D warnings

# Eval suite (structure validation)
./eval/run.sh
```

## Code conventions

- **Rust idiomatic.** No `unsafe` unless necessary and justified.
- **Error handling:** `thiserror` for library crates, `anyhow` at binary entry point. Propagate with `?`, no `unwrap()`/`expect()` outside tests.
- **Async:** `tokio` runtime, `JoinSet` + `CancellationToken` for structured concurrency. No orphan tasks.
- **Compact code:** collapse duplicate branches, avoid unnecessary nesting.
- **`cargo fmt` is law.** `clippy -D warnings` is law.
- **No new dependencies** if std or existing deps can do the job.
- **Commit messages:** clean, describe "why". Every commit must build (bisectable).

## Test conventions

- E2E tests run the real binary (`assert_cmd`), located in `crates/cli/tests/e2e/`.
- LLM tests use `wiremock` mock server.
- TUI tests use ratatui `TestBackend` + `insta` snapshot.
- No hardcoding expected values, no adding if branches just to pass tests.

## Contributing workflow

1. Fork the repo, create a feature branch.
2. Write tests for your changes (if applicable).
3. Ensure `cargo fmt`, `cargo clippy --all-targets -- -D warnings`, `cargo test --workspace` pass.
4. Open a PR with a clear description: **what** changed, **why**, and **test plan**.

## Bug reports

Open a GitHub Issue with:
- Bug description
- Steps to reproduce
- Expected vs actual behavior
- `zz version` output
- OS + Rust version
