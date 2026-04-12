mod config;
mod statusline;
mod validate_hooks;

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
    Statusline {
        /// Disable color output
        #[arg(long)]
        no_color: bool,
    },
    /// Validate hooks configuration
    ValidateHooks,
}

fn main() {
    let cli = Cli::parse();

    let result = match cli.command {
        Command::Statusline { no_color } => statusline::handle_stdin(no_color),
        Command::ValidateHooks => validate_hooks::run(),
    };

    if let Err(e) = result {
        eprintln!("Error: {e:?}");
        std::process::exit(1);
    }
}
