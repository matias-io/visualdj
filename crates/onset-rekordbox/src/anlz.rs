//! rekordbox analysis files: `ANLZ0000.DAT` (beat grid, cue list) and `ANLZ0000.EXT`
//! (song structure, extended cues, colour waveforms). Parsing is done by `rekordcrate`; this
//! module maps its structures onto Onset's domain types.
use std::io::Cursor;
use std::path::{Path, PathBuf};

use binrw::BinRead;
use onset_core::grid::{Beat, BeatGrid};
use onset_core::phrase::{Mood, Phrase, PhraseKind, PhraseMap};
use onset_core::track::HotCue;
use rekordcrate::anlz::{self, ANLZ, Content};

use crate::paths::RekordboxPaths;

pub struct Analysis {
    pub grid: BeatGrid,
    /// `None` when rekordbox did not run phrase analysis for the track.
    pub phrases: Option<PhraseMap>,
    /// Cues stored in the analysis files, sorted by time. rekordbox 7 keeps cues in
    /// `master.db` and writes them here only on USB export, so this is often empty locally;
    /// prefer `Library::cues`.
    pub cues: Vec<HotCue>,
}

#[derive(Debug, thiserror::Error)]
pub enum AnlzError {
    #[error("missing analysis file {0}")]
    Missing(PathBuf),
    /// The file exists but rekordbox never beat-analysed the track (empty `PQTZ`).
    #[error("no beat grid in {0}")]
    NoBeatGrid(PathBuf),
    #[error("cannot parse {path}: {reason}")]
    Parse { path: PathBuf, reason: String },
    #[error("i/o error reading {path}: {source}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
}

fn read_anlz(path: &Path) -> Result<Option<ANLZ>, AnlzError> {
    if !path.is_file() {
        return Ok(None);
    }
    let bytes = std::fs::read(path).map_err(|source| AnlzError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    let anlz = ANLZ::read(&mut Cursor::new(bytes)).map_err(|e| AnlzError::Parse {
        path: path.to_path_buf(),
        reason: e.to_string(),
    })?;
    Ok(Some(anlz))
}

/// rekordbox's phrase vocabulary, per mood. The `k1..k3` flags select the numbered variant
/// in High mood (same scheme rekordbox's own UI uses).
fn label_and_kind(mood: Mood, kind: u16, k1: u8, k2: u8, k3: u8) -> (String, PhraseKind) {
    use PhraseKind as K;
    match mood {
        Mood::High => {
            let idx = usize::from(k1 + 2 * k2 + k3);
            let (kind, labels): (K, &[&str]) = match kind {
                1 => (K::Intro, &["Intro 2", "Intro 1"]),
                2 => (K::Up, &["Up 1", "Up 2", "Up 3"]),
                3 => (K::Down, &["Down"]),
                5 => (K::Chorus, &["Chorus 2", "Chorus 1"]),
                6 => (K::Outro, &["Outro 2", "Outro 1"]),
                _ => (K::Verse, &["Verse"]),
            };
            let label = labels.get(idx).or(labels.first()).map_or("?", |s| *s);
            (label.to_string(), kind)
        }
        Mood::Low | Mood::Mid => {
            let labels: [&str; 10] = if mood == Mood::Low {
                [
                    "Intro", "Verse 1", "Verse 1", "Verse 1", "Verse 2", "Verse 2", "Verse 2",
                    "Bridge", "Chorus", "Outro",
                ]
            } else {
                [
                    "Intro", "Verse 1", "Verse 2", "Verse 3", "Verse 4", "Verse 5", "Verse 6",
                    "Bridge", "Chorus", "Outro",
                ]
            };
            let kinds = [
                K::Intro,
                K::Verse,
                K::Verse,
                K::Verse,
                K::Verse,
                K::Verse,
                K::Verse,
                K::Bridge,
                K::Chorus,
                K::Outro,
            ];
            let i = usize::from(kind.saturating_sub(1)).min(9);
            (labels[i].to_string(), kinds[i])
        }
    }
}

/// `hot_cue` is 0 for memory cues and 1..=8 for hot cues A..H.
fn hot_cue_slot(hot_cue: u32) -> u8 {
    u8::try_from(hot_cue.min(8)).unwrap_or(0)
}

/// Loads the `.DAT` file at the rekordbox-relative path and its sibling `.EXT`.
pub fn load_analysis(paths: &RekordboxPaths, dat_rel: &str) -> Result<Analysis, AnlzError> {
    let dat_path = paths.resolve_share(dat_rel);
    let ext_path = dat_path.with_extension("EXT");
    let dat = read_anlz(&dat_path)?.ok_or_else(|| AnlzError::Missing(dat_path.clone()))?;
    let ext = read_anlz(&ext_path)?;

    let mut beats: Vec<Beat> = Vec::new();
    let mut cues: Vec<HotCue> = Vec::new();
    let mut extended_cues_seen = false;
    let mut phrases = None;

    let sections = dat
        .sections
        .iter()
        .chain(ext.iter().flat_map(|e| e.sections.iter()));
    for section in sections {
        match &section.content {
            Content::BeatGrid(g) if beats.is_empty() => {
                beats = g
                    .beats
                    .iter()
                    .map(|b| Beat {
                        time_ms: b.time,
                        beat_in_bar: u8::try_from(b.beat_number).unwrap_or(1),
                        bpm: f32::from(b.tempo) / 100.0,
                    })
                    .collect();
            }
            Content::ExtendedCueList(l) => {
                // PCO2 (in .EXT) carries names and colours; prefer it over the plain list.
                if !extended_cues_seen {
                    cues.clear();
                    extended_cues_seen = true;
                }
                for c in &l.cues {
                    let (r, g, b) = c.hot_cue_color_rgb;
                    cues.push(HotCue {
                        slot: hot_cue_slot(c.hot_cue),
                        time_ms: c.time,
                        name: c.comment.to_string(),
                        color: Some([r, g, b]),
                    });
                }
            }
            Content::CueList(l) if !extended_cues_seen => {
                for c in &l.cues {
                    cues.push(HotCue {
                        slot: hot_cue_slot(c.hot_cue),
                        time_ms: c.time,
                        name: String::new(),
                        color: None,
                    });
                }
            }
            Content::SongStructure(ss) => {
                let d = &ss.data;
                let mood = match d.mood {
                    anlz::Mood::Low => Mood::Low,
                    anlz::Mood::Mid => Mood::Mid,
                    anlz::Mood::High => Mood::High,
                };
                let mut out = Vec::with_capacity(d.phrases.len());
                for (i, p) in d.phrases.iter().enumerate() {
                    let end = d.phrases.get(i + 1).map_or(d.end_beat, |n| n.beat);
                    let (label, kind) = label_and_kind(mood, p.kind, p.k1, p.k2, p.k3);
                    out.push(Phrase {
                        kind,
                        label,
                        start_beat: u32::from(p.beat),
                        end_beat: u32::from(end),
                        fill_from_beat: (p.fill != 0).then(|| u32::from(p.beat_fill)),
                    });
                }
                phrases = Some(PhraseMap {
                    mood,
                    phrases: out,
                    end_beat: u32::from(d.end_beat),
                });
            }
            _ => {}
        }
    }

    if beats.is_empty() {
        return Err(AnlzError::NoBeatGrid(dat_path));
    }
    cues.sort_by_key(|c| c.time_ms);
    Ok(Analysis {
        grid: BeatGrid::new(beats),
        phrases,
        cues,
    })
}
