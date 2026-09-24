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
    /// Show a track's beat grid summary, phrases and cues
    Anlz {
        #[arg(long)]
        app_dir: Option<PathBuf>,
        /// Track title (case-insensitive exact match)
        title: String,
    },
}

fn cmd_anlz(app_dir: Option<PathBuf>, title: &str) -> anyhow::Result<()> {
    let paths = resolve_paths(app_dir)?;
    let lib = onset_rekordbox::library::Library::open(&paths, &cache_dir()?)?;
    let track = lib
        .find_by_title_artist(title, "")
        .ok_or_else(|| anyhow::anyhow!("no track titled {title:?}"))?;
    let rel = track
        .analysis_path
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("{title:?} has no analysis file"))?;
    let a = onset_rekordbox::anlz::load_analysis(&paths, rel)?;

    println!("{} - {}", track.artist, track.title);
    println!(
        "beats: {}   bpm@60s: {:?}   first beat at {:?} ms",
        a.grid.len(),
        a.grid.bpm_at(60_000.0),
        a.grid.beats().first().map(|b| b.time_ms)
    );
    match &a.phrases {
        Some(pm) => {
            println!(
                "phrases ({:?} mood, ends at beat {}):",
                pm.mood, pm.end_beat
            );
            for p in &pm.phrases {
                let start_ms = a
                    .grid
                    .time_of(usize::try_from(p.start_beat.saturating_sub(1)).unwrap_or(0))
                    .unwrap_or(0.0);
                println!(
                    "  {:>5}-{:<5} {:>7.1}s  {:<10} {:?}",
                    p.start_beat,
                    p.end_beat,
                    start_ms / 1000.0,
                    p.label,
                    p.kind
                );
            }
        }
        None => println!("phrases: none (no PSSI section)"),
    }
    println!("cues:");
    for c in &a.cues {
        let slot = if c.slot == 0 {
            "mem".to_string()
        } else {
            char::from(b'A' + c.slot - 1).to_string()
        };
        println!(
            "  {:>7.1}s  {slot:<3} {}",
            f64::from(c.time_ms) / 1000.0,
            c.name
        );
    }
    Ok(())
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
        Command::Anlz { app_dir, title } => cmd_anlz(app_dir, &title)?,
    }
    Ok(())
}
