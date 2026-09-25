//! rekordbox analysis files: `ANLZ0000.DAT` (beat grid, cue list) and `ANLZ0000.EXT`
//! (song structure, extended cues, colour waveforms). Parsing is done by `rekordcrate`; this
//! module maps its structures onto Onset's domain types.
use std::io::Cursor;
use std::path::{Path, PathBuf};

use binrw::BinRead;
use onset_core::bands::TrackBands;
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
    /// rekordbox's 3-band waveform and vocal detection from `ANLZ0000.2EX`, when present.
    pub bands: Option<TrackBands>,
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

/// Big-endian u32 at `at`, if the bytes are there.
fn be_u32(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_be_bytes(b.get(at..at + 4)?.try_into().ok()?))
}

/// Reads the 3-band waveform (`PWV7`) and vocal detection (`PVDI`) sections of an
/// `ANLZ0000.2EX` file. `rekordcrate` does not know these tags, so the sections are walked by
/// hand: each starts with a 4-byte tag, a header length and a total length (big-endian).
pub fn parse_2ex(bytes: &[u8]) -> Option<TrackBands> {
    if bytes.get(0..4)? != b"PMAI" {
        return None;
    }
    let mut at = be_u32(bytes, 4)? as usize;
    let mut frames: Vec<[u8; 3]> = Vec::new();
    let mut rate_hz = 150.0;
    let mut vocal: Vec<u8> = Vec::new();
    let mut vocal_rate_hz = 0.0;
    while at + 12 <= bytes.len() {
        let tag = &bytes[at..at + 4];
        let header = be_u32(bytes, at + 4)? as usize;
        let total = be_u32(bytes, at + 8)? as usize;
        if total < header || header < 12 || at + total > bytes.len() {
            break;
        }
        let head = &bytes[at + 12..at + header];
        let body = &bytes[at + header..at + total];
        match tag {
            b"PWV7" => {
                let entry = be_u32(head, 0).unwrap_or(0) as usize;
                let count = be_u32(head, 4).unwrap_or(0) as usize;
                if let Some(r) = head.get(8..10) {
                    let r = u16::from_be_bytes([r[0], r[1]]);
                    if r > 0 {
                        rate_hz = f32::from(r);
                    }
                }
                if entry == 3 {
                    frames = body
                        .as_chunks::<3>()
                        .0
                        .iter()
                        .take(count)
                        .copied()
                        .collect();
                }
            }
            b"PVDI" => {
                // Header: hop in samples (u32), sample rate (u16), unknown (u16), count (u32).
                let hop = be_u32(head, 0).unwrap_or(0);
                let sr = head.get(4..6).map_or(0, |r| u16::from_be_bytes([r[0], r[1]]));
                let count = be_u32(head, 8).unwrap_or(0) as usize;
                if hop > 0 && sr > 0 {
                    vocal_rate_hz = f32::from(sr) / hop as f32;
                    vocal = body.iter().take(count).copied().collect();
                }
            }
            _ => {}
        }
        at += total;
    }
    (!frames.is_empty() || !vocal.is_empty())
        .then(|| TrackBands::new(rate_hz, frames, vocal_rate_hz, vocal))
}

/// Loads the `.DAT` file at the rekordbox-relative path and its sibling `.EXT`.
pub fn load_analysis(paths: &RekordboxPaths, dat_rel: &str) -> Result<Analysis, AnlzError> {
    let dat_path = paths.resolve_share(dat_rel);
    let ext_path = dat_path.with_extension("EXT");
    let dat = read_anlz(&dat_path)?.ok_or_else(|| AnlzError::Missing(dat_path.clone()))?;
    let ext = read_anlz(&ext_path)?;
    let bands = std::fs::read(dat_path.with_extension("2EX"))
        .ok()
        .and_then(|b| parse_2ex(&b));

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
        bands,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn section(tag: [u8; 4], head: &[u8], body: &[u8]) -> Vec<u8> {
        let header = 12 + head.len();
        let total = header + body.len();
        let mut v = tag.to_vec();
        v.extend_from_slice(&u32::try_from(header).unwrap().to_be_bytes());
        v.extend_from_slice(&u32::try_from(total).unwrap().to_be_bytes());
        v.extend_from_slice(head);
        v.extend_from_slice(body);
        v
    }

    #[test]
    fn parses_the_three_band_waveform_and_vocal_detection() {
        let mut file = b"PMAI".to_vec();
        file.extend_from_slice(&28u32.to_be_bytes());
        file.extend_from_slice(&0u32.to_be_bytes());
        file.resize(28, 0);
        let mut pwv7_head = 3u32.to_be_bytes().to_vec();
        pwv7_head.extend_from_slice(&2u32.to_be_bytes());
        pwv7_head.extend_from_slice(&[0, 150, 0, 0]);
        file.extend(section(*b"PPTH", &[0, 0, 0, 0], b"xx"));
        file.extend(section(*b"PWV7", &pwv7_head, &[9, 5, 1, 3, 2, 8]));
        let mut pvdi_head = 1024u32.to_be_bytes().to_vec();
        pvdi_head.extend_from_slice(&[0x56, 0x22, 0, 1]);
        pvdi_head.extend_from_slice(&3u32.to_be_bytes());
        file.extend(section(*b"PVDI", &pvdi_head, &[0, 2, 4]));
        let b = parse_2ex(&file).expect("parsed");
        assert_eq!(b.frames, vec![[9, 5, 1], [3, 2, 8]]);
        assert!((b.rate_hz - 150.0).abs() < 1e-6);
        assert_eq!(b.vocal, vec![0, 2, 4]);
        assert!((b.vocal_rate_hz - 22050.0 / 1024.0).abs() < 1e-3);
    }

    #[test]
    fn rejects_other_files() {
        assert!(parse_2ex(b"RIFF....").is_none());
        assert!(parse_2ex(&[]).is_none());
    }
}
