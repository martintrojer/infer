.PHONY: check check-fast check-full fmt lint test test-fast test-full build release

# Pre-commit check: fmt + clippy + unit tests (~3s)
check: check-fast

check-fast: fmt lint test-fast

# Full check: fast + ignored integration tests (C dump-textual sweep, etc.)
check-full: fmt lint test-full

fmt:
	cargo fmt --check

lint:
	cargo clippy -- -D warnings

test-fast:
	cargo test

# Includes #[ignore] tests (C sweep spawns infer binary — needs infer built)
test-full:
	cargo test -- --include-ignored

# Build optimized release binary → bin/infer-rs
build: release

release:
	cargo build --release
	mkdir -p bin
	cp target/release/infer-rs bin/infer-rs
