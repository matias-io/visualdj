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
    /// Find a paused deck's displayed time in memory, then keep what moves after play
    Memfind {
        /// The deck's elapsed time as shown, in seconds (e.g. 140.4 for 02:20.4)
        seconds: f64,
        #[arg(long, default_value_t = 0.06)]
        tolerance: f64,
        /// Seconds to wait for the operator to press play before re-checking
        #[arg(long, default_value_t = 20)]
        wait: u64,
        #[arg(long, default_value_t = 6)]
        depth: usize,
        #[arg(long, default_value_t = 64)]
        branch: usize,
    },
    /// Find the master-deck flag by moving MASTER between two calibrated decks
    Memmaster {
        /// Deck 1's position chain in rkbx notation
        deck1: String,
        /// Deck 2's position chain in rkbx notation
        deck2: String,
        /// Seconds given to the operator for each MASTER press
        #[arg(long, default_value_t = 20)]
        wait: u64,
        /// Bytes watched from the start of each object along the chains
        #[arg(long, default_value_t = 0x10000)]
        window: usize,
    },
    /// Find bytes near the decks that follow a two-way change (MASTER on one deck or the
    /// other, a pad playing or not); a trigger file marks each state
    Memflag {
        /// The state (0 or 1) set up before each trigger, in order
        #[arg(long, value_delimiter = ',')]
        labels: Vec<u8>,
        /// Created by the operator after each change
        #[arg(long)]
        trigger: PathBuf,
        /// Offsets folder (default: `offsets/` or `ONSET_OFFSETS`)
        #[arg(long)]
        offsets_dir: Option<PathBuf>,
        /// Save the raw windows here
        #[arg(long)]
        dump: Option<PathBuf>,
        /// Analyse a saved dump instead of reading rekordbox
        #[arg(long)]
        from: Option<PathBuf>,
    },
    /// Print every static pointer chain to the given absolute addresses in rekordbox
    Memchains {
        /// Addresses in hex, e.g. 0x2d81797d2b4
        addrs: Vec<String>,
        #[arg(long, default_value_t = 6)]
        depth: usize,
        #[arg(long, default_value_t = 64)]
        branch: usize,
    },
    /// Resolve pointer chains (in the rkbx-link line format) against the running rekordbox
    Memresolve {
        /// Chains such as "05C64808 2A0 10 1175 109A 0"
        chains: Vec<String>,
    },
    /// Derive the pointer chains for the running rekordbox version and write
    /// offsets/<version>.toml. Each step is announced and detected in memory; no typing.
    Calibrate {
        /// Where to write the offsets file (default: `offsets/` or `ONSET_OFFSETS`)
        #[arg(long)]
        out_dir: Option<PathBuf>,
        /// Seconds allowed for each step before the calibrator carries on without it
        #[arg(long, default_value_t = 180)]
        step_seconds: u64,
        /// Repair the existing file for the running version (keep only the pointers every
        /// deck shares) instead of calibrating; needs no deck interaction
        #[arg(long)]
        refine: bool,
    },
}

#[allow(clippy::too_many_lines)] // one match arm per command
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
        Command::Memfind {
            seconds,
            tolerance,
            wait,
            depth,
            branch,
        } => memscan::memfind(seconds, tolerance, wait, depth, branch)?,
        #[cfg(not(windows))]
        Command::Memfind { .. } => anyhow::bail!("memfind needs Windows"),
        #[cfg(windows)]
        Command::Memmaster {
            deck1,
            deck2,
            wait,
            window,
        } => memscan::memmaster(&deck1, &deck2, wait, window)?,
        #[cfg(not(windows))]
        Command::Memmaster { .. } => anyhow::bail!("memmaster needs Windows"),
        #[cfg(windows)]
        Command::Memflag {
            labels,
            trigger,
            offsets_dir,
            dump,
            from,
        } => memscan::memflag(
            &offsets_dir.unwrap_or_else(calibrate::default_out_dir),
            &labels,
            &trigger,
            dump.as_deref(),
            from.as_deref(),
        )?,
        #[cfg(not(windows))]
        Command::Memflag { .. } => anyhow::bail!("memflag needs Windows"),
        #[cfg(windows)]
        Command::Memchains {
            addrs,
            depth,
            branch,
        } => memscan::memchains(&addrs, depth, branch)?,
        #[cfg(not(windows))]
        Command::Memchains { .. } => anyhow::bail!("memchains needs Windows"),
        #[cfg(windows)]
        Command::Memresolve { chains } => memscan::memresolve(&chains)?,
        #[cfg(not(windows))]
        Command::Memresolve { .. } => anyhow::bail!("memresolve needs Windows"),
        #[cfg(windows)]
        Command::Calibrate {
            out_dir,
            step_seconds,
            refine,
        } => {
            let dir = out_dir.unwrap_or_else(calibrate::default_out_dir);
            if refine {
                calibrate::refine(&dir)?;
            } else {
                calibrate::calibrate(&dir, step_seconds)?;
            }
        }
        #[cfg(not(windows))]
        Command::Calibrate { .. } => anyhow::bail!("calibrate needs Windows"),
    }
    Ok(())
}
