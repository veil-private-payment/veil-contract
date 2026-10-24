//! Service configuration.
//!
//! Everything is read from the command line or the environment, never from a
//! file in the repository, and is validated before the service does any work:
//! a contract ID that is not a contract strkey stops the process at start-up
//! rather than producing an empty index.

use core::fmt;

use clap::Args;

/// The deployment the indexer reads.
#[derive(Clone, Debug, Args)]
pub struct ChainConfig {
    /// Network passphrase the contracts are deployed on.
    #[arg(
        long,
        env = "VEIL_NETWORK_PASSPHRASE",
        default_value = "Test SDF Network ; September 2015"
    )]
    pub network_passphrase: String,

    /// Soroban RPC endpoint.
    #[arg(
        long,
        env = "VEIL_RPC_URL",
        default_value = "https://soroban-testnet.stellar.org"
    )]
    pub rpc_url: String,

    /// Pool contract to index.
    #[arg(long, env = "VEIL_POOL_CONTRACT", value_parser = parse_contract_id)]
    pub pool_contract: String,

    /// Allowlist (ASP membership) contract the pool reads.
    #[arg(long, env = "VEIL_ALLOWLIST_CONTRACT", value_parser = parse_contract_id)]
    pub allowlist_contract: String,
}

/// How log lines are written.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, clap::ValueEnum)]
pub enum LogFormat {
    /// One JSON object per line, for log collectors.
    #[default]
    Json,
    /// Human-readable lines, for local development.
    Pretty,
}

/// Logging options shared by every subcommand.
#[derive(Clone, Debug, Args)]
pub struct LogConfig {
    #[arg(long, env = "VEIL_LOG_FORMAT", value_enum, default_value_t = LogFormat::Json, global = true)]
    pub log_format: LogFormat,
}

/// A value that must never be printed, such as a database URL with a password.
#[derive(Clone, PartialEq, Eq)]
pub struct Secret(String);

impl Secret {
    pub fn new(value: String) -> Self {
        Self(value)
    }

    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(***)")
    }
}

/// Accepts a contract strkey (`C…`) and nothing else.
pub fn parse_contract_id(value: &str) -> Result<String, String> {
    match stellar_strkey::Strkey::from_string(value) {
        Ok(stellar_strkey::Strkey::Contract(_)) => Ok(value.to_owned()),
        Ok(_) => Err(format!("{value} is a strkey but not a contract ID")),
        Err(_) => Err(format!("{value} is not a valid contract strkey")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn contract_ids_are_accepted() {
        let pool = "CDJELV6HUP6BKVWBUIFR7GEJPQDQRV5PCSUTEODOKSGTJPUD7DZ6B66G";
        assert_eq!(parse_contract_id(pool).as_deref(), Ok(pool));
    }

    #[test]
    fn account_ids_and_garbage_are_refused() {
        let account = "GCMNXF35HYEY5CEVEYCE57DTD4MGFIKQP5II54YK5SMRSXLFFZC7HRXN";
        assert!(parse_contract_id(account).is_err());
        assert!(parse_contract_id("CDJELV6H").is_err());
        assert!(parse_contract_id("").is_err());
    }

    #[test]
    fn secrets_do_not_print() {
        let secret = Secret::new("postgres://user:hunter2@db/veil".to_owned());
        assert!(!format!("{secret:?}").contains("hunter2"));
        assert_eq!(secret.expose(), "postgres://user:hunter2@db/veil");
    }
}
