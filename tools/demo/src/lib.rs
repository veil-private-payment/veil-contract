//! A command-line client that makes a private payment on testnet using only
//! the Veil read API for everything it learns about the pool.
//!
//! It holds its own keys, opens the notes sent to it, builds Groth16 proofs
//! locally, and submits through the `stellar` CLI, which signs with a fresh
//! testnet account. Nothing it does needs the server to be trusted: every
//! root and path it uses is checked by the circuit and the pool.

pub mod api;
pub mod chain;
pub mod ext_data;
pub mod flows;
pub mod note_crypto;
pub mod wallet;
