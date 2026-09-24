//! Developer tools: inspect the rekordbox library, play a track through the simulator,
//! listen to the loopback capture. Nothing here ships to end users.
#[cfg(windows)]
mod calibrate;
mod common;
mod library_cmds;
mod live;
#[cfg(windows)]
mod memscan;

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
    /// Scan a running rekordbox for its live values and the pointer chains that reach them
    Memscan {
        /// BPM shown on the playing deck, to confirm tempo candidates
        #[arg(long)]
        bpm: Option<f32>,
        /// Title of the track loaded on the deck of interest (finds its text block and path)
        #[arg(long)]
        title: Option<String>,
        /// rekordbox data folder, for the track's analysis path
        #[arg(long)]
        app_dir: Option<PathBuf>,
        /// Skip the playback-counter search (nothing is playing)
        #[arg(long)]
        no_counters: bool,
        /// Window between the two memory snapshots that detect audio counters
        #[arg(long, default_value_t = 300)]
        dt_ms: u64,
        /// Pointer scan depth (hops from a static root to the value)
        #[arg(long, default_value_t = 6)]
        depth: usize,
        /// Intermediate pointers followed per level
        #[arg(long, default_value_t = 48)]
        branch: usize,
        /// Only report strings, known tails and counters; skip the pointer scan
        #[arg(long)]
        no_pointers: bool,
        /// Write the full report here
        #[arg(long)]
        out: Option<PathBuf>,
    },
    /// Resolve pointer chains (in the rkbx-link line format) against the running rekordbox
    Memresolve {
        /// Chains such as "05C64808 2A0 10 1175 109A 0"
        chains: Vec<String>,
    },
    /// Derive the pointer chains for the running rekordbox version (interactive) and write
    /// offsets/<version>.toml
    Calibrate {
        /// Where to write the offsets file (default: `offsets/` or `ONSET_OFFSETS`)
        #[arg(long)]
        out_dir: Option<PathBuf>,
        /// Title of the track already playing on deck 1 (skips the first question)
        #[arg(long)]
        title: Option<String>,
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
        #[cfg(windows)]
        Command::Memscan {
            bpm,
            title,
            app_dir,
            no_counters,
            dt_ms,
            depth,
            branch,
            no_pointers,
            out,
        } => memscan::memscan(&memscan::MemscanArgs {
            bpm,
            title,
            app_dir,
            dt_ms,
            depth,
            branch,
            skip_pointers: no_pointers,
            skip_counters: no_counters,
            out,
        })?,
        #[cfg(not(windows))]
        Command::Memscan { .. } => anyhow::bail!("memscan needs Windows"),
        #[cfg(windows)]
        Command::Memresolve { chains } => memscan::memresolve(&chains)?,
        #[cfg(not(windows))]
        Command::Memresolve { .. } => anyhow::bail!("memresolve needs Windows"),
        #[cfg(windows)]
        Command::Calibrate { out_dir, title } => {
            calibrate::calibrate(&out_dir.unwrap_or_else(calibrate::default_out_dir), title)?;
        }
        #[cfg(not(windows))]
        Command::Calibrate { .. } => anyhow::bail!("calibrate needs Windows"),
    }
    Ok(())
}
