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

Both trees must be created at the depth the circuit was compiled for, which is
16. The pool and the ASP membership contract each take their own `--levels`,
and a mismatch is not rejected at deploy time: it surfaces later as every real
proof failing, because the roots cannot agree. Record the value you used.

Depth 16 is set by the circuit, not by the chain. An earlier version of this
guide said depth 17 failed with `write ledger entries: 52 > 50`. The transaction
write limit is 200 now, read from the `ConfigSetting` ledger entry on
2026-09-21, and the same wasm deployed at depth 17, 20, 24 and 32. Use 16 anyway,
because the committed proving and verifying keys are for a depth-16 circuit and a
mismatch surfaces later as every proof failing.

```sh
stellar contract deploy --wasm target/wasm32v1-none/release/circom_groth16_verifier.wasm \
  --source veil-instaward-1 --network testnet

stellar contract deploy --wasm target/wasm32v1-none/release/asp_membership.wasm \
  --source veil-instaward-1 --network testnet \
  -- --admin <ADMIN> --levels 16

stellar contract deploy --wasm target/wasm32v1-none/release/pool.wasm \
  --source veil-instaward-1 --network testnet \
  -- --admin <ADMIN> --token <TOKEN_SAC> --verifier <VERIFIER> \
     --asp_membership <ASP> --maximum_deposit_amount 1000000000 \
     --fee_recipient <ADMIN> --fee_bps 0 --levels 16
```

The token used for the recorded run is the native XLM SAC, from
`stellar contract id asset --asset native --network testnet`.

The verifier takes no constructor arguments: its verification key is embedded
at build time from `circuits/keys/policy_tx_2_2_vk.json`.

## 3b. The Onboarding Faucet

A spender who is not in the allowlist cannot move a note, and the allowlist
only accepts writes from its admin. Deploy the faucet and hand it that role, so
testers enrol themselves:

```sh
stellar contract deploy --wasm target/wasm32v1-none/release/faucet.wasm \
  --source veil-instaward-1 --network testnet \
  -- --admin <ADMIN> --asp <ASP> --drip_amount 0 --cooldown_ledgers 12

stellar contract invoke --id <ASP> --source veil-instaward-1 --network testnet --send=yes \
  -- update_admin --new_admin <FAUCET>
```

Omit `--token` when the pool settles the native asset: the faucet cannot be the
admin of that Stellar Asset Contract, so it only enrols and testers fund
themselves from friendbot. Pass `--token <SAC>` with a positive `--drip_amount`
when the faucet administers the pool's token, and it will mint as well.

A tester then calls:

```sh
stellar contract invoke --id <FAUCET> --source <THEIR_KEY> --network testnet --send=yes \
  -- onboard --to <THEIR_ADDRESS> --membership_leaf <LEAF>
```

The cooldown is per recipient address.

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

## 6b. Withdrawals

Pass the external amount to the fixture generator as a second argument. A
negative value withdraws:

```sh
stellar contract invoke --id <POOL> --source veil-instaward-1 --network testnet \
  -- get_ext_data_hash --ext_data '{ "encrypted_output0": "", "encrypted_output1": "", "ext_amount": "-10", "recipient": "<RECIPIENT>" }'

cargo run -p policy-fixture --bin testnet-fixture -- <EXT_DATA_HASH_HEX> -10
```

Check the encoding matches what the pool expects before submitting:

```sh
stellar contract invoke --id <POOL> --source veil-instaward-1 --network testnet \
  -- get_public_amount --ext_amount=-10
```

That value must equal `publicAmount` in the generated fixture. The pool must
also hold enough of the token to cover the payout and the protocol fee.

## 6c. Leaving A Full Pool

Every transaction writes two output commitments, so `transact` stops working
once the tree fills, and withdrawals go through `transact`. `exit` is the way
out: the same checks, the recipient is paid, and the transaction's output notes
are discarded rather than inserted.

Check which entrypoint applies before building a proof:

```sh
stellar contract invoke --id <POOL> --source veil-instaward-1 --network testnet \
  -- is_tree_full

stellar contract invoke --id <POOL> --source veil-instaward-1 --network testnet \
  -- remaining_leaves
```

While `remaining_leaves` is above one, `exit` is refused and `transact` is the
right call. Once the tree is full, build the withdrawal exactly as in 6b and
invoke `exit` in place of `transact`. Anything the proof assigns to the output
notes is destroyed, so the inputs must be spent in full and the whole value
withdrawn. Two notes go per call, which is enough to drain any holding.

The deployed pool was upgraded to this wasm on 2026-09-14:

| Step | Transaction |
|---|---|
| Upload `pool` wasm `179289800a9f…` | [`a73109ac…`](https://stellar.expert/explorer/testnet/tx/a73109accb367d7764c1bbff8ef31d9748f7b5885d55968f86ecafbc00cb0fd5) |
| `upgrade` on the pool | [`72c8983a…`](https://stellar.expert/explorer/testnet/tx/72c8983a1d4f307dd882c7b0755372a5b762446b309cc04241a39ef15c1522a2) |

The upgrade is verified by the views answering on the live contract:
`is_tree_full` returns `false` and `remaining_leaves` returns `65530`, six
leaves having been used by the three inserts recorded above.

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
depth 16, and has not been audited. It is a testnet demonstration.
