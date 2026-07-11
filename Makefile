# ZeroZero — canonical build/test/lint commands.
# These targets are the project's standard verification suite.

.PHONY: fmt fmt-check clippy build test test-cli verify

fmt:
	cargo fmt --all

fmt-check:
	cargo fmt --all -- --check

clippy:
	cargo clippy --workspace --all-targets -- -D warnings

build:
	cargo build --workspace

test:
	cargo test --workspace

test-cli:
	cargo test -p zerozero-cli --bin zz

# Full pre-merge gate (matches CI): fmt -> clippy -> test.
verify: fmt-check clippy test
