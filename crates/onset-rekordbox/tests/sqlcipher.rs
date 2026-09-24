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
