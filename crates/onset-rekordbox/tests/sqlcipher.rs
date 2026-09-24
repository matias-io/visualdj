mod common;
use onset_rekordbox::sqlcipher::{REKORDBOX_KEY, decrypt_database};

#[test]
fn decrypts_real_database_and_passes_integrity_check() {
    let dir = require_private_fixtures!();
    let out = tempfile::NamedTempFile::new().unwrap().into_temp_path();
    let stats = decrypt_database(&dir.join("master.db"), None, REKORDBOX_KEY, &out).unwrap();
    assert!(stats.pages > 100);
    let conn =
        rusqlite::Connection::open_with_flags(&out, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .unwrap();
    let ok: String = conn
        .query_row("PRAGMA integrity_check", [], |r| r.get(0))
        .unwrap();
    assert_eq!(ok, "ok");
    let n: i64 = conn
        .query_row("SELECT COUNT(*) FROM djmdContent", [], |r| r.get(0))
        .unwrap();
    assert!(n > 100, "expected Reima's collection, got {n} rows");
}

#[test]
fn decrypts_and_applies_wal() {
    let dir = require_private_fixtures!();
    let without = tempfile::NamedTempFile::new().unwrap().into_temp_path();
    let with = tempfile::NamedTempFile::new().unwrap().into_temp_path();
    decrypt_database(&dir.join("master.db"), None, REKORDBOX_KEY, &without).unwrap();
    let stats = decrypt_database(
        &dir.join("master.db"),
        Some(&dir.join("master.db-wal")),
        REKORDBOX_KEY,
        &with,
    )
    .unwrap();
    assert!(
        stats.wal_frames_applied > 0,
        "fixture WAL is 4 MB; frames must apply"
    );
    let open = |p: &std::path::Path| {
        rusqlite::Connection::open_with_flags(p, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .unwrap()
    };
    let ok: String = open(&with)
        .query_row("PRAGMA integrity_check", [], |r| r.get(0))
        .unwrap();
    assert_eq!(ok, "ok");
    // The WAL-applied copy must be at least as current as the bare file.
    let count = |c: &rusqlite::Connection| -> i64 {
        c.query_row(
            "SELECT COUNT(*) FROM djmdContent WHERE rb_local_deleted = 0",
            [],
            |r| r.get(0),
        )
        .unwrap()
    };
    assert!(count(&open(&with)) >= count(&open(&without)));
}

/// rekordbox writes a WAL frame header and its page in two writes; reading in between yields
/// a frame with valid salts and a failing HMAC. SQLite treats that as end-of-log; so must we.
#[test]
fn torn_trailing_wal_frame_is_treated_as_end_of_log() {
    let dir = require_private_fixtures!();
    let wal = std::fs::read(dir.join("master.db-wal")).unwrap();
    let frame_len = 24 + 4096;
    let frames = (wal.len() - 32) / frame_len;
    assert!(frames > 10);
    // Corrupt one byte inside the page data of the last frame.
    let mut torn = wal.clone();
    let last_page_start = 32 + (frames - 1) * frame_len + 24;
    torn[last_page_start + 100] ^= 0xFF;
    let torn_path = tempfile::NamedTempFile::new().unwrap().into_temp_path();
    std::fs::write(&torn_path, &torn).unwrap();

    let out = tempfile::NamedTempFile::new().unwrap().into_temp_path();
    let stats = decrypt_database(
        &dir.join("master.db"),
        Some(&torn_path),
        REKORDBOX_KEY,
        &out,
    )
    .expect("a torn frame must not fail the whole decryption");
    let ok: String =
        rusqlite::Connection::open_with_flags(&out, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .unwrap()
            .query_row("PRAGMA integrity_check", [], |r| r.get(0))
            .unwrap();
    assert_eq!(ok, "ok");
    assert!(
        stats.wal_frames_applied > 0 && stats.wal_frames_applied < u32::try_from(frames).unwrap(),
        "{stats:?}"
    );
}

/// The fixture WAL holds cue rows that the main file does not; applying the WAL must surface them.
#[test]
fn wal_only_rows_become_visible() {
    let dir = require_private_fixtures!();
    let without = tempfile::NamedTempFile::new().unwrap().into_temp_path();
    let with = tempfile::NamedTempFile::new().unwrap().into_temp_path();
    decrypt_database(&dir.join("master.db"), None, REKORDBOX_KEY, &without).unwrap();
    decrypt_database(
        &dir.join("master.db"),
        Some(&dir.join("master.db-wal")),
        REKORDBOX_KEY,
        &with,
    )
    .unwrap();
    let ids = |p: &std::path::Path| -> std::collections::BTreeSet<String> {
        let c =
            rusqlite::Connection::open_with_flags(p, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
                .unwrap();
        let mut s = c.prepare("SELECT ID FROM djmdCue").unwrap();
        s.query_map([], |r| r.get::<_, String>(0))
            .unwrap()
            .flatten()
            .collect()
    };
    let (a, b) = (ids(&without), ids(&with));
    assert!(
        b.difference(&a).count() > 0,
        "expected cue rows only present in the WAL"
    );
}

#[test]
fn wrong_key_is_rejected() {
    let dir = require_private_fixtures!();
    let out = tempfile::NamedTempFile::new().unwrap().into_temp_path();
    let err = decrypt_database(&dir.join("master.db"), None, "not-the-key", &out).unwrap_err();
    assert!(matches!(
        err,
        onset_rekordbox::sqlcipher::SqlcipherError::HmacMismatch { page: 1 }
    ));
}
