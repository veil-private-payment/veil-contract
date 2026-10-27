# Note encryption

A private payment creates output commitments the recipient must later spend.
To spend one, the recipient needs the amount and blinding behind it. The sender
encrypts those to the recipient's published key and passes the result as
`encrypted_output0` / `encrypted_output1` in `ExtData`. The pool binds both to
the proof through the external data hash and republishes each in the
`new_commitment_event` for its commitment. The indexer stores the bytes as they
are and never decrypts them.

This document fixes the format, version `0x02`. Nothing in this repository
writes notes yet. The first implementation, with test vectors, is the CLI that
sends a payment through the indexer's API.

## Keys

`register` publishes two keys per account in `public_key_event`:

| Key | Size | Used for |
|---|---|---|
| `encryption_key` | 32 bytes | X25519 public key. Notes to this account are encrypted to it |
| `note_key` | 32 bytes | BN254 field element. The `outPubkey` the circuit hashes into the commitment |

They are independent keys. Knowing one says nothing about the other.

## Format

```
note = version || ephemeral_public_key || ciphertext || tag

version               1 byte    0x02
ephemeral_public_key  32 bytes  X25519, fresh for every note
ciphertext            64 bytes  ChaCha20-Poly1305 encryption of the plaintext
tag                   16 bytes  Poly1305 tag
                      113 bytes in total
```

The plaintext is two 32-byte big-endian integers:

```
plaintext = amount || blinding
```

`amount` must be below 2^248, as the circuit enforces. `blinding` must be below
the BN254 scalar field modulus.

## Encrypting

For a recipient with published `encryption_key` R:

1. Generate an ephemeral X25519 key pair (e, E).
2. `shared = X25519(e, R)`. Abort if `shared` is all zeros.
3. `okm = HKDF-SHA256(ikm = shared, salt = E || R, info = "veil/note/v2/chacha20poly1305", length = 44)`.
4. `key = okm[0..32]`, `nonce = okm[32..44]`.
5. `ciphertext || tag = ChaCha20-Poly1305(key, nonce, aad = 0x02 || E, plaintext)`.

Every note uses a new ephemeral key, so no key and nonce pair is ever used
twice, and two notes to the same recipient share no bytes.

## Scanning and accepting a note

A recipient holding the X25519 secret r for R:

1. Skip any note whose first byte is not `0x02` or whose length is not 113.
2. `shared = X25519(r, E)`, derive `key` and `nonce` as above, and decrypt.
   A tag mismatch means the note is not for this recipient. Skip it.
3. Check `amount < 2^248` and `blinding` is below the field modulus.
4. Recompute the commitment as the circuit does,
   `Poseidon2(amount, note_key, blinding)` with domain separation `0x01`, and
   compare it with the commitment the event carries. On a mismatch, discard
   the note: the sender encrypted values that do not open the commitment, and
   the output cannot be spent.

Only after step 4 does the recipient treat the output as received.

## What this does and does not hide

Each note is the same length, so the event reveals nothing about the amount
or the recipient. The ephemeral key is unlinkable to the sender. Scanning
costs one X25519 operation per note in the pool's history, which the
indexer's note feed lets a client do incrementally from its last position.

## Notes already on chain

The live testnet pool's history holds notes written before this format was
fixed: 13 of 137 bytes beginning with `0x01`, and 7 empty. Their layout is not
specified here. Clients skip them under step 1, and the indexer stores them
like any other note.
