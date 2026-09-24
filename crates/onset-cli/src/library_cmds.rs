//! `schema`, `library` and `anlz`: read-only views of the rekordbox data layer.
use std::path::PathBuf;

use crate::common::{cache_dir, open_library, resolve_paths};

pub fn schema(app_dir: Option<PathBuf>) -> anyhow::Result<()> {
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

pub fn library(app_dir: Option<PathBuf>, grep: Option<String>) -> anyhow::Result<()> {
    let (_paths, lib) = open_library(app_dir)?;
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

pub fn anlz(app_dir: Option<PathBuf>, title: &str) -> anyhow::Result<()> {
    let (paths, lib) = open_library(app_dir)?;
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
    println!("cues (from master.db):");
    for c in lib.cues(track.id) {
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
