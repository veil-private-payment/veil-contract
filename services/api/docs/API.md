# Veil read API

Read-only HTTP API over the indexer's database. It serves what an app needs to
make a private payment, and nothing that needs trust: the server holds no
secret, signs nothing and makes no proofs. Every answer can be checked against
the chain.

The machine-readable description is served at `GET /openapi.json`.

## Conventions

- Field elements (commitments, nullifiers, roots, leaves) are 32-byte
  big-endian values written `0x` plus 64 hex digits. Other byte strings
  (notes, keys) are `0x` plus hex of any length. Addresses are strkeys.
- Responses that depend on the chain carry `indexed_ledger`: the last ledger
  the indexer has read completely. The answer reflects the chain up to there.
- Listings page with `cursor` and `limit` (1 to 1000, default 100). Pass the
  response's `next_cursor` back as `cursor`. It is absent on the last page.
- Errors are JSON, `{"error": {"code": "...", "message": "..."}}`. Codes:
  `bad_request` (400), `not_found` (404), `conflict` (409), `unavailable`
  (503), `rate_limited` (429), `internal` (500).
- Every response carries an `x-request-id` header. A caller's own
  `x-request-id` is echoed back.
- Requests are rate limited per client address (20 per second sustained, bursts
  of 100, by default). Any origin may call the API from a browser.

## Endpoints

| Method and path | Returns |
|---|---|
| `GET /v1/status` | The contracts followed, the indexed ledger, when the indexer last advanced |
| `GET /v1/pool` | Tree depth, size and root computed from the index, beside the root and size the pool held at the last verification |
| `GET /v1/pool/leaves` | Every tree position in order: the commitments, and the zero leaf beside each legacy deposit |
| `GET /v1/pool/leaves/{leaf_index}/path` | Merkle path of one position, as the circuit takes it |
| `GET /v1/pool/nullifiers/{nullifier}` | Whether a nullifier is spent, and where |
| `GET /v1/notes` | Encrypted notes in tree order, for recipients to scan |
| `GET /v1/public-keys/{owner}` | The receiving keys an account last published |
| `GET /v1/allowlist` | Allowlist depth, additions, revocations, latest root, beside the on-chain state |
| `GET /v1/allowlist/entries` | Additions and revocations in order |
| `GET /v1/allowlist/leaves/{leaf}/path` | Membership path of a leaf |
| `GET /health` | `ok` while the process runs |
| `GET /ready` | 200 when the database answers and the indexer advanced within two minutes |

### Pool state

```http
GET /v1/pool
```

```json
{
  "contract": "CDJELV6HUP6BKVWBUIFR7GEJPQDQRV5PCSUTEODOKSGTJPUD7DZ6B66G",
  "levels": 16,
  "next_index": 34,
  "root": "0x2658c9003126dae7e4052ce3df4d73ff43f0342a7a8512990245aa0f8f7080fd",
  "indexed_ledger": 5452790,
  "on_chain": {
    "ledger": 5452790,
    "levels": 16,
    "next_index": 34,
    "root": "0x2658c9003126dae7e4052ce3df4d73ff43f0342a7a8512990245aa0f8f7080fd",
    "matches": true,
    "checked_at": "2026-10-29T15:26:34.617Z"
  }
}
```

`root` is computed by the API from the stored commitments. `on_chain` is what
the indexer read from the pool's storage at `on_chain.ledger`. The same root at
the same size means the index holds exactly the pool's data. Returns 503 until
the indexer has verified the pool once, since the tree depth comes from there.

### Tree positions

```http
GET /v1/pool/leaves?cursor=0&limit=3
```

```json
{
  "items": [
    { "leaf_index": 0, "commitment": "0x108d69c6…cbf655", "origin": "legacy_deposit", "ledger": 4659661 },
    { "leaf_index": 1, "commitment": "0x0000000000000000000000000000000000000000000000000000000000000000", "origin": "legacy_deposit_zero", "ledger": 4659661 },
    { "leaf_index": 2, "commitment": "0x13f923f9…1da3ba", "origin": "legacy_deposit", "ledger": 4659665 }
  ],
  "next_cursor": 3,
  "indexed_ledger": 5452790
}
```

`origin` is `transact`, `legacy_deposit`, or `legacy_deposit_zero`. The pool's
closed deposit path inserted each deposit as a pair, the commitment and a zero
leaf, and emitted an event for the commitment only. The zero leaf is listed so
that placing every item at its `leaf_index` rebuilds the pool's tree exactly.
Positions not listed hold the contracts' empty-leaf value.

### Merkle path

```http
GET /v1/pool/leaves/10/path
```

```json
{
  "leaf_index": 10,
  "leaf": "0x1eaa64128c3b170f933878093320b4010f43d3aca4fced09edb94132f8395b54",
  "path_elements": ["0x012c9a33…5ec814", "0x2a79a57c…0014aa", "… 16 in all"],
  "path_indices": 10,
  "root": "0x2658c9003126dae7e4052ce3df4d73ff43f0342a7a8512990245aa0f8f7080fd",
  "indexed_ledger": 5452790
}
```

`path_elements` are the siblings from the leaf up. Bit `i` of `path_indices` is
set when the node at level `i` is a right child, the order the circuit's
`inPathIndices` uses. Hashing up with Poseidon2 compression gives `root`.

### Spent check

```http
GET /v1/pool/nullifiers/0x191cdaf721f9f3bd32669c26d3eef27b5acef556d951ce984793f43db89227c4
```

```json
{
  "nullifier": "0x191cdaf721f9f3bd32669c26d3eef27b5acef556d951ce984793f43db89227c4",
  "spent": true,
  "ledger": 4659673,
  "event": "0020013143145066497-0000000002",
  "indexed_ledger": 5452790
}
```

An unspent nullifier returns `"spent": false` with `ledger` and `event` null.
The answer holds up to `indexed_ledger`. A spend after it is not yet known.

### Notes

```http
GET /v1/notes?cursor=10&limit=1
```

```json
{
  "items": [
    { "leaf_index": 10, "commitment": "0x1eaa6412…395b54", "encrypted_output": "0x0171fa1f…", "ledger": 4662333 }
  ],
  "next_cursor": 11,
  "indexed_ledger": 5452790
}
```

One item per output of a settled `transact`, in tree order. A recipient keeps
the last `next_cursor` and resumes from it. The format of `encrypted_output` is
`docs/NOTE_ENCRYPTION.md` at the repository root. Notes may be empty, and
notes written before that format was fixed begin with `0x01`. Clients skip
both.

### Public keys

```http
GET /v1/public-keys/{owner}
```

Returns `owner`, `encryption_key` (X25519), `note_key` (BN254), the `ledger`
of the latest registration and how many `registrations` the owner has made.
404 if the owner has never registered.

### Allowlist

`GET /v1/allowlist` returns the depth, the number of additions and
revocations, the latest emitted root and the state last read from the
contract.

`GET /v1/allowlist/entries` lists changes in order. Its cursor is an event
position, for example `0020568237603311617-0000000000`.

`GET /v1/allowlist/leaves/{leaf}/path` returns the membership path in the
same form as the pool's. The API replays the additions and serves the path
only when the replayed root equals the root the contract last emitted. After a
revocation, the allowlist's operator rebuilds the tree off chain and publishes
roots, so paths cannot be derived from events: the endpoint then answers 409.

## Checking an answer against the chain

- The pool root and size: compare `GET /v1/pool` with the pool's
  `get_root()` on Stellar Expert or through RPC.
- A commitment or nullifier: its `ledger` and `event` position identify the
  transaction on Stellar Expert.
- A path: hash it up yourself. It must end at the pool's on-chain root.
