//! Developer tools: inspect the rekordbox library, play a track through the simulator,
//! listen to the loopback capture. Nothing here ships to end users.
use std::path::PathBuf;

use clap::{Parser, Subcommand};
use onset_rekordbox::paths::RekordboxPaths;

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
}

fn cmd_library(app_dir: Option<PathBuf>, grep: Option<String>) -> anyhow::Result<()> {
    let paths = resolve_paths(app_dir)?;
    let lib = onset_rekordbox::library::Library::open(&paths, &cache_dir()?)?;
    let needle = grep.map(|g| g.to_lowercase());
    let mut shown = 0;
    for t in lib.tracks() {
        if let Some(n) = &needle
            && !t.title.to_lowercase().contains(n)
            && !t.artist.to_lowercase().contains(n)
        {
            continue;
        }
        let bpm = t.bpm.map_or("  -  ".to_string(), |b| format!("{b:6.2}"));
        let art = t
            .artwork_path
            .as_ref()
            .map_or("no-art", |p| if p.exists() { "art" } else { "art?" });
        println!(
            "{}\t{bpm}\t{:<4}\t{} - {}\t{art}",
            t.id.0,
            t.key.as_deref().unwrap_or("-"),
            t.artist,
            t.title
        );
        shown += 1;
    }
    eprintln!("{shown} of {} tracks", lib.tracks().len());
    Ok(())
}

fn resolve_paths(app_dir: Option<PathBuf>) -> anyhow::Result<RekordboxPaths> {
    Ok(match app_dir {
        Some(dir) => RekordboxPaths::from_app_dir(&dir)?,
        None => RekordboxPaths::discover()?,
    })
}

fn cache_dir() -> anyhow::Result<PathBuf> {
    directories::ProjectDirs::from("", "Onset", "Onset")
        .map(|d| d.cache_dir().to_path_buf())
        .ok_or_else(|| anyhow::anyhow!("no cache directory available"))
}

fn cmd_schema(app_dir: Option<PathBuf>) -> anyhow::Result<()> {
    let paths = resolve_paths(app_dir)?;
    let db = onset_rekordbox::cache::plaintext_db(&paths, &cache_dir()?)?;
    let conn =
        rusqlite::Connection::open_with_flags(&db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let mut stmt = conn.prepare(
        "SELECT name, sql FROM sqlite_master WHERE type = 'table' AND sql IS NOT NULL ORDER BY name",
    )?;
    let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
    for row in rows {
        let (name, sql) = row?;
        println!("-- {name}\n{sql};\n");
    }
    Ok(())
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
        Command::Schema { app_dir } => cmd_schema(app_dir)?,
        Command::Library { app_dir, grep } => cmd_library(app_dir, grep)?,
    }
    Ok(())
}
