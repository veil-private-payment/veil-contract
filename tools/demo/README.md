# veil-demo

A command-line client that makes a private payment on testnet using only the
read API (`services/api`) for everything it learns about the pool. It is the
evidence run for the read API: if the API served a wrong root, path, note or
spent check, the circuit or the pool would refuse the payment.

What runs where:

| Step | Where |
|---|---|
| Keys (note key, X25519 key) | Generated locally, kept in `.veil-demo/<name>.json` (owner-readable, testnet only) |
| Pool root, Merkle paths, allowlist path, notes, spent checks, receiving keys | The read API |
| Note encryption | Locally, format v2 (`docs/NOTE_ENCRYPTION.md`) |
| Groth16 proof | Locally, with fresh randomness, against `circuits/keys` |
| Signing and submitting | The `stellar` CLI, with a fresh friendbot-funded account per wallet |

The tool never holds a Stellar secret: each wallet's account lives in the
`stellar` CLI under the alias `veil-demo-<name>`.

## Requirements

- The compiled circuit: `make compile-policy-circuit` (installs circom 2.2.3
  and writes `target/circuits-artifacts/manual`).
- The `stellar` CLI with the `testnet` network.
- The read API URL, by default the testnet deployment.

## Run

```sh
cargo build --release -p veil-demo
alias veil-demo=target/release/veil-demo

veil-demo new alice          # funded account + fresh note keys
veil-demo new bob
veil-demo enroll alice       # allowlist entry (faucet) + published receiving keys
veil-demo enroll bob
veil-demo shield alice 10    # 10 XLM from alice's account into a private note
veil-demo pay alice bob 4    # private payment: 4 to bob, 6 back to alice
veil-demo balance bob        # bob finds his note by scanning the API
veil-demo balance alice
```

Every command that writes prints its transaction on Stellar Expert and waits
until the API has indexed it.

## What the payment reveals

The payment's transaction is signed by alice's account, so the sender is
public. Its arguments carry two commitments, two nullifiers, two notes
encrypted to their recipients, a zero external amount and a proof. Bob's
address and the amount appear nowhere in it, and only bob can open his note.

## Recorded run (testnet, 2026-10-30)

| Step | Transaction |
|---|---|
| Enrol alice (allowlist, keys) | [`8cbce410`](https://stellar.expert/explorer/testnet/tx/8cbce41036c24782712a7e2f064108408afd992a1cf9ada1381ee595229dfd2c), [`53e5b51a`](https://stellar.expert/explorer/testnet/tx/53e5b51a2f0e0b8039d16a9632abd50f16c7e9a9074468d5086436f7a0ff590e) |
| Enrol bob | [`4fcc5157`](https://stellar.expert/explorer/testnet/tx/4fcc515725d413ad4b8e9fb0223ed2439b0ff5aa750b9a73b9823a6a072711c7), [`86d8d308`](https://stellar.expert/explorer/testnet/tx/86d8d308f809385ed620bd5d8aeba2a0815fae2994999d20377e1b6260f59e22) |
| Shield 10 XLM (alice) | [`a2044cf3`](https://stellar.expert/explorer/testnet/tx/a2044cf3937e973ffbbff099dbdfd6a8a3cfdcb12b572b76d2cf3843ec5f3105) |
| Pay 4 XLM privately (alice to bob) | [`c2fc836e`](https://stellar.expert/explorer/testnet/tx/c2fc836e065a5fe45e97b06a67f8facbdb3f0655184754f0b3c5f49d5b325772) |

After the payment `veil-demo balance bob` showed 4 XLM in one note (leaf 36)
and `veil-demo balance alice` 6 XLM (leaf 37).

## Tests

```sh
cargo test -p veil-demo
```

- Unit tests pin the note format (round trip, every tampered byte refused,
  legacy notes skipped, low-order keys refused, a test vector) and check the
  external data hash against a value the deployed pool returned.
- `tests/end_to_end.rs` shields and then pays privately through the real
  pool, allowlist and Groth16 verifier contracts, with paths from the same
  tree code the API uses: both proofs settle, a double spend is refused, and
  only the recipient opens the payment note.
