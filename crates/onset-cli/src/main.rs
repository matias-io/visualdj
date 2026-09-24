//! Developer tools: inspect the rekordbox library, play a track through the simulator,
//! listen to the loopback capture. Nothing here ships to end users.
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "onset-cli", about = "Onset developer tools", version)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Print the workspace version
    Version,
}

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();
    match Cli::parse().command {
        Command::Version => println!("onset {}", env!("CARGO_PKG_VERSION")),
    }
    Ok(())
}
