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

/// The database the indexer writes to.
#[derive(Clone, Debug, Args)]
pub struct DatabaseConfig {
    /// Postgres URL. Read from the environment in deployments so the password
    /// stays out of the process list.
    #[arg(long, env = "DATABASE_URL", hide_env_values = true, value_parser = parse_secret)]
    pub database_url: Secret,

    /// Largest number of open database connections.
    #[arg(long, env = "VEIL_DB_MAX_CONNECTIONS", default_value_t = 5)]
    pub db_max_connections: u32,
}

/// How events are read from Soroban RPC.
#[derive(Clone, Debug, Args)]
pub struct IngestConfig {
    /// Ledger to start from when the database has no RPC checkpoint. Without
    /// it, RPC starts from the completed seed (see `seed`). Ignored once a
    /// checkpoint exists.
    #[arg(long, env = "VEIL_START_LEDGER")]
    pub start_ledger: Option<u32>,

    /// Events requested per `getEvents` call.
    #[arg(
        long,
        env = "VEIL_PAGE_SIZE",
        default_value_t = 200,
        value_parser = clap::value_parser!(u32).range(1..=10_000)
    )]
    pub page_size: u32,

    /// Ledgers before the seed's last ledger that RPC re-reads, so the two
    /// sources overlap and are cross-checked.
    #[arg(long, env = "VEIL_OVERLAP_LEDGERS", default_value_t = crate::seed::DEFAULT_OVERLAP)]
    pub overlap_ledgers: u32,

    /// Seconds before an RPC request is abandoned and retried.
    #[arg(long, env = "VEIL_RPC_TIMEOUT_SECS", default_value_t = 30)]
    pub rpc_timeout_secs: u64,
}

/// Where history older than RPC retention is read from.
#[derive(Clone, Debug, Args)]
pub struct ExpertConfig {
    /// Stellar Expert API base for the network.
    #[arg(
        long,
        env = "VEIL_EXPERT_URL",
        default_value = "https://api.stellar.expert/explorer/testnet"
    )]
    pub expert_url: String,
}

/// The long-running service.
#[derive(Clone, Debug, Args)]
pub struct ServiceArgs {
    /// Address the health and metrics server listens on.
    #[arg(long, env = "VEIL_LISTEN", default_value = "0.0.0.0:8080")]
    pub listen: std::net::SocketAddr,

    /// Seconds between syncs once at the tip.
    #[arg(long, env = "VEIL_POLL_SECS", default_value_t = 5)]
    pub poll_secs: u64,

    /// `/ready` fails when the index is more than this many ledgers behind.
    #[arg(long, env = "VEIL_READY_MAX_LAG", default_value_t = 10)]
    pub ready_max_lag: u32,

    /// `/ready` fails when no sync has succeeded for this many seconds.
    #[arg(long, env = "VEIL_READY_MAX_SILENCE_SECS", default_value_t = 120)]
    pub ready_max_silence_secs: u64,

    /// Seconds between periodic `verify` runs, published on `/verify`.
    /// 0 turns them off.
    #[arg(long, env = "VEIL_VERIFY_EVERY_SECS", default_value_t = 600)]
    pub verify_every_secs: u64,

    /// Seed from Stellar Expert on start when the database is empty.
    #[arg(
        long,
        env = "VEIL_SEED_IF_EMPTY",
        default_value_t = true,
        action = clap::ArgAction::Set
    )]
    pub seed_if_empty: bool,
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

fn parse_secret(value: &str) -> Result<Secret, String> {
    if value.is_empty() {
        return Err("must not be empty".to_owned());
    }
    Ok(Secret::new(value.to_owned()))
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
