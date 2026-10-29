//! Read-only HTTP API over the indexer's database.
//!
//! Gives an app what it needs to make a private payment: the pool's
//! commitments in tree order with their roots and Merkle paths, whether a
//! nullifier is spent, the encrypted notes a recipient scans, published
//! receiving keys, and the allowlist with its membership paths.
//!
//! The server holds no secret and signs nothing. Every database session it
//! opens is read-only, and every answer can be checked against the chain:
//! the roots it computes are served next to the roots the indexer last read
//! from the contracts.

pub mod error;
pub mod model;
pub mod queries;
pub mod routes;

use std::time::Duration;

use sqlx::Executor;
use sqlx::postgres::{PgConnectOptions, PgPool, PgPoolOptions};

/// A connection pool whose every session refuses writes and bounds each
/// statement to five seconds. The API issues no writes; this makes sure a
/// bug could not either.
pub async fn read_only_pool(
    options: PgConnectOptions,
    max_connections: u32,
) -> Result<PgPool, sqlx::Error> {
    PgPoolOptions::new()
        .max_connections(max_connections)
        .acquire_timeout(Duration::from_secs(5))
        .after_connect(|connection, _| {
            Box::pin(async move {
                connection
                    .execute(
                        "SET SESSION CHARACTERISTICS AS TRANSACTION READ ONLY; \
                         SET statement_timeout = '5s'",
                    )
                    .await?;
                Ok(())
            })
        })
        .connect_with(options)
        .await
}
