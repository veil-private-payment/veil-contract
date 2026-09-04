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

CI runs the same checks plus a wasm build, and separately compiles the circuit
and settles a real proof through the contracts. See
[`.github/workflows/ci.yml`](.github/workflows/ci.yml).

## Testnet Deployment

Deployed on the Stellar test network on 2026-09-04. Contract IDs and the
demonstration transaction hashes are in
[`deployments/testnet.json`](deployments/testnet.json); the steps that produced
them are in [`docs/DEPLOY.md`](docs/DEPLOY.md).

| | |
|---|---|
| Pool | `CDYCEX7IXGNNJA4FAVV7WU5KS7RVBAA3RYDTZFOMDWDX36RYV7GWHFTD` |
| ASP membership | `CDWJ6CBJFRAMAKPN46N2LJQNI4WN7DH6OP6CHZSQRABFEYAOTVENCTCG` |
| Groth16 verifier | `CCH3JX7NPMNOR45KKWABCZEQUZ6QFQU4WHN6K3CVPTTG4JGSKH6YIQ6B` |

Demonstration transactions on
[Stellar Expert](https://stellar.expert/explorer/testnet):

| Step | Transaction |
|---|---|
| Deposit | [`07d39287…`](https://stellar.expert/explorer/testnet/tx/07d392871ba89a18e2c893264e0c58f993599abf907341b7a873c70281b63a0b) |
| ASP-gated spend with a real proof | [`2264cf72…`](https://stellar.expert/explorer/testnet/tx/2264cf725519d2647e5d18c0e9b1aa4d617523784942641649bdd3f9a9de0b33) |
| Spend rejected by the allowlist gate | [`2e06159b…`](https://stellar.expert/explorer/testnet/tx/2e06159b2f40eefe1be4da3840cebc58995f1232986d6cdbfca0af7ae2d68827) |

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
