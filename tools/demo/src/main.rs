use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{Context as _, Result, bail};
use clap::{Parser, Subcommand};
use veil_demo::api::Api;
use veil_demo::chain::Cli;
use veil_demo::flows::{self, Context};
use veil_demo::wallet::Wallet;

/// Make a private payment on testnet using only the Veil read API.
#[derive(Debug, Parser)]
#[command(name = "veil-demo", version, about)]
struct Args {
    /// The read API.
    #[arg(
        long,
        env = "VEIL_API_URL",
        default_value = "https://api-production-4d5ab.up.railway.app"
    )]
    api: String,

    /// `stellar` CLI network name.
    #[arg(long, env = "VEIL_NETWORK", default_value = "testnet")]
    network: String,

    /// Faucet that enrols keys on the allowlist (deployments/testnet.json).
    #[arg(
        long,
        env = "VEIL_FAUCET_CONTRACT",
        default_value = "CBQNDLJFT3WIAWMLCGJX7TAPOFLGNRAV73EUF6VVZM33SMSUYHAWW56N"
    )]
    faucet: String,

    /// Where wallet files are kept.
    #[arg(long, env = "VEIL_WALLETS", default_value = ".veil-demo")]
    wallets: PathBuf,

    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Create a wallet: a funded testnet account and fresh note keys.
    New { name: String },
    /// Enrol the wallet's key on the allowlist and publish its receiving keys.
    Enroll { name: String },
    /// Move XLM from the wallet's account into a private note.
    Shield { name: String, amount: String },
    /// List the wallet's unspent private notes, found by scanning the API.
    Balance { name: String },
    /// Pay privately to another wallet (by name) or address.
    Pay {
        from: String,
        to: String,
        amount: String,
    },
}

/// Parses an XLM amount with up to 7 decimals into stroops.
fn stroops(amount: &str) -> Result<u128> {
    let (whole, fraction) = amount.split_once('.').unwrap_or((amount, ""));
    if fraction.len() > 7 || whole.is_empty() && fraction.is_empty() {
        bail!("{amount} is not an XLM amount with at most 7 decimals");
    }
    let whole: u128 = if whole.is_empty() {
        0
    } else {
        whole.parse().context("amount")?
    };
    let fraction: u128 = format!("{fraction:0<7}").parse().context("amount")?;
    whole
        .checked_mul(10_000_000)
        .and_then(|value| value.checked_add(fraction))
        .context("amount is too large")
}

fn xlm(stroops: u128) -> String {
    format!("{}.{:07}", stroops / 10_000_000, stroops % 10_000_000)
}

#[tokio::main]
async fn main() -> ExitCode {
    match run(Args::parse()).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("error: {err:#}");
            ExitCode::FAILURE
        }
    }
}

async fn run(args: Args) -> Result<()> {
    let api = Api::new(&args.api)?;
    let status = api.status().await.context("reaching the API")?;
    let ctx = Context {
        api,
        cli: Cli {
            network: args.network.clone(),
        },
        pool: status.pool.clone(),
        faucet: args.faucet,
        wallets: args.wallets,
    };
    let explorer = |submitted: &veil_demo::chain::Submitted| submitted.explorer(&args.network);

    match args.command {
        Command::New { name } => {
            let alias = format!("veil-demo-{name}");
            println!("creating testnet account {alias} (funded by friendbot)...");
            let address = ctx.cli.create_funded(&alias)?;
            let wallet = Wallet::create(&name, alias, address)?;
            let file = wallet.save(&ctx.wallets)?;
            println!("wallet {name}: {}", wallet.address);
            println!(
                "  note public key   {}",
                flows::scalar_hex(wallet.note_public_key())
            );
            println!(
                "  encryption key    0x{}",
                hex::encode(wallet.encryption_key())
            );
            println!(
                "  saved to {} (testnet keys, owner-readable only)",
                file.display()
            );
        }
        Command::Enroll { name } => {
            let wallet = Wallet::load(&ctx.wallets, &name)?;
            println!("enrolling {name} ({})", wallet.address);
            for submitted in flows::enroll(&ctx, &wallet).await? {
                println!("  tx {}", explorer(&submitted));
            }
            println!("  on the allowlist and receiving keys published (read back from the API)");
        }
        Command::Shield { name, amount } => {
            let wallet = Wallet::load(&ctx.wallets, &name)?;
            let amount = stroops(&amount)?;
            println!("shielding {} XLM from {}", xlm(amount), wallet.address);
            let submitted = flows::shield(&ctx, &wallet, amount).await?;
            println!("  tx {}", explorer(&submitted));
        }
        Command::Balance { name } => {
            let wallet = Wallet::load(&ctx.wallets, &name)?;
            let notes = flows::scan(&ctx, &wallet).await?;
            let total: u128 = notes
                .iter()
                .map(|note| note.amount)
                .fold(0, u128::saturating_add);
            println!(
                "{name}: {} XLM in {} private note(s)",
                xlm(total),
                notes.len()
            );
            for note in notes {
                println!("  leaf {:>5}  {} XLM", note.leaf_index, xlm(note.amount));
            }
        }
        Command::Pay { from, to, amount } => {
            let wallet = Wallet::load(&ctx.wallets, &from)?;
            let recipient = if to.starts_with('G') && to.len() == 56 {
                to
            } else {
                Wallet::load(&ctx.wallets, &to)?.address
            };
            let amount = stroops(&amount)?;
            println!(
                "paying {} XLM privately from {from} to {recipient}",
                xlm(amount)
            );
            let submitted = flows::pay(&ctx, &wallet, &recipient, amount).await?;
            println!("  tx {}", explorer(&submitted));
            println!("  the transaction shows neither the amount nor the recipient");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn amounts_parse_to_stroops() {
        assert_eq!(stroops("1").expect("parses"), 10_000_000);
        assert_eq!(stroops("0.5").expect("parses"), 5_000_000);
        assert_eq!(stroops("2.0000001").expect("parses"), 20_000_001);
        assert!(stroops("1.00000001").is_err());
        assert!(stroops("abc").is_err());
        assert!(stroops(".").is_err());
        assert_eq!(xlm(20_000_001), "2.0000001");
    }
}
