# Deploying the indexer

The indexer is one container and one Postgres database. Nothing in it holds a
key or signs anything: it only reads the chain and writes its own database.

## Image

```sh
docker build -f services/indexer/Dockerfile -t veil-indexer .
```

Multi-stage: the build stage compiles `veil-indexer` with the pinned
toolchain, and the runtime image is distroless (no shell, no package manager)
running as a non-root user. The default command is `run`, listening on
port 8080.

## Database

Any Postgres 15 or later. Use a managed instance with automated daily backups
and point-in-time recovery if offered. The indexer applies its own migrations
at start-up, under an advisory lock.

Give the indexer a role that owns its schema. The read API (separate service)
gets its own role with `SELECT` only.

Losing the database loses nothing that cannot be rebuilt: every row comes from
the chain. A restore from backup resumes from the restored checkpoint, and a
total loss is recovered by `seed` then `run` on an empty database.

## Configuration

Set at least:

| Variable | Value for the testnet deployment |
|---|---|
| `DATABASE_URL` | the managed Postgres URL, as a secret |
| `VEIL_POOL_CONTRACT` | `CDJELV6HUP6BKVWBUIFR7GEJPQDQRV5PCSUTEODOKSGTJPUD7DZ6B66G` |
| `VEIL_ALLOWLIST_CONTRACT` | `CBRZ7RD5ESD7LRMPA5QY2V2BTMJBGISHS4SXE3NWFKAKB4BOJF2OBRMB` |

The contract IDs come from `deployments/testnet.json`. Everything else has a
working default for testnet; see the README for the full list.

## First start

1. Run the image once with the command `seed`. It reads both contracts' full
   history from Stellar Expert and records the ledger it is complete to. It is
   resumable if interrupted.
2. Start the service with the default command `run`. With no RPC checkpoint
   yet, it starts RPC about a day before the seed's end, re-reads that overlap
   and checks each event against the seeded copy.
3. Point the platform's health check at `GET /ready` (port 8080). It returns
   200 once the index is within 10 ledgers of the RPC tip.
4. Run `verify` (same image, same environment). It exits 0 when the pool's
   recomputed root and size, and the allowlist's roots, match the chain.

Run one `run` instance. A second refuses to start while the first holds the
writer lock, so a rolling deploy that overlaps two instances briefly is safe:
the new one exits and the platform restarts it until the old one is gone.

## Operating

| Signal | Meaning | Action |
|---|---|---|
| `/ready` 503, "behind the RPC tip" | Catching up, or RPC is slow | Normal after a restart. Investigate if it persists |
| `/ready` 503, `last_error` mentions "gap" | The index stopped longer than RPC retention (about seven days) | Run `seed` again, then restart `run` |
| `/ready` 503, `last_error` mentions a conflict or decode error | Two sources disagree, or a contract emitted an event the decoder does not know | Stop and investigate. Never skip it: the index would be wrong |
| Process exits with "writer lock was lost" | The database connection holding the lock dropped | The platform restarts it. Nothing to do |
| `verify` exits non-zero | The index does not match the chain | Investigate before serving reads from it |

`/metrics` exposes `veil_indexer_lag_ledgers`, `veil_indexer_indexed_ledger`,
`veil_indexer_latest_ledger`, `veil_indexer_events_stored_total`,
`veil_indexer_sync_errors_total` and `veil_indexer_last_success_age_seconds`.

On SIGTERM the service stops at once. A page being written then is either
committed with its checkpoint or rolled back, so a stop at any moment loses
nothing and duplicates nothing.
