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
| `tools/policy-fixture` | Builds a witness, proves it, and settles the proof through the contracts |
| `scripts` | Circuit compile and trusted setup helpers |
| `deployments` | Deployed contract IDs and demonstration transaction hashes |
| `docs` | Deploy and demo guide |

## What The Pool Enforces

On every shielded spend the pool checks, in order, that the proof was built
against a Merkle root the pool has held, that the external data hash matches the
`ExtData` supplied with the call, that the public amount matches the external
amount, that the ASP membership root in the proof is the live allowlist root,
that the Groth16 proof verifies against the embedded verification key, and that
no nullifier has been spent before. The circuit binds the spender's key to the
allowlist, so a spender who is not enrolled cannot satisfy both the membership
constraint and the live root.

## Requirements

- Rust toolchain pinned in `rust-toolchain.toml`
- `stellar` CLI for contract builds
- Node and `pnpm` for the circuit tooling

## Checks

```sh
make fmt-check
make check
```

CI runs the same checks plus a wasm build, and separately compiles the circuit
and settles a real proof through the contracts. See
[`.github/workflows/ci.yml`](.github/workflows/ci.yml).

## Testnet Deployment

Deployed on the Stellar test network on 2026-09-04. Contract IDs, every
transaction hash and the deploy steps are in
[`deployments/testnet.json`](deployments/testnet.json) and
[`docs/DEPLOY.md`](docs/DEPLOY.md).

| Contract | ID |
|---|---|
| Pool | `CDYCEX7IXGNNJA4FAVV7WU5KS7RVBAA3RYDTZFOMDWDX36RYV7GWHFTD` |
| ASP membership | `CDWJ6CBJFRAMAKPN46N2LJQNI4WN7DH6OP6CHZSQRABFEYAOTVENCTCG` |
| Groth16 verifier | `CCH3JX7NPMNOR45KKWABCZEQUZ6QFQU4WHN6K3CVPTTG4JGSKH6YIQ6B` |

### Recorded Transactions

Every link goes to [Stellar Expert](https://stellar.expert/explorer/testnet).
The spends carry a real Groth16 proof over the `policy_tx_2_2` circuit,
verified on chain by the verifier contract.

| Step | Result | Transaction |
|---|---|---|
| Register note and encryption keys | accepted | [`4506af9f…`](https://stellar.expert/explorer/testnet/tx/4506af9fcd29fb6d8444b205adb2e2f7e3d95a8d8a369d8c084e7b56319ba9f3) |
| Deposit, first commitment | accepted | [`07d39287…`](https://stellar.expert/explorer/testnet/tx/07d392871ba89a18e2c893264e0c58f993599abf907341b7a873c70281b63a0b) |
| Deposit, second commitment | accepted | [`c6583b6e…`](https://stellar.expert/explorer/testnet/tx/c6583b6e9658c01099100b7ba02d90717f934c1b1cd0931971531c4058cbb03f) |
| Enrol first key in the allowlist | accepted | [`1b3ec86d…`](https://stellar.expert/explorer/testnet/tx/1b3ec86d7bb6c00c447a633f12790d9648a8df81fa6e1f60385d95770659cfad) |
| Enrol second key in the allowlist | accepted | [`8a55a6d0…`](https://stellar.expert/explorer/testnet/tx/8a55a6d00388e9481743045a842976f27478e78a56a5187bc46e3edaaf6e161a) |
| Shielded spend, real proof | accepted | [`2264cf72…`](https://stellar.expert/explorer/testnet/tx/2264cf725519d2647e5d18c0e9b1aa4d617523784942641649bdd3f9a9de0b33) |
| Spend bound to a root that is not the live allowlist | **rejected on chain** | [`2e06159b…`](https://stellar.expert/explorer/testnet/tx/2e06159b2f40eefe1be4da3840cebc58995f1232986d6cdbfca0af7ae2d68827) |
| Spend replaying an already spent nullifier | **rejected on chain** | [`ae61382c…`](https://stellar.expert/explorer/testnet/tx/ae61382c9cefb360cddeb7c745018dfca523c8c5e311d6dc38e958d3239d9f79) |
| Pool upgrade | accepted | [`4aa3985f…`](https://stellar.expert/explorer/testnet/tx/4aa3985f82cabb56e634a16ca42ce7d21e7176841dbd5c7de155985863cf3d1a) |

A second contract set records the same lifecycle from an empty pool through to
the double spend attempt: register, two deposits, two enrolments, an accepted
spend and the rejected replay. Its contract IDs and hashes are under `demoRun`
in the manifest.

Both rejections land on the ledger with `successful: false` and an
`invoke_host_function: trapped` result. Simulation normally catches an invalid
spend before submission, so recording a refusal on chain takes a signed
transaction whose state assumption stops holding before it executes.
[`docs/DEPLOY.md`](docs/DEPLOY.md) describes exactly how each was produced.

### The Same Refusals As Tests

The contract tests cover the refusal paths directly, without the sequencing
work an on-chain recording needs:

| Test | Refuses |
|---|---|
| `transact_rejects_a_spend_whose_asp_root_is_not_the_live_allowlist` | proof bound to a foreign allowlist |
| `transact_rejects_a_stale_asp_root_after_a_new_enrollment` | proof bound to an allowlist that has moved on |
| `transact_rejects_a_replayed_nullifier` | double spend |
| `transact_rejects_unknown_root` | proof bound to a pool root the pool never had |
| `transact_rejects_bad_ext_hash` | external data swapped after proving |
| `transact_rejects_bad_public_amount` | public amount not matching the external amount |
| `a_real_proof_is_rejected_when_the_spender_is_not_enrolled` | real proof, spender never enrolled |

## Circuit

```sh
make install-circom
make compile-policy-circuit
make setup-policy-circuit-keys
```

See [circuits/README.md](circuits/README.md) for the public input order and
the trusted setup limitation.

## Status And Limitations

- Single admin key. No multisig, no timelock, no pause.
- Single-party trusted setup for the proving and verifying keys. Anyone holding
  the setup randomness could forge proofs. A multi-party ceremony is required
  before the pool holds value.
- Merkle depth 10, so 1,024 note commitments per pool.
- Not audited. Testnet only.

## License

Apache-2.0. See [LICENSE](LICENSE).
