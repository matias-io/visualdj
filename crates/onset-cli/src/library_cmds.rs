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

/// Looks up (or reads from the cache) the lyrics of every track in the collection, or of
/// those whose title or artist contains `grep`, and prints a line per track and a summary.
pub fn lyrics(app_dir: Option<PathBuf>, grep: Option<String>, offline: bool) -> anyhow::Result<()> {
    use onset_lyrics::{LyricsStore, Outcome, Query};
    let (_, library) = crate::common::open_library(app_dir)?;
    let dir = crate::common::cache_dir()?.join("lyrics");
    let store = LyricsStore::new(&dir);
    let wanted = grep.map(|g| g.to_lowercase());
    let (mut synced, mut plain, mut instrumental, mut none) = (0, 0, 0, 0);
    for t in library.tracks() {
        let hay = format!("{} {}", t.artist, t.title).to_lowercase();
        if wanted.as_ref().is_some_and(|w| !hay.contains(w)) {
            continue;
        }
        let q = Query::from_meta(t);
        let was_cached = store.cached(&q).is_some();
        let out = store.lookup(&q, !offline);
        let tag = match &out {
            Outcome::Synced(l) => {
                synced += 1;
                format!("synced ({} lines)", l.lines.len())
            }
            Outcome::Plain => {
                plain += 1;
                "unsynced only".to_string()
            }
            Outcome::Instrumental => {
                instrumental += 1;
                "instrumental".to_string()
            }
            Outcome::NotFound | Outcome::Offline => {
                none += 1;
                "none".to_string()
            }
        };
        println!("{tag:<20} {} - {}", t.artist, t.title);
        if !was_cached && !offline {
            // Be polite to the free services.
            std::thread::sleep(std::time::Duration::from_millis(300));
        }
    }
    println!(
        "\n{synced} synced, {plain} unsynced only, {instrumental} instrumental, {none} without lyrics. Cache: {}",
        dir.display()
    );
    Ok(())
}
