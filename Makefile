.PHONY: all build test check clippy fmt fmt-check clean doc

all: check test

# Build the library
build:
	cargo build

# Run all tests
test:
	cargo test

# Run tests with output
test-verbose:
	cargo test -- --nocapture

# Type check without building
check:
	cargo check

# Run clippy lints
clippy:
	cargo clippy --all-targets --all-features -- -D warnings

# Format code
fmt:
	cargo fmt

# Check formatting
fmt-check:
	cargo fmt --all -- --check

# Build documentation
doc:
	cargo doc --no-deps --all-features

# Clean build artifacts
clean:
	cargo clean

# Run all CI checks
ci: fmt-check clippy test

