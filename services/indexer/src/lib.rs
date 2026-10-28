//! Off-chain indexer for the Veil pool and allowlist contracts.
//!
//! The contracts publish their state only as events. This crate reads those
//! events, from Stellar Expert once to seed history and from Soroban RPC after
//! that, so an app can learn the pool's commitments, spent nullifiers,
//! encrypted notes and allowlist without collecting everything itself.

pub mod chain;
pub mod config;
pub mod event;
pub mod expert;
pub mod ingest;
pub mod logging;
pub mod position;
pub mod rpc;
pub mod seed;
pub mod service;
pub mod source;
pub mod store;
pub mod tree;
pub mod verify;
