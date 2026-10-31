//! Writes to the chain go through the `stellar` CLI.
//!
//! The CLI keeps each demo account's secret under an alias, simulates the
//! call against RPC, signs and submits it. This tool only passes it the
//! contract arguments, so it never holds a Stellar secret.

use std::process::Command;

use anyhow::{Context, Result, bail};

pub struct Cli {
    pub network: String,
}

/// A submitted transaction.
#[derive(Debug, Clone)]
pub struct Submitted {
    pub hash: Option<String>,
    pub output: String,
}

impl Submitted {
    pub fn explorer(&self, network: &str) -> String {
        self.hash.as_deref().map_or_else(
            || "(hash not reported by the stellar CLI)".to_owned(),
            |hash| format!("https://stellar.expert/explorer/{network}/tx/{hash}"),
        )
    }
}

fn run(command: &mut Command) -> Result<(String, String)> {
    let shown = format!("{command:?}");
    let output = command
        .output()
        .with_context(|| format!("running {shown} (is the stellar CLI installed?)"))?;
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
    if !output.status.success() {
        bail!("stellar CLI failed: {stderr}\n{stdout}");
    }
    Ok((stdout, stderr))
}

/// The first 64-hex-digit word in `text`: the CLI reports the hash of the
/// transaction it signed.
fn transaction_hash(text: &str) -> Option<String> {
    text.split(|c: char| !c.is_ascii_hexdigit())
        .find(|word| word.len() == 64)
        .map(str::to_lowercase)
}

impl Cli {
    /// Creates an identity and funds it from friendbot (testnet).
    pub fn create_funded(&self, alias: &str) -> Result<String> {
        run(Command::new("stellar").args([
            "keys",
            "generate",
            alias,
            "--network",
            &self.network,
            "--fund",
        ]))?;
        self.address(alias)
    }

    pub fn address(&self, alias: &str) -> Result<String> {
        Ok(run(Command::new("stellar").args(["keys", "address", alias]))?.0)
    }

    /// Calls `function` on `contract`, signed by `alias`, and returns once the
    /// transaction is in a ledger.
    pub fn invoke(
        &self,
        contract: &str,
        alias: &str,
        function: &str,
        args: &[(&str, String)],
    ) -> Result<Submitted> {
        let mut command = Command::new("stellar");
        command.args([
            "contract",
            "invoke",
            "--id",
            contract,
            "--network",
            &self.network,
            "--source-account",
            alias,
            "--send=yes",
            "--",
            function,
        ]);
        for (name, value) in args {
            command.arg(format!("--{name}")).arg(value);
        }
        let (stdout, stderr) = run(&mut command)?;
        Ok(Submitted {
            hash: transaction_hash(&stderr),
            output: stdout,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_hash_is_found_in_cli_output() {
        let stderr = "ℹ️ Signing transaction: 64afcf21c7adbd1bd144db3b978da85a1a510d1785a8ac39c193ed3b0766f428\n✅ done";
        assert_eq!(
            transaction_hash(stderr).as_deref(),
            Some("64afcf21c7adbd1bd144db3b978da85a1a510d1785a8ac39c193ed3b0766f428")
        );
        assert_eq!(transaction_hash("no hash here 1234"), None);
    }
}
