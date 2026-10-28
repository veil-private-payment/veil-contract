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
| Database schema and migrations | Done, see [docs/SCHEMA.md](docs/SCHEMA.md) |
| RPC ingestion and resume | Done |
| One-time seed from Stellar Expert, hand-off to RPC | Done |
| `verify`: recomputed roots against on-chain state | Done |
| Long-running service with `/health`, `/ready`, `/metrics` | Done, see [docs/DEPLOY.md](docs/DEPLOY.md) |
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
docker compose -f services/indexer/docker-compose.yml up -d
VEIL_TEST_DATABASE_URL=postgres://veil:veil@localhost:5433/veil cargo test -p veil-indexer
```

Without `VEIL_TEST_DATABASE_URL` the database tests are skipped locally. CI
sets it and fails if it is missing.

- `tests/contract_parity.rs` publishes each event through the contract's own
  struct and decodes what the host records. It covers the events the testnet
  deployment has never emitted (`Exit`, `public_key_event`).
- `tests/store_and_ingest.rs` runs on a real Postgres, one fresh database per
  test. It replays the full testnet history through a source that pages it the
  way RPC does, and checks: migrations on an empty database with instances
  racing, one row per event however often it is written, resume after a
  failure mid-sync with a result identical to an uninterrupted run, the same
  result for any page size, and that conflicting content, a reused tree index,
  a checkpoint moving back or an event from the wrong contract all stop
  ingestion with nothing from the failed batch kept.
- `tests/rpc.rs` exercises the `getEvents` client over HTTP against a mock
  server, using a real recorded testnet response: paging, cursor resume,
  retry on 5xx and 429, no retry on RPC errors.
- `tests/tree_parity.rs` compares the off-chain Poseidon2 compression and
  zero hashes with the contracts' own functions, the pool tree with the
  contract's `MerkleTreeWithHistory` after every insertion, and replays the
  live allowlist reproducing each root it emitted on testnet.
- `tests/chain.rs` reads contract state from recorded `getLedgerEntries`
  responses and checks the storage keys are encoded as the contract encodes
  them.
- `tests/seed_and_handoff.rs` seeds from a mock Stellar Expert that pages the
  recorded listings: full history, resume after a failure, refusal of
  foreign or out-of-order listings, the RPC overlap finding the same events
  (and stopping on a single differing byte), and the hand-off refusing any
  gap RPC retention would leave.
- `tests/verify_checks.rs` runs `verify` against the recorded testnet state
  (pool root `0x2658…80fd` over 34 positions, allowlist root `0x04cd…3738`)
  and against indexes that are wrong: a missing or altered commitment, a
  missing final pair, a tampered allowlist root, an operator-maintained
  allowlist.
- `tests/service.rs` covers the single-writer lock, the HTTP endpoints, and
  the follow loop handing off from the seed, reporting failures without
  exiting, and stopping on request.
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

Store events in a local Postgres:

```sh
docker compose -f services/indexer/docker-compose.yml up -d
export DATABASE_URL=postgres://veil:veil@localhost:5433/veil
export VEIL_POOL_CONTRACT=CDJELV6HUP6BKVWBUIFR7GEJPQDQRV5PCSUTEODOKSGTJPUD7DZ6B66G
export VEIL_ALLOWLIST_CONTRACT=CBRZ7RD5ESD7LRMPA5QY2V2BTMJBGISHS4SXE3NWFKAKB4BOJF2OBRMB

# Follow RPC, serving /health, /ready and /metrics on VEIL_LISTEN. On an
# empty database this first reads the full history from Stellar Expert.
cargo run -p veil-indexer -- run
# Any time: compare the index with the contracts' on-chain roots.
cargo run -p veil-indexer -- verify
```

`sync` reads to the tip once and exits, for scripts. Only one process may
write: `seed`, `sync` and `run` take a Postgres advisory lock. `seed` and
`sync` refuse to start while another process holds it, and `run` serves
`/health` and waits for it, so a rolling deploy hands over cleanly. `verify`
catches up first when no indexer is running, and otherwise waits for the
running one.

## Configuration

| Variable | Default | Meaning |
|---|---|---|
| `VEIL_NETWORK_PASSPHRASE` | `Test SDF Network ; September 2015` | Network the contracts are on |
| `VEIL_RPC_URL` | `https://soroban-testnet.stellar.org` | Soroban RPC endpoint |
| `VEIL_POOL_CONTRACT` | required | Pool contract ID (`C…`) |
| `VEIL_ALLOWLIST_CONTRACT` | required | Allowlist contract ID (`C…`) |
| `DATABASE_URL` | required except for `decode`, `config` | Postgres URL. Never logged |
| `VEIL_DB_MAX_CONNECTIONS` | `5` | Connection pool size |
| `VEIL_START_LEDGER` | none | Override where RPC starts when there is no RPC checkpoint. Normally left unset: RPC starts from the seed |
| `VEIL_OVERLAP_LEDGERS` | `17280` (about a day) | Ledgers before the seed's end that RPC re-reads and cross-checks |
| `VEIL_EXPERT_URL` | `https://api.stellar.expert/explorer/testnet` | Stellar Expert API for `seed` |
| `VEIL_LISTEN` | `0.0.0.0:8080` | Health and metrics server (`run`) |
| `VEIL_POLL_SECS` | `5` | Seconds between syncs at the tip |
| `VEIL_READY_MAX_LAG` | `10` | `/ready` fails beyond this many ledgers behind |
| `VEIL_READY_MAX_SILENCE_SECS` | `120` | `/ready` fails when no sync succeeded for this long |
| `VEIL_SEED_IF_EMPTY` | `true` | `run` seeds from Stellar Expert first when the database has neither a seed nor an RPC checkpoint |
| `VEIL_PAGE_SIZE` | `200` | Events per `getEvents` call, 1 to 10,000 |
| `VEIL_RPC_TIMEOUT_SECS` | `30` | Per-request timeout. Failed requests retry with backoff |
| `VEIL_LOG_FORMAT` | `json` | `json` or `pretty` |
| `RUST_LOG` | `info,sqlx=warn` | Log level filter |

Contract IDs are validated at start-up.
