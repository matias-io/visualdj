//! Onset: real-time, structure-aware visuals for rekordbox DJs.
mod app;
mod config;
mod monitors;

use std::time::Duration;

use clap::Parser;
use winit::event_loop::EventLoop;

use crate::app::{AppOptions, OnsetApp};
use crate::config::{Config, MonitorChoice};

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
    let opts = AppOptions {
        config,
        monitor: cli.monitor.as_deref().map(parse_monitor),
        exit_after: cli.exit_after.map(Duration::from_secs_f64),
    };
    let event_loop = EventLoop::new()?;
    let mut app = OnsetApp::new(opts);
    event_loop.run_app(&mut app)?;
    Ok(())
}
