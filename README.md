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
| `contracts/pool` | Shielded pool: storage, admin surface, deposit, shielded transact, and the full-tree exit |
| `contracts/verifier` | Groth16 verifier over BN254, embeds the circuit verifying key at build time |
| `contracts/mock-verifier` | Demo-only verifier fallback for local tests, performs no verification |
| `circuit-keys` | Key parsing helpers shared by the verifier build script |
| `contracts/asp-membership` | Association Set Provider allowlist read by the pool on every spend |
| `contracts/faucet` | Testnet onboarding: holds the allowlist admin role and enrols testers under a cooldown |
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

The same checks run on `exit`, the entrypoint that spends out of a pool whose
tree is full. It differs from `transact` in one respect: the output commitments
are discarded rather than inserted, which is what lets it run with no room left.

## Authorization Model

The pool has one admin address, set at construction. Only that address can
rotate the admin, repoint the verifier or the allowlist, or upgrade the
contract wasm, and all four are covered by tests that drop every authorization
and assert the call is refused. Deposits and spends require the funding
account's own signature. There is no operator role and no mint allowance: an
address is either the admin or an ordinary user.

The allowlist contract has its own admin. By default only that admin may enrol
a key; `set_admin_insert_only(false)` opens enrolment to anyone, which is
useful on testnet.

A spender who is not enrolled cannot move a note at all, so a shared testnet
needs a way in that does not involve a human. `contracts/faucet` takes the
allowlist admin role and enrols a caller under a per-address cooldown, and
mints the pool token as well when it is the admin of that token's Stellar Asset
Contract. It grants admission to anyone who asks, which is a testnet
convenience and not a compliance posture.

### External Calls

The token contract is code this pool does not own. Both paths that call it now
finish their state changes first: `deposit` records the commitment before
pulling funds, and a withdrawal inserts the output commitments and spends the
nullifiers before paying anyone. A regression test drives a token that
re-enters `deposit` and asserts the commitment is not inserted twice.

## Client Views

Two views exist so a client can build a proof the pool will accept, rather than
reimplementing the pool's encoding and hoping it matches:

| View | Returns |
|---|---|
| `get_ext_data_hash(ext_data)` | the binding hash the proof must carry |
| `get_public_amount(ext_amount)` | the field encoding of a deposit or withdrawal amount |

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

Deployed on the Stellar test network on 2026-09-14, at circuit depth 16.
Contract IDs, every transaction hash and the deploy steps are in
[`deployments/testnet.json`](deployments/testnet.json) and
[`docs/DEPLOY.md`](docs/DEPLOY.md). The earlier depth-10 deployment and its
recorded runs are kept in the manifest under `supersededDepth10Deployment`.

| Contract | ID |
|---|---|
| Pool | `CDJELV6HUP6BKVWBUIFR7GEJPQDQRV5PCSUTEODOKSGTJPUD7DZ6B66G` |
| ASP membership | `CBQXK3FHJDQ3SM2Q6OT3NU527B3TY6YWBCBMM4NSV3ZYNUHW6YN5W2UD` |
| Groth16 verifier | `CDKW7LEKFSFNOB36GDI5QUI5PONPECIJZAE2EBRYJYP3G3YJQJBLU53F` |
| Faucet | `CBSPSGKVO77BKK357ECZX2LW5PCAGK2QHOPNHNA3Y2PNPTYWA2UWULIU` |

The allowlist admin role is held by the faucet, so a tester enrols themselves.
Both roots were checked against the proof before the spend was submitted, and
the allowlist root was built entirely through faucet calls.

| Step | Transaction |
|---|---|
| Allowlist admin handed to the faucet | [`bf3fb8c6…`](https://stellar.expert/explorer/testnet/tx/bf3fb8c64e6cfbbc4b571cb98ab01268d20069c56ca49ee747d655bb174fdd24) |
| Deposit | [`49f31aa2…`](https://stellar.expert/explorer/testnet/tx/49f31aa2a1065065dcdb233b5444837d0114e5d3a5773e981c1d049317911ffc) |
| Self-onboard through the faucet | [`04d3958a…`](https://stellar.expert/explorer/testnet/tx/04d3958a664723c4f64b3250cd59427a2ca2de47393a993e2936283fea8bde40) |
| Shielded spend, real depth-16 proof | [`64afcf21…`](https://stellar.expert/explorer/testnet/tx/64afcf21c7adbd1bd144db3b978da85a1a510d1785a8ac39c193ed3b0766f428) |

### Recorded Transactions From The Depth-10 Deployment


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

### Withdrawal With A Real Proof

A separate pool configured with a 1000 bps protocol fee recorded a withdrawal
of 10 stroops carrying a real proof:
[`12766d49…`](https://stellar.expert/explorer/testnet/tx/12766d4903289db15c5971fe1e10bb88d2bd4db0c287600773c0cc64b49692a1).
The pool paid 9 to the recipient and 1 to the fee recipient, and the settlement
event records the external amount as `-10`.

The circuit enforces `sumIns + publicAmount == sumOuts`, so a withdrawal
shrinks the shielded output by the amount leaving the pool. The pool derives the
same public amount from `ExtData` through `get_public_amount`, and the proof
verifies only if the two agree. Both sides were checked against each other
before the spend was submitted.

### Full Entrypoint Exercise

A third contract set was driven through every entrypoint, 19 transactions, each
outcome checked against Horizon. It covers key registration, six deposits, four
enrolments, opening and closing the allowlist to non-admin enrolment, an
accepted shielded spend, and admin repointing of the verifier and the allowlist.
Two refusals were recorded on chain: a replayed nullifier and a duplicate
commitment. Three more refusals never reach the ledger because simulation
rejects them first: a deposit above the configured maximum, a zero-amount
deposit, and a non-admin enrolment while the allowlist is admin-only. All of it
is listed under `exerciseRun` in the manifest.

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
| `exit_is_refused_while_transact_still_works` | forfeiting outputs while the normal path is open |
| `exit_refuses_a_deposit` | value paid into a pool that would discard the note |
| `exit_refuses_a_transfer_that_moves_no_money_out` | private send whose value lives only in the discarded outputs |
| `exit_rejects_a_replayed_nullifier` | double spend through the exit path |
| `exit_rejects_an_unknown_root` | exit proof bound to a root the pool never had |
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
- Merkle depth 16, so 65,536 leaves per pool. A deposit uses two leaves and a
  shielded transaction uses two, so that is 32,768 operations. Depth 16 is the
  ceiling the pool constructor can create in one transaction: it writes two
  ledger entries per level and Soroban allows 50 writes. Going deeper needs the
  per-level arrays packed into single entries.
- A full tree closes `transact`, because every shielded transaction writes two
  output commitments. `exit` is the way out of a full pool: same proof and
  nullifier checks, the recipient is paid, and the transaction's output notes
  are discarded instead of inserted. A caller must therefore spend their inputs
  in full, two notes per call, and the pool pays out less than it took in and
  never more. It is refused while `transact` still works, and refused for
  anything that is not a withdrawal. `is_tree_full` and `remaining_leaves` say
  which path applies. Capacity itself is unchanged: this keeps a full pool from
  trapping the notes inside it, it does not make the pool bigger.
- Not audited. Testnet only.

## License

Apache-2.0. See [LICENSE](LICENSE).
