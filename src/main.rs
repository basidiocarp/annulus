mod bridge;
mod config;
mod config_export;
mod notify;
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
        /// Render once and exit (skip polling/refresh loops)
        #[arg(long)]
        once: bool,
    },
    /// Show ecosystem availability status
    Status {
        /// Output JSON instead of human-readable table
        #[arg(long)]
        json: bool,
    },
    /// Show and clear canopy notifications
    Notify {
        /// Poll for and print unread notifications, then mark them as read
        #[arg(long)]
        poll: bool,
        /// Send system notification (macOS only, opt-in)
        #[arg(long)]
        system: bool,
    },
    /// Manage configuration
    Config {
        #[command(subcommand)]
        subcommand: ConfigCommand,
    },
    /// Validate hooks configuration
    ValidateHooks,
}

#[derive(Subcommand)]
enum ConfigCommand {
    /// Export resolved configuration as resolved-status-customization-v1 JSON
    Export,
}

fn main() {
    let cli = Cli::parse();

    let result = match cli.command {
        Command::Statusline {
            no_color,
            json,
            once,
        } => statusline::handle_stdin(json, no_color, once),
        Command::Status { json } => {
            if json {
                println!("{}", status::status_json());
            } else {
                print!("{}", status::status_table());
            }
            Ok(())
        }
        Command::Notify { poll, system } => notify::handle(poll, system),
        Command::Config { subcommand } => match subcommand {
            ConfigCommand::Export => config_export::handle_config_export(),
        },
        Command::ValidateHooks => validate_hooks::run(),
    };

    if let Err(e) = result {
        eprintln!("Error: {e:?}");
        std::process::exit(1);
    }
}
