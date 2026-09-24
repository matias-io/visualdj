//! Shared helpers for integration tests that read Reima's real rekordbox data.
use std::path::PathBuf;

/// Root of the gitignored copy of a real rekordbox data folder, if present.
///
/// Looks at `ONSET_FIXTURES_DIR` first (so a git worktree can point at the main checkout's
/// copy), then at `<repo>/fixtures/private`.
pub fn private_fixture_dir() -> Option<PathBuf> {
    let candidates = [
        std::env::var_os("ONSET_FIXTURES_DIR").map(PathBuf::from),
        Some(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/private")),
    ];
    candidates
        .into_iter()
        .flatten()
        .find(|root| root.join("master.db").exists())
}

/// Returns the private fixture dir, or returns from the test with a skip message.
#[macro_export]
macro_rules! require_private_fixtures {
    () => {
        match $crate::common::private_fixture_dir() {
            Some(dir) => dir,
            None => {
                eprintln!("skipping: fixtures/private not present");
                return;
            }
        }
    };
}
