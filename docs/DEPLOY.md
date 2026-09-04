# Testnet Deploy And Demo

Reproduces the deployment recorded in
[`deployments/testnet.json`](../deployments/testnet.json).

## Prerequisites

- `stellar` CLI 26.x
- Rust toolchain from `rust-toolchain.toml`
- `pnpm install` for the circuit tooling

## 1. Identity

```sh
stellar keys generate veil-instaward-1 --network testnet --fund
stellar keys address veil-instaward-1
```

This key becomes the contract admin. It can rotate the admin, repoint the
verifier and ASP contracts, and enroll into the allowlist.

## 2. Build

```sh
stellar contract build
```

Writes `target/wasm32v1-none/release/{pool,asp_membership,circom_groth16_verifier}.wasm`.

## 3. Deploy

```sh
stellar contract deploy --wasm target/wasm32v1-none/release/circom_groth16_verifier.wasm \
  --source veil-instaward-1 --network testnet

stellar contract deploy --wasm target/wasm32v1-none/release/asp_membership.wasm \
  --source veil-instaward-1 --network testnet \
  -- --admin <ADMIN> --levels 10

stellar contract deploy --wasm target/wasm32v1-none/release/pool.wasm \
  --source veil-instaward-1 --network testnet \
  -- --admin <ADMIN> --token <TOKEN_SAC> --verifier <VERIFIER> \
     --asp_membership <ASP> --maximum_deposit_amount 1000000000 \
     --fee_recipient <ADMIN> --fee_bps 0 --levels 10
```

The token used for the recorded run is the native XLM SAC, from
`stellar contract id asset --asset native --network testnet`.

The verifier takes no constructor arguments: its verification key is embedded
at build time from `circuits/keys/policy_tx_2_2_vk.json`.

## 4. Bind A Proof To The Live Pool

The pool derives `extDataHash` from the XDR encoding of its own `ExtData`, so a
proof has to be generated against the value the deployed pool reports.

```sh
stellar contract invoke --id <POOL> --source veil-instaward-1 --network testnet \
  -- get_ext_data_hash --ext_data '{ "encrypted_output0": "", "encrypted_output1": "", "ext_amount": "0", "recipient": "<ADMIN>" }'

make compile-policy-circuit
cargo run -p policy-fixture --bin testnet-fixture -- <EXT_DATA_HASH_HEX>
```

The second command prints the input commitments, the ASP membership leaves, the
proof bytes, and every public input as decimal strings.

## 5. Rebuild The State The Proof Assumes

Deposit the input commitments in order, then enroll the membership leaves in
order:

```sh
stellar contract invoke --id <POOL> --source veil-instaward-1 --network testnet --send=yes \
  -- deposit --from <ADMIN> --amount 1 --commitment <COMMITMENT>

stellar contract invoke --id <ASP> --source veil-instaward-1 --network testnet --send=yes \
  -- insert_leaf --leaf <MEMBERSHIP_LEAF>
```

Check the roots agree before spending:

```sh
stellar contract invoke --id <POOL> --source veil-instaward-1 --network testnet -- get_root
stellar contract invoke --id <POOL> --source veil-instaward-1 --network testnet -- get_asp_membership_root
```

## 6. Spend

Assemble the proof argument as JSON, splitting the 256-byte proof into `a`
(64 bytes), `b` (128 bytes) and `c` (64 bytes), then:

```sh
stellar contract invoke --id <POOL> --source veil-instaward-1 --network testnet --send=yes \
  -- transact --proof-file-path proof.json \
     --ext_data '{ "encrypted_output0": "", "encrypted_output1": "", "ext_amount": "0", "recipient": "<ADMIN>" }' \
     --sender <ADMIN>
```

## 7. The Rejected Spend

The allowlist gate refuses a spend whose ASP membership root is not the live
one. Simulation catches that before submission, so a plain invoke fails locally
and never reaches the ledger.

To record the rejection on chain, the recorded run signed the spend while the
allowlist still matched, then changed the allowlist before submitting, so the
gate fired during execution instead of during simulation. The spender was a
second account so that changing the allowlist did not consume the spender's
sequence number.

```sh
stellar contract invoke --id <POOL2> --source veil-spender --network testnet --build-only \
  -- transact --proof-file-path proof.json --ext_data '<EXT_DATA>' --sender <SPENDER> > tx.xdr
stellar tx simulate --source-account veil-spender --network testnet tx.xdr > tx-prepared.xdr
stellar tx sign --sign-with-key veil-spender --network testnet tx-prepared.xdr > tx-signed.xdr

# Admin changes the allowlist, which moves the root the pool reads.
stellar contract invoke --id <ASP3> --source veil-instaward-1 --network testnet --send=yes \
  -- insert_leaf --leaf 424242

stellar tx send --network testnet tx-signed.xdr
```

The submitted transaction lands with `successful: false` and a
`invoke_host_function: trapped` result: the pool rejected the spend because the
proof was bound to an allowlist root that is no longer live.

## Limitations

The deployed set uses a single admin key, a single-party trusted setup, Merkle
depth 10, and has not been audited. It is a testnet demonstration.
