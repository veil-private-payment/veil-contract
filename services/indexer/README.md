# veil-indexer

Reads the events the pool and allowlist contracts emit, so an app can learn the
pool's commitments, spent nullifiers, encrypted notes and allowlist without
collecting the whole history itself.

The contracts publish their state only as events. Soroban RPC keeps about seven
days of them, so a pool's older history is read once from Stellar Expert, which
keeps every event with the same XDR, and RPC is used from there on.

## Status

| Part | State |
|---|---|
| Event decoding (all pool and allowlist events) | Done |
| Database schema and migrations | Not started |
| RPC ingestion and resume | Not started |
| One-time seed from Stellar Expert and root check | Not started |
| Read API | Not started (separate crate) |

## Events

The decoder covers every event in `contracts/pool/src/event.rs` and
`contracts/asp-membership/src/event.rs`:

| Contract | Event (first topic) | What the indexer does with it |
|---|---|---|
| Pool | `new_commitment_event` | Inserts a commitment and its encrypted note at a tree index |
| Pool | `new_nullifier_event` | Marks a nullifier spent |
| Pool | `Settlement` | Records a settled `transact` and its outputs |
| Pool | `Exit` | Records a spend out of a full pool. Adds nothing to the tree |
| Pool | `Deposit` | Legacy. No longer emitted, but older pools inserted commitments with it |
| Pool | `public_key_event` | Records the keys an account published for receiving |
| Allowlist | `LeafAdded` | Inserts a member at an index, with the resulting root |
| Allowlist | `LeafRevoked` | Removes a member, with the root of the rebuilt tree |

The host also reports `executable_update` under a contract's ID when its wasm is
upgraded. It carries no Veil state and is logged and skipped.

Decoding is strict. A missing field, an extra field, a wrong type or a wrong
number of topics is an error, and so is any event name not listed above. A
contract change that alters an event therefore stops the indexer loudly instead
of producing a quietly wrong index.

## Tests

```sh
cargo test -p veil-indexer
```

- `tests/contract_parity.rs` publishes each event through the contract's own
  struct and decodes what the host records. It covers the events the testnet
  deployment has never emitted (`Exit`, `public_key_event`).
- `tests/stellar_expert_fixtures.rs` decodes the full testnet history of the
  deployment in `deployments/testnet.json` as Stellar Expert returned it, and
  checks ledger order, unique tree indices, one spend per nullifier, and that
  every settlement's outputs match the commitments its operation inserted.

## Running

```sh
# Decode a saved Stellar Expert listing, one JSON line per event
cargo run -p veil-indexer -- --log-format pretty decode \
  services/indexer/tests/fixtures/stellar_expert/pool_CDJELV6H.json

# Validate configuration
VEIL_POOL_CONTRACT=CDJELV6HUP6BKVWBUIFR7GEJPQDQRV5PCSUTEODOKSGTJPUD7DZ6B66G \
VEIL_ALLOWLIST_CONTRACT=CBRZ7RD5ESD7LRMPA5QY2V2BTMJBGISHS4SXE3NWFKAKB4BOJF2OBRMB \
cargo run -p veil-indexer -- config
```

Logs are JSON on stderr by default (`--log-format pretty` for local work), and
the level comes from `RUST_LOG`.

A local Postgres for development:

```sh
docker compose -f services/indexer/docker-compose.yml up -d
```

## Configuration

| Variable | Default | Meaning |
|---|---|---|
| `VEIL_NETWORK_PASSPHRASE` | `Test SDF Network ; September 2015` | Network the contracts are on |
| `VEIL_RPC_URL` | `https://soroban-testnet.stellar.org` | Soroban RPC endpoint |
| `VEIL_POOL_CONTRACT` | required | Pool contract ID (`C…`) |
| `VEIL_ALLOWLIST_CONTRACT` | required | Allowlist contract ID (`C…`) |
| `VEIL_LOG_FORMAT` | `json` | `json` or `pretty` |
| `RUST_LOG` | `info` | Log level filter |

Contract IDs are validated at start-up.
