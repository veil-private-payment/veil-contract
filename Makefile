.PHONY: check fmt fmt-check test install-circom fetch-circomlib compile-policy-circuit setup-policy-circuit-keys

check:
	cargo clippy --workspace --all-targets --locked -- -D warnings
	cargo test --workspace --locked

fmt:
	cargo fmt --all

fmt-check:
	cargo fmt --all -- --check

test:
	cargo test --workspace --locked

install-circom:
	./scripts/install-circom.sh

fetch-circomlib:
	./scripts/fetch-circomlib.sh

compile-policy-circuit:
	./scripts/compile-policy-circuit.sh

setup-policy-circuit-keys:
	./scripts/setup-policy-circuit-keys.sh
