mod common;

use onset_rekordbox::paths::RekordboxPaths;

#[test]
fn from_app_dir_builds_expected_paths() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("master.db"), b"x").unwrap();
    std::fs::create_dir_all(tmp.path().join("share/PIONEER/USBANLZ")).unwrap();

    let p = RekordboxPaths::from_app_dir(tmp.path()).unwrap();

    assert_eq!(p.master_db, tmp.path().join("master.db"));
    assert_eq!(p.master_wal, tmp.path().join("master.db-wal"));
    assert_eq!(p.share_dir, tmp.path().join("share"));
    assert_eq!(
        p.resolve_share("/PIONEER/USBANLZ/abc/ANLZ0000.DAT"),
        tmp.path()
            .join("share")
            .join("PIONEER")
            .join("USBANLZ")
            .join("abc")
            .join("ANLZ0000.DAT")
    );
}

#[test]
fn from_app_dir_rejects_missing_db() {
    let tmp = tempfile::tempdir().unwrap();
    assert!(RekordboxPaths::from_app_dir(tmp.path()).is_err());
}

#[test]
fn private_fixture_dir_is_valid_app_dir() {
    let dir = require_private_fixtures!();
    let p = RekordboxPaths::from_app_dir(&dir).unwrap();
    assert!(p.share_dir.join("PIONEER/USBANLZ").is_dir());
}
