-- Schema for the Veil indexer. docs/SCHEMA.md explains each table and the
-- invariants the constraints below enforce.
--
-- Every row is keyed by the position of the event that produced it
-- (`toid`, `event_index`), so writing the same event twice is a no-op, and
-- every typed row points back at the raw event it was decoded from.

-- Every contract event the indexer stored, exactly as it was read. Kept so the
-- typed tables can be rebuilt by re-decoding without fetching history again.
CREATE TABLE contract_events (
    toid        bigint  NOT NULL CHECK (toid > 0),
    event_index integer NOT NULL CHECK (event_index >= 0),
    ledger      integer NOT NULL CHECK (ledger > 0),
    contract    text    NOT NULL CHECK (contract ~ '^C[A-Z2-7]{55}$'),
    source      text    NOT NULL CHECK (source IN ('stellar_expert', 'rpc')),
    name        text    NOT NULL,
    topics_xdr  text[]  NOT NULL,
    data_xdr    text    NOT NULL,
    stored_at   timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (toid, event_index),
    -- A TOID carries its ledger in the top 32 bits.
    CHECK (ledger = (toid >> 32))
);

CREATE INDEX contract_events_by_contract ON contract_events (contract, toid, event_index);

-- Pool: commitments in insertion order. `encrypted_output` is the note as the
-- sender published it, opaque to the indexer, and NULL for commitments the
-- closed deposit path inserted, which carried no note.
CREATE TABLE pool_commitments (
    toid             bigint  NOT NULL,
    event_index      integer NOT NULL,
    pool             text    NOT NULL,
    leaf_index       bigint  NOT NULL CHECK (leaf_index >= 0),
    commitment       bytea   NOT NULL CHECK (octet_length(commitment) = 32),
    encrypted_output bytea,
    origin           text    NOT NULL CHECK (origin IN ('transact', 'legacy_deposit')),
    ledger           integer NOT NULL,
    PRIMARY KEY (toid, event_index),
    FOREIGN KEY (toid, event_index) REFERENCES contract_events (toid, event_index),
    -- The pool never reuses a tree position.
    UNIQUE (pool, leaf_index),
    CHECK ((origin = 'legacy_deposit') = (encrypted_output IS NULL))
);

CREATE INDEX pool_commitments_by_value ON pool_commitments (pool, commitment);

-- Pool: spent nullifiers. The pool rejects a second spend, so a repeat here
-- means the index is wrong and the write fails.
CREATE TABLE pool_nullifiers (
    toid        bigint  NOT NULL,
    event_index integer NOT NULL,
    pool        text    NOT NULL,
    nullifier   bytea   NOT NULL CHECK (octet_length(nullifier) = 32),
    ledger      integer NOT NULL,
    PRIMARY KEY (toid, event_index),
    FOREIGN KEY (toid, event_index) REFERENCES contract_events (toid, event_index),
    UNIQUE (pool, nullifier)
);

-- Pool: one row per input of a settled `transact`.
CREATE TABLE pool_settlements (
    toid               bigint  NOT NULL,
    event_index        integer NOT NULL,
    pool               text    NOT NULL,
    nullifier          bytea   NOT NULL CHECK (octet_length(nullifier) = 32),
    output_commitment0 bytea   NOT NULL CHECK (octet_length(output_commitment0) = 32),
    output_commitment1 bytea   NOT NULL CHECK (octet_length(output_commitment1) = 32),
    output_index0      bigint  NOT NULL CHECK (output_index0 >= 0),
    output_index1      bigint  NOT NULL CHECK (output_index1 >= 0),
    amount_bucket      numeric(39, 0) NOT NULL,
    public_amount      bytea   NOT NULL CHECK (octet_length(public_amount) = 32),
    recipient          text,
    asset              text    NOT NULL,
    ledger             integer NOT NULL,
    PRIMARY KEY (toid, event_index),
    FOREIGN KEY (toid, event_index) REFERENCES contract_events (toid, event_index)
);

CREATE INDEX pool_settlements_by_nullifier ON pool_settlements (pool, nullifier);

-- Pool: spends out of a full pool through `exit`.
CREATE TABLE pool_exits (
    toid          bigint  NOT NULL,
    event_index   integer NOT NULL,
    pool          text    NOT NULL,
    nullifier     bytea   NOT NULL CHECK (octet_length(nullifier) = 32),
    amount_bucket numeric(39, 0) NOT NULL,
    public_amount bytea   NOT NULL CHECK (octet_length(public_amount) = 32),
    recipient     text    NOT NULL,
    asset         text    NOT NULL,
    ledger        integer NOT NULL,
    PRIMARY KEY (toid, event_index),
    FOREIGN KEY (toid, event_index) REFERENCES contract_events (toid, event_index)
);

CREATE INDEX pool_exits_by_nullifier ON pool_exits (pool, nullifier);

-- Pool: deposits through the closed path. Their commitments are also in
-- pool_commitments, at the same position.
CREATE TABLE pool_legacy_deposits (
    toid          bigint  NOT NULL,
    event_index   integer NOT NULL,
    pool          text    NOT NULL,
    leaf_index    bigint  NOT NULL CHECK (leaf_index >= 0),
    amount_bucket numeric(39, 0) NOT NULL,
    asset         text    NOT NULL,
    ledger        integer NOT NULL,
    PRIMARY KEY (toid, event_index),
    FOREIGN KEY (toid, event_index) REFERENCES pool_commitments (toid, event_index)
);

-- Pool: receiving keys accounts published. An account may publish again, and
-- the latest row for an owner is the one to use.
CREATE TABLE pool_public_keys (
    toid           bigint  NOT NULL,
    event_index    integer NOT NULL,
    pool           text    NOT NULL,
    owner          text    NOT NULL,
    encryption_key bytea   NOT NULL,
    note_key       bytea   NOT NULL,
    ledger         integer NOT NULL,
    PRIMARY KEY (toid, event_index),
    FOREIGN KEY (toid, event_index) REFERENCES contract_events (toid, event_index)
);

CREATE INDEX pool_public_keys_by_owner ON pool_public_keys (pool, owner, toid DESC, event_index DESC);

-- Allowlist: membership changes in order. `leaf_index` is set for additions
-- only. An operator-maintained allowlist publishes indices and roots it
-- computed off chain, so neither is assumed unique or recomputable here.
CREATE TABLE allowlist_events (
    toid        bigint  NOT NULL,
    event_index integer NOT NULL,
    allowlist   text    NOT NULL,
    kind        text    NOT NULL CHECK (kind IN ('added', 'revoked')),
    leaf        bytea   NOT NULL CHECK (octet_length(leaf) = 32),
    leaf_index  bigint  CHECK (leaf_index >= 0),
    root        bytea   NOT NULL CHECK (octet_length(root) = 32),
    ledger      integer NOT NULL,
    PRIMARY KEY (toid, event_index),
    FOREIGN KEY (toid, event_index) REFERENCES contract_events (toid, event_index),
    CHECK ((kind = 'added') = (leaf_index IS NOT NULL))
);

CREATE INDEX allowlist_events_by_leaf ON allowlist_events (allowlist, leaf);

-- How far each ingestion stream has read. Written in the same transaction as
-- the events it covers, so a restart resumes exactly after the last batch that
-- committed.
CREATE TABLE ingest_checkpoints (
    stream            text    PRIMARY KEY,
    cursor_toid       bigint  NOT NULL CHECK (cursor_toid >= 0),
    cursor_event      bigint  NOT NULL CHECK (cursor_event >= 0 AND cursor_event <= 4294967295),
    latest_ledger     integer NOT NULL,
    updated_at        timestamptz NOT NULL DEFAULT now()
);
