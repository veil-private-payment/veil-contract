//! Structured logging.
//!
//! The level filter comes from `RUST_LOG` (default `info`, with sqlx's
//! per-statement notices at `warn`). Logs go to stderr
//! so stdout stays free for command output such as `decode`.

use tracing_subscriber::EnvFilter;

use crate::config::LogFormat;

/// Installs the global subscriber. Call once, before anything logs.
pub fn init(format: LogFormat) {
    let filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info,sqlx=warn"));
    let builder = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr);
    match format {
        LogFormat::Json => builder.json().with_current_span(false).init(),
        LogFormat::Pretty => builder.init(),
    }
}
