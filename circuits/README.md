# Policy Circuit Sources

This directory carries the Circom source closure for the `policy_tx_2_2`
circuit that the pool verifier boundary checks proofs against.

The circuit proves, for each input note, that the spender knows the note
secret, that the note commitment is in the pool Merkle tree, that the
nullifier is correctly derived, and that the spender's public key is enrolled
in the ASP membership tree. It does not carry a blocklist or non-membership
gate.

The protocol direction is inspired by Nethermind's Stellar Private Payments
research implementation.

## Layout

- `src/policy_tx_2_2.circom`: entry point, 2 inputs and 2 outputs, tree depth 16
- `src/policyTransaction.circom`: the transaction and policy constraints
- `src/keypair.circom`: note key derivation and signature
- `src/merkleProof.circom`: Merkle inclusion
- `src/poseidon2/*`: Poseidon2 over BN254
- `circomlib.lock`: pinned `circomlib` revision
- `keys/*`: proving and verifying keys for the current circuit

## Public Input Order

The verifier contract and every client must serialise public inputs in this
order:

1. `root`, the pool Merkle root the proof was built against
2. `publicAmount`
3. `extDataHash`
4. `inputNullifier[0..nIns]`
5. `outputCommitment[0..nOuts]`
6. `membershipRoots`, one ASP membership root per input

For the 2-in/2-out instance that is 9 public inputs.

## Rebuilding

```sh
make fetch-circomlib
make install-circom
make compile-policy-circuit
make setup-policy-circuit-keys
```

The compile step reports the constraint count. The setup step downloads the
Powers of Tau file matching that count, runs the Groth16 setup, and writes the
proving and verifying keys.

snarkjs sizes the setup by the total constraint count, linear constraints
included. At depth 16 the circuit has 34,894 constraints, so the setup needs
the 2^16 Powers of Tau file.

## Powers Of Tau

Phase 1 is generated locally by `make generate-powers-of-tau`. The published
Hermez transcript is normally reused, but every mirror of it currently returns
AccessDenied, including the URLs snarkjs still documents. The local transcript
uses the hash of Stellar mainnet ledger 64411842, closed 2026-09-13T16:36:31Z,
as its final beacon, and `snarkjs powersoftau verify` passes on it.

This makes both phases single-party. Point `PTAU_FILE` at a published
multi-party transcript and re-run the setup once one is reachable again.

## Passing A Bus To The Witness Calculator

`MembershipProof` is a circom bus, and the witness calculator takes a bus as a
flat array in declaration order:

```
[leaf, blinding, pathElements[0..levels], pathIndices]
```

An object keyed by field name is rejected with "Not enough values for input
signal membershipProofs". The error only appears when a proof is generated, not
when the input is written, so a client can ship a witness that never works.
`circuits/fixtures/policy_tx_2_2_input.json` uses the array form and proves
directly with snarkjs.

## Trusted Setup Limitation

The committed keys come from a single-party setup performed locally with one
contribution. Anyone who kept the setup randomness could forge proofs. That is
acceptable for testnet and for review, and it is not acceptable for mainnet
value. A multi-party ceremony is required before the pool holds real funds.
