//! Onset: real-time, structure-aware visuals for rekordbox DJs.
mod app;
mod config;
mod engine;
mod monitors;

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
    /// Play this track title through the built-in simulator
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
    /// Close after this many seconds (for unattended checks)
    #[arg(long)]
    exit_after: Option<f64>,
    /// Print where the config file lives and exit
    #[arg(long)]
    config_path: bool,
}

fn parse_monitor(s: &str) -> MonitorChoice {
    s.parse::<usize>().map_or_else(
        |_| MonitorChoice::NameContains(s.to_string()),
        MonitorChoice::Index,
    )
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

    let sim_track = cli.sim.clone().or_else(|| config.sim_track.clone());
    let engine = match Engine::start(EngineConfig {
        app_dir: cli.app_dir.clone(),
        cache_dir: cache_dir(),
        sim_track,
        sim_seek_s: cli.seek,
        sim_gain: cli.gain,
        audio_device: config.audio_device.clone(),
        capture_audio: false,
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
        exit_after: cli.exit_after.map(Duration::from_secs_f64),
        engine,
    };
    let event_loop = EventLoop::new()?;
    let mut app = OnsetApp::new(opts);
    event_loop.run_app(&mut app)?;
    Ok(())
}
