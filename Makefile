.PHONY: check fmt fmt-check test

check:
	cargo clippy --workspace --all-targets --locked -- -D warnings
	cargo test --workspace --locked

fmt:
	cargo fmt --all

fmt-check:
	cargo fmt --all -- --check

test:
	cargo test --workspace --locked
