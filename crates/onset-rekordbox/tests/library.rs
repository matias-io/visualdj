mod common;

use onset_rekordbox::library::Library;
use onset_rekordbox::paths::RekordboxPaths;

/// The library keeps reopening its plaintext database, so the cache directory must outlive it.
struct Opened {
    lib: Library,
    _cache: tempfile::TempDir,
}

impl std::ops::Deref for Opened {
    type Target = Library;
    fn deref(&self) -> &Library {
        &self.lib
    }
}

fn open() -> Option<Opened> {
    let dir = common::private_fixture_dir()?;
    let paths = RekordboxPaths::from_app_dir(&dir).unwrap();
    let cache = tempfile::tempdir().unwrap();
    let lib = Library::open(&paths, cache.path()).unwrap();
    Some(Opened { lib, _cache: cache })
}

#[test]
fn loads_reimas_collection() {
    let Some(lib) = open() else {
        eprintln!("skipping: fixtures/private not present");
        return;
    };
    assert!(lib.tracks().len() >= 150, "got {}", lib.tracks().len());

    let mv = lib
        .find_by_title_artist("Move", "Adam Port")
        .expect("Move by Adam Port");
    assert_eq!(mv.bpm, Some(120.0));
    assert_eq!(mv.key.as_deref(), Some("1A"));
    assert!(mv.artist.contains("Keinemusik"), "{}", mv.artist);
    assert!(
        mv.analysis_path
            .as_deref()
            .unwrap_or("")
            .contains("USBANLZ"),
        "{:?}",
        mv.analysis_path
    );
    assert!(
        mv.artwork_path.as_ref().is_some_and(|p| p.exists()),
        "artwork should resolve on disk: {:?}",
        mv.artwork_path
    );
    assert!(
        mv.file_path
            .as_ref()
            .is_some_and(|p| p.to_string_lossy().ends_with("Adam Port - Move.flac")),
        "{:?}",
        mv.file_path
    );
    assert!(
        mv.duration_s.is_some_and(|d| (d - 177.0).abs() < 3.0),
        "{:?}",
        mv.duration_s
    );
}

#[test]
fn hot_cues_for_move_include_named_drop() {
    let Some(lib) = open() else { return };
    let mv = lib.find_by_title_artist("Move", "Adam Port").unwrap();
    let cues = lib.cues(mv.id);
    assert!(
        cues.iter()
            .any(|c| c.slot > 0 && c.name.eq_ignore_ascii_case("DROP")),
        "{cues:?}"
    );
    assert!(
        cues.windows(2).all(|w| w[0].time_ms <= w[1].time_ms),
        "sorted by time"
    );
}

#[test]
fn analysis_path_lookup_round_trips() {
    let Some(lib) = open() else { return };
    let t = lib
        .tracks()
        .iter()
        .find(|t| t.analysis_path.is_some())
        .unwrap();
    let rel = t.analysis_path.clone().unwrap();
    assert_eq!(lib.by_analysis_path(&rel).map(|x| x.id), Some(t.id));
    // Case and separator differences from a memory read must still resolve.
    let variant = rel.to_uppercase().replace('/', "\\");
    assert_eq!(lib.by_analysis_path(&variant).map(|x| x.id), Some(t.id));
}

#[test]
fn deleted_rows_are_excluded_and_ids_unique() {
    let Some(lib) = open() else { return };
    let mut ids: Vec<u64> = lib.tracks().iter().map(|t| t.id.0).collect();
    let n = ids.len();
    ids.sort_unstable();
    ids.dedup();
    assert_eq!(ids.len(), n, "duplicate track ids");
    assert!(ids.iter().all(|&id| id > 0), "unparsed id");
}
