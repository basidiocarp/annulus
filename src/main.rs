mod config;
mod providers;
mod status;
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
        /// Output JSON instead of terminal statusline
        #[arg(long)]
        json: bool,
    },
    /// Show ecosystem availability status
    Status {
        /// Output JSON instead of human-readable table
        #[arg(long)]
        json: bool,
    },
    /// Validate hooks configuration
    ValidateHooks,
}

fn main() {
    let cli = Cli::parse();

    let result = match cli.command {
        Command::Statusline { no_color, json } => statusline::handle_stdin(json, no_color),
        Command::Status { json } => {
            if json {
                println!("{}", status::status_json());
            } else {
                print!("{}", status::status_table());
            }
            Ok(())
        }
        Command::ValidateHooks => validate_hooks::run(),
    };

    if let Err(e) = result {
        eprintln!("Error: {e:?}");
        std::process::exit(1);
    }
}
