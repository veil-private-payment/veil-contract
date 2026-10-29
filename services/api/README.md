# veil-api

Read-only HTTP API over the indexer's database: the pool's tree with roots and
Merkle paths, spent checks, the encrypted-note feed, published receiving keys
and the allowlist with membership paths. The reference is
[docs/API.md](docs/API.md), and `GET /openapi.json` describes it for tools.

The API holds no secret and signs nothing. Every database session it opens is
read-only and statements time out after five seconds. It reads the database
`services/indexer` fills, and needs nothing else.

## Running

```sh
docker compose -f services/indexer/docker-compose.yml up -d
export DATABASE_URL=postgres://veil:veil@localhost:5433/veil
export VEIL_POOL_CONTRACT=CDJELV6HUP6BKVWBUIFR7GEJPQDQRV5PCSUTEODOKSGTJPUD7DZ6B66G
export VEIL_ALLOWLIST_CONTRACT=CBRZ7RD5ESD7LRMPA5QY2V2BTMJBGISHS4SXE3NWFKAKB4BOJF2OBRMB

cargo run -p veil-indexer -- run &   # fills the database
cargo run -p veil-api                # serves it on :8080
curl localhost:8080/v1/pool
```

The tree endpoints answer 503 until the indexer has verified the contracts
once (on start-up of `verify`, or within ten minutes of `run`).

## Configuration

| Variable | Default | Meaning |
|---|---|---|
| `DATABASE_URL` | required | The indexer's database. Never logged |
| `VEIL_POOL_CONTRACT` | required | Pool contract ID |
| `VEIL_ALLOWLIST_CONTRACT` | required | Allowlist contract ID |
| `VEIL_NETWORK_PASSPHRASE` | testnet | Reported by `/v1/status` |
| `VEIL_LISTEN` | `0.0.0.0:8080` | Listen address. `PORT` overrides the port |
| `VEIL_DB_MAX_CONNECTIONS` | `10` | Connection pool size |
| `VEIL_RATE_PER_SECOND` | `20` | Sustained requests per second per client address |
| `VEIL_RATE_BURST` | `100` | Burst above the sustained rate |
| `VEIL_REQUEST_TIMEOUT_SECS` | `10` | Requests running longer answer 408 |
| `VEIL_READY_MAX_STALENESS_SECS` | `120` | `/ready` fails when the indexer has not advanced for this long |
| `VEIL_LOG_FORMAT` | `json` | `json` or `pretty` |

## Deploying

Same image recipe as the indexer: `docker build -f services/api/Dockerfile .`
gives a distroless, non-root image. On Railway it is a third service in the
indexer's project with `RAILWAY_DOCKERFILE_PATH=services/api/Dockerfile`,
`DATABASE_URL=${{Postgres.DATABASE_URL}}` and the two contract IDs; the
platform terminates TLS on the public domain. Point the deploy health check at
`/health` and monitoring at `/ready`.

## Tests

```sh
VEIL_TEST_DATABASE_URL=postgres://veil:veil@localhost:5433/veil cargo test -p veil-api
```

`tests/api.rs` loads the deployment's real testnet history into a fresh
database, serves the API over HTTP with the same read-only pool and
middleware it runs with, and checks every endpoint: the computed pool root
equals the on-chain root, every pool and allowlist path hashes up to its
on-chain root, paging returns every position once, spent and unknown
nullifiers, the 20 notes, latest-wins keys, 409 for an operator-maintained
allowlist, writes refused by the database, request ids, CORS, readiness, rate
limiting and the OpenAPI document.
