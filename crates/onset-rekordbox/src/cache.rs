//! Keeps a decrypted copy of `master.db` up to date in a cache folder.
use std::path::{Path, PathBuf};

use crate::paths::RekordboxPaths;
use crate::sqlcipher;

/// Decrypts `master.db` (and its WAL) into `<cache_dir>/master.plain.db` when the source
/// changed since the last run, and returns the plaintext path.
pub fn plaintext_db(paths: &RekordboxPaths, cache_dir: &Path) -> anyhow::Result<PathBuf> {
    std::fs::create_dir_all(cache_dir)?;
    let out = cache_dir.join("master.plain.db");
    let stamp_path = cache_dir.join("master.plain.stamp");

    let stamp = source_stamp(paths);
    if out.is_file() && std::fs::read_to_string(&stamp_path).ok().as_deref() == Some(&stamp) {
        return Ok(out);
    }

    let wal = paths
        .master_wal
        .is_file()
        .then_some(paths.master_wal.as_path());
    let started = std::time::Instant::now();
    let stats = sqlcipher::decrypt_database(&paths.master_db, wal, sqlcipher::REKORDBOX_KEY, &out)?;
    tracing::info!(
        pages = stats.pages,
        wal_frames = stats.wal_frames_applied,
        ms = started.elapsed().as_millis(),
        "decrypted master.db"
    );
    std::fs::write(&stamp_path, stamp)?;
    Ok(out)
}

/// Modification times and sizes of the encrypted database and its WAL, as one string.
fn source_stamp(paths: &RekordboxPaths) -> String {
    let describe = |p: &Path| {
        std::fs::metadata(p).map_or_else(
            |_| "absent".to_string(),
            |m| format!("{:?}:{}", m.modified().ok(), m.len()),
        )
    };
    format!(
        "{}|{}",
        describe(&paths.master_db),
        describe(&paths.master_wal)
    )
}
