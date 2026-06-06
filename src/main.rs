mod commands;
mod max;
mod project;

use anyhow::Result;
use clap::{Parser, Subcommand};

/// rackabel — build Max for Live devices and Ableton Live extensions.
#[derive(Parser)]
#[command(name = "rackabel", version, about, long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Scaffold a new M4L device project
    New {
        /// Name of the device project to create
        name: String,
        /// Device type to scaffold
        #[arg(long, value_enum, default_value_t = commands::new::DeviceKind::AudioEffect)]
        kind: commands::new::DeviceKind,
    },
    /// Assemble the device into a distributable .amxd
    Build,
    /// Copy the built device into Ableton's User Library
    Install,
    /// Rebuild (and reinstall) whenever source files change
    Watch,
    /// Check your environment: Max, Ableton Live, and library paths
    Doctor,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::New { name, kind } => commands::new::run(&name, kind),
        Command::Build => commands::build::run(),
        Command::Install => commands::install::run(),
        Command::Watch => commands::watch::run(),
        Command::Doctor => commands::doctor::run(),
    }
}
