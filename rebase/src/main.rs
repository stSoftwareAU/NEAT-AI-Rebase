//! `neat_ai_rebase` — rebase portable NEAT-AI improvements onto the latest
//! champion and let the scorer decide.
//!
//! See [`neat_ai_rebase::cli`] for the flags, the outputs and the exit codes.

use clap::Parser;
use neat_ai_rebase::cli::{Cli, run};

fn main() -> std::process::ExitCode {
    let cli = Cli::parse();
    match run(&cli) {
        Ok(code) => std::process::ExitCode::from(code as u8),
        Err(error) => {
            eprintln!("neat_ai_rebase: {error}");
            std::process::ExitCode::from(error.code as u8)
        }
    }
}
