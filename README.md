# Veil Contracts

Soroban contracts and Circom circuit for Veil, a compliance-gated shielded
pool on Stellar.

This repository holds the settlement layer only: the shielded pool contract,
the Association Set Provider membership contract that gates every spend, the
Groth16/BN254 verifier contract, and the circuit that produces the proofs they
check. The client SDK, frontend, indexer, and relayer live elsewhere and are
out of scope here.

The protocol direction is inspired by Nethermind's Stellar Private Payments
research implementation.

## Layout

| Path | Role |
|---|---|
| `contracts/pool` | Shielded pool: storage, admin surface, deposit and shielded transact |
| `contracts/verifier` | Groth16 verifier over BN254, embeds the circuit verifying key at build time |
| `contracts/mock-verifier` | Demo-only verifier fallback for local tests, performs no verification |
| `circuit-keys` | Key parsing helpers shared by the verifier build script |
| `contracts/asp-membership` | Association Set Provider allowlist read by the pool on every spend |
| `contracts/types` | Shared contract types |
| `contracts/soroban-utils` | BN254 and Groth16 helpers for contract code |
| `circuits` | Circom policy transaction circuit, plus its proving and verifying keys |
| `poseidon2` | Poseidon2 hash used by the contracts and the circuit |

More crates land as the milestone progresses.

## Requirements

- Rust toolchain pinned in `rust-toolchain.toml`
- `stellar` CLI for contract builds

## Checks

```sh
make fmt-check
make check
```

## Circuit

```sh
make install-circom
make compile-policy-circuit
make setup-policy-circuit-keys
```

See [circuits/README.md](circuits/README.md) for the public input order and
the trusted setup limitation.

## License

Apache-2.0. See [LICENSE](LICENSE).
