# Indexer database schema

The schema lives in `services/indexer/migrations/`. This document explains
what each table holds and which invariants the database enforces, so a wrong
index fails loudly instead of serving wrong answers.

## Keys

Every event has a position: the TOID of the operation that emitted it and its
index within that operation (see `src/position.rs`). Every table below except
`ingest_checkpoints` is keyed by `(toid, event_index)`, and every typed row
references the raw row in `contract_events` it was decoded from.

Consequences:

- Writing the same event twice is a no-op. Ingestion can re-read any range.
- A typed row cannot exist without the raw event behind it.
- `ledger` is derivable from `toid` (top 32 bits). `contract_events` checks
  that the two agree, and the typed tables copy it for querying.

Values are stored in their on-chain form: 256-bit values (commitments,
nullifiers, roots, leaves, public amounts) as 32-byte big-endian `bytea`,
addresses as strkeys, `i128` amounts as `numeric(39, 0)`.

## Tables

| Table | One row per | Notable constraints |
|---|---|---|
| `contract_events` | Stored event, in its original XDR | Contract ID shape, source is `stellar_expert` or `rpc`, ledger matches TOID |
| `pool_commitments` | Commitment inserted into the pool tree | `UNIQUE (pool, leaf_index)`. `encrypted_output` NULL exactly when `origin = 'legacy_deposit'` |
| `pool_nullifiers` | Spent nullifier | `UNIQUE (pool, nullifier)` |
| `pool_settlements` | Input of a settled `transact` | Indexed by nullifier |
| `pool_exits` | Spend through `exit` | Indexed by nullifier |
| `pool_legacy_deposits` | Deposit through the closed path | References its row in `pool_commitments` |
| `pool_public_keys` | Key registration | Latest per `(pool, owner)` wins. Indexed for that lookup |
| `allowlist_events` | Allowlist addition or revocation | `leaf_index` set exactly for additions |
| `ingest_checkpoints` | Ingestion stream | Cursor as TOID and event index |

`contract_events` also keeps host events that carry no Veil state, such as
`executable_update` on a wasm upgrade, so the raw table is a complete record
of what each contract emitted. They have no typed row.

### Why some things are not unique

- **Commitments by value.** The pool does not reject a repeated commitment,
  so the index on `(pool, commitment)` is for lookup only.
- **Allowlist leaf indices.** An operator-maintained allowlist publishes the
  index and root it computed off chain (`publish_leaf`), and a revocation
  rebuilds the tree. The table records the stream of changes as emitted and
  leaves interpretation to the reader.

## Write path

`Store::write_batch` (`src/store.rs`) takes a page of decoded events and the
checkpoint that follows them and, in one transaction:

1. Locks the stream's checkpoint row and refuses a checkpoint behind it.
2. Inserts each event into `contract_events` with `ON CONFLICT DO NOTHING`.
3. If the event was already stored, compares contract, topics and body. A
   difference is a conflict: the transaction rolls back and ingestion stops.
   Stellar Expert and RPC return the same XDR for the same event, so a seeded
   event re-read from RPC is accepted as the same row and keeps its original
   `source`.
4. Otherwise inserts the typed row. A tree index or nullifier already claimed
   by a different event violates a unique constraint, which rolls back the
   transaction.
5. Moves the checkpoint.

A crash at any point leaves either the whole page and its checkpoint or
neither. On restart, ingestion resumes after the stored cursor.

## Checkpoints

`ingest_checkpoints.stream` names an ingestion stream:

| Stream | Written by | Cursor means |
|---|---|---|
| `stellar_expert:<contract>` | `seed`, per page | Last event of that contract read from Stellar Expert |
| `stellar_expert` | `seed`, at the end | End of the ledger the seed is complete to (`latest_ledger`) |
| `rpc` | `sync`, `run` | Where RPC ingestion resumes |

For `rpc`, the cursor is the position RPC returned with the last committed page:
either the last event of a full page, or the end of the ledger range the server
scanned (event index `4294967295`). Resuming passes it back as the `getEvents`
cursor, which is exclusive.

## Migrations

- Applied at start-up by `veil-indexer migrate`, `sync` and later the
  long-running service. sqlx takes a Postgres advisory lock for the duration,
  so instances starting together apply each migration once.
- Applied migrations are never edited. sqlx stores a checksum and refuses to
  start against a database whose applied migration differs from the file.
  Changes go in a new numbered file.
- The read API will use a separate role with `SELECT` only.
