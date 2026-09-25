//! Onset: real-time, structure-aware visuals for rekordbox DJs.
mod app;
mod bench;
mod config;
mod engine;
mod launcher;
mod monitors;
mod overlay;

use std::path::PathBuf;
use std::time::Duration;

use clap::Parser;
use winit::event_loop::EventLoop;

use crate::app::{AppOptions, OnsetApp};
use crate::config::{Config, MonitorChoice};
use crate::engine::{Engine, EngineConfig};

#[derive(Parser)]
#[command(
    name = "onset",
    about = "Real-time, structure-aware visuals for rekordbox DJs",
    version
)]
struct Cli {
    /// Output monitor: a zero-based index, or part of its name (overrides the config)
    #[arg(long)]
    monitor: Option<String>,
    /// Developer mode: play this library title through the built-in simulator instead of
    /// reading rekordbox
    #[arg(long)]
    sim: Option<String>,
    /// rekordbox data folder (defaults to %APPDATA%\Pioneer\rekordbox)
    #[arg(long)]
    app_dir: Option<PathBuf>,
    /// Simulator start position in seconds
    #[arg(long, default_value_t = 0.0)]
    seek: f64,
    /// Simulator output gain 0..1
    #[arg(long, default_value_t = 0.5)]
    gain: f32,
    /// Start on this scene (overrides the config)
    #[arg(long)]
    scene: Option<String>,
    /// Close after this many seconds (for unattended checks)
    #[arg(long)]
    exit_after: Option<f64>,
    /// Print where the config file lives and exit
    #[arg(long)]
    config_path: bool,
    /// Run for this many seconds without vsync, then print frame statistics and exit
    #[arg(long, value_name = "SECONDS")]
    bench: Option<f64>,
    /// Start with the settings panel open (Tab toggles it)
    #[arg(long)]
    settings: bool,
    /// Save one frame as PNG here, a second before exit (or 3 s in without --exit-after)
    #[arg(long, value_name = "PNG")]
    screenshot: Option<PathBuf>,
    /// Skip the launcher and put the show on the configured monitor at once
    #[arg(long)]
    show: bool,
}

fn parse_monitor(s: &str) -> MonitorChoice {
    s.parse::<usize>().map_or_else(
        |_| MonitorChoice::NameContains(s.to_string()),
        MonitorChoice::Index,
    )
}

/// `ONSET_OFFSETS` if set, else `offsets/` beside the executable, else `offsets/` in the
/// working directory (the development layout).
fn offsets_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("ONSET_OFFSETS") {
        return PathBuf::from(dir);
    }
    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
    {
        let beside = dir.join("offsets");
        if beside.is_dir() {
            return beside;
        }
    }
    PathBuf::from("offsets")
}

fn cache_dir() -> PathBuf {
    directories::ProjectDirs::from("", "Onset", "Onset")
        .map_or_else(|| PathBuf::from(".cache"), |d| d.cache_dir().to_path_buf())
}

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
                tracing_subscriber::EnvFilter::new("info,wgpu_core=warn,wgpu_hal=warn")
            }),
        )
        .with_writer(std::io::stderr)
        .init();
    let cli = Cli::parse();
    if cli.config_path {
        println!("{}", Config::path().display());
        return Ok(());
    }
    let config = Config::load();
    let offsets_dir = offsets_dir();
    // Unattended runs and the developer simulator go straight to the show.
    let launcher = !(cli.show
        || cli.sim.is_some()
        || cli.exit_after.is_some()
        || cli.bench.is_some()
        || cli.screenshot.is_some()
        || cli.settings);

    // Onset is a visualiser: rekordbox is the source. The simulator runs only when asked.
    let engine = match Engine::start(EngineConfig {
        app_dir: cli.app_dir.clone(),
        cache_dir: cache_dir(),
        offsets_dir: offsets_dir.clone(),
        sim_track: cli.sim.clone(),
        sim_seek_s: cli.seek,
        sim_gain: cli.gain,
        audio_device: config.audio_device.clone(),
        capture_audio: cli.sim.is_none(),
    }) {
        Ok(e) => {
            tracing::info!(status = ?e.status(), "engine started");
            Some(e)
        }
        Err(e) => {
            tracing::error!("engine did not start (visuals will idle): {e:#}");
            None
        }
    };

    let opts = AppOptions {
        config,
        monitor: cli.monitor.as_deref().map(parse_monitor),
        scene: cli.scene.clone(),
        exit_after: cli.exit_after.or(cli.bench).map(Duration::from_secs_f64),
        bench: cli.bench.is_some(),
        settings_open: cli.settings,
        screenshot: cli.screenshot.clone(),
        launcher,
        offsets_dir,
        engine,
    };
    let event_loop = EventLoop::new()?;
    let mut app = OnsetApp::new(opts);
    event_loop.run_app(&mut app)?;
    Ok(())
}
