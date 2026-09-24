mod common;

use onset_core::phrase::PhraseKind;
use onset_rekordbox::anlz::{AnlzError, load_analysis};
use onset_rekordbox::library::Library;
use onset_rekordbox::paths::RekordboxPaths;

struct Opened {
    paths: RekordboxPaths,
    lib: Library,
    _cache: tempfile::TempDir,
}

fn open() -> Option<Opened> {
    let dir = common::private_fixture_dir()?;
    let paths = RekordboxPaths::from_app_dir(&dir).unwrap();
    let cache = tempfile::tempdir().unwrap();
    let lib = Library::open(&paths, cache.path()).unwrap();
    Some(Opened {
        paths,
        lib,
        _cache: cache,
    })
}

#[test]
fn move_has_grid_phrases_and_cues() {
    let Some(o) = open() else {
        eprintln!("skipping: fixtures/private not present");
        return;
    };
    let mv = o.lib.find_by_title_artist("Move", "Adam Port").unwrap();
    let a = load_analysis(&o.paths, mv.analysis_path.as_deref().unwrap()).unwrap();

    assert!(a.grid.len() > 300, "beats: {}", a.grid.len());
    let bpm = a.grid.bpm_at(60_000.0).unwrap();
    assert!((bpm - 120.0).abs() < 0.05, "bpm {bpm}");

    let ph = a.phrases.expect("Move has phrase analysis");
    assert!(ph.phrases.len() >= 5, "{}", ph.phrases.len());
    assert_eq!(ph.phrases[0].kind, PhraseKind::Intro);
    assert!(ph.phrases.iter().any(|p| p.kind == PhraseKind::Chorus));
    // Phrases tile the analysed range without gaps or overlaps.
    assert!(
        ph.phrases
            .windows(2)
            .all(|w| w[0].end_beat == w[1].start_beat),
        "{:?}",
        ph.phrases
    );
    // rekordbox 7 keeps cues in master.db; local analysis files usually carry none.
    assert!(a.cues.windows(2).all(|w| w[0].time_ms <= w[1].time_ms));
}

#[test]
fn every_analysed_track_parses() {
    let Some(o) = open() else { return };
    let (mut ok, mut no_phrases, mut no_grid, mut failed) = (0, 0, 0, Vec::new());
    for t in o.lib.tracks().iter().filter(|t| t.analysis_path.is_some()) {
        match load_analysis(&o.paths, t.analysis_path.as_deref().unwrap()) {
            Ok(a) => {
                ok += 1;
                if a.phrases.is_none() {
                    no_phrases += 1;
                }
            }
            // Tracks rekordbox never beat-analysed have an empty PQTZ; that is data, not a bug.
            Err(AnlzError::NoBeatGrid(_)) => no_grid += 1,
            Err(e) => failed.push(format!("{}: {e}", t.title)),
        }
    }
    eprintln!(
        "parsed {ok}, without phrases {no_phrases}, never beat-analysed {no_grid}, failed {}",
        failed.len()
    );
    assert!(failed.is_empty(), "{failed:#?}");
    assert!(ok > 150, "parsed only {ok}");
}
