//! Developer tools: inspect the rekordbox library, play a track through the simulator,
//! listen to the loopback capture. Nothing here ships to end users.
mod common;
mod library_cmds;
mod live;

use std::path::PathBuf;

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
    /// Print the table schema of the (decrypted) rekordbox database
    Schema {
        /// rekordbox data folder (defaults to %APPDATA%\Pioneer\rekordbox)
        #[arg(long)]
        app_dir: Option<PathBuf>,
    },
    /// List the collection with BPM, key and artwork presence
    Library {
        #[arg(long)]
        app_dir: Option<PathBuf>,
        /// Case-insensitive substring filter on title or artist
        #[arg(long)]
        grep: Option<String>,
    },
    /// Show a track's beat grid summary, phrases and cues
    Anlz {
        #[arg(long)]
        app_dir: Option<PathBuf>,
        /// Track title (case-insensitive exact match)
        title: String,
    },
    /// Play a track through the simulator and print live beat, phrase and drop state
    Sim {
        #[arg(long)]
        app_dir: Option<PathBuf>,
        /// Track title (case-insensitive exact match)
        title: String,
        /// Start position in seconds
        #[arg(long, default_value_t = 0.0)]
        seek: f64,
        /// Output gain 0..1
        #[arg(long, default_value_t = 0.5)]
        gain: f32,
        /// Also run the band analyzer on the simulator's audio and show a meter
        #[arg(long)]
        analyze: bool,
        /// Stop after this many seconds (0 = until the track ends or `q`)
        #[arg(long, default_value_t = 0.0)]
        seconds: f64,
    },
    /// List audio output endpoints that can be loopback-captured
    Devices,
    /// Capture an output endpoint (WASAPI loopback) and show a 24-band meter
    Listen {
        /// Substring of the endpoint name; default output device when omitted
        #[arg(long)]
        device: Option<String>,
        /// Stop after this many seconds (0 = until `q`)
        #[arg(long, default_value_t = 0.0)]
        seconds: f64,
    },
}

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        .init();
    match Cli::parse().command {
        Command::Version => println!("onset {}", env!("CARGO_PKG_VERSION")),
        Command::Schema { app_dir } => library_cmds::schema(app_dir)?,
        Command::Library { app_dir, grep } => library_cmds::library(app_dir, grep)?,
        Command::Anlz { app_dir, title } => library_cmds::anlz(app_dir, &title)?,
        Command::Sim {
            app_dir,
            title,
            seek,
            gain,
            analyze,
            seconds,
        } => live::sim(app_dir, &title, seek, gain, analyze, seconds)?,
        Command::Devices => live::devices(),
        Command::Listen { device, seconds } => live::listen(device.as_deref(), seconds)?,
    }
    Ok(())
}
