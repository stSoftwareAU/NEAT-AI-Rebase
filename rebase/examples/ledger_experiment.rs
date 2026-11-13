//! Opt-in, paired ledger replay experiment. See docs/ledger-experiment.md.
//!
//! cargo run --release --example ledger_experiment -- freeze draft.json manifest.json
//! cargo run --release --example ledger_experiment -- run manifest.json output-directory

mod ledger;

use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser)]
struct Args {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Create synthetic smoke inputs; never evidence of representative benefit.
    PrepareSmoke { output: PathBuf, scorer: PathBuf },
    /// Pin the protocol and every input before validation.
    Freeze { draft: PathBuf, manifest: PathBuf },
    /// Run or resume both arms; never publish a population candidate.
    Run { manifest: PathBuf, output: PathBuf },
}

fn main() -> Result<(), String> {
    match Args::parse().command {
        Command::PrepareSmoke { output, scorer } => ledger::smoke::prepare(&output, &scorer),
        Command::Freeze { draft, manifest } => ledger::freeze(&draft, &manifest),
        Command::Run { manifest, output } => {
            let scorer = ledger::scorer::BoundedScorer::from_manifest(&manifest)?;
            let result = ledger::run(&manifest, &output, &scorer)?;
            println!(
                "{}",
                serde_json::to_string_pretty(&result).map_err(|e| e.to_string())?
            );
            if result["status"] != "complete" {
                std::process::exit(3);
            }
            Ok(())
        }
    }
}
