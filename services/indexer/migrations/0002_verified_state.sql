-- The contracts' on-chain tree state as `verify` last read it, and whether
-- the index matched it. Written by the indexer after each verification, read
-- by the API so it can show its computed roots next to the chain's.
CREATE TABLE verified_state (
    contract            text    PRIMARY KEY CHECK (contract ~ '^C[A-Z2-7]{55}$'),
    kind                text    NOT NULL CHECK (kind IN ('pool', 'allowlist')),
    ledger              integer NOT NULL CHECK (ledger > 0),
    levels              integer NOT NULL CHECK (levels BETWEEN 1 AND 32),
    next_index          bigint  NOT NULL CHECK (next_index >= 0),
    root                bytea   NOT NULL CHECK (octet_length(root) = 32),
    operator_maintained boolean NOT NULL DEFAULT false,
    matches             boolean NOT NULL,
    checked_at          timestamptz NOT NULL DEFAULT now()
);
