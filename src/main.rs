mod statusline;
mod validate_hooks;

use anyhow::Result;
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "annulus")]
#[command(about = "Cross-ecosystem operator utilities for the Basidiocarp ecosystem")]
#[command(version)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Print statusline information
    Statusline,
    /// Validate hooks configuration
    ValidateHooks,
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Command::Statusline => statusline::run(),
        Command::ValidateHooks => validate_hooks::run(),
    }
}
