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

- `src/policy_tx_2_2.circom`: entry point, 2 inputs and 2 outputs, tree depth 10
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

## Trusted Setup Limitation

The committed keys come from a single-party setup performed locally with one
contribution. Anyone who kept the setup randomness could forge proofs. That is
acceptable for testnet and for review, and it is not acceptable for mainnet
value. A multi-party ceremony is required before the pool holds real funds.
