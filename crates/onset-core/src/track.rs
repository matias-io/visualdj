//! Track identity and metadata as rekordbox knows it.
use std::path::PathBuf;

/// rekordbox's content id (`djmdContent.ID`).
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
pub struct TrackId(pub u64);

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct TrackMeta {
    pub id: TrackId,
    pub title: String,
    /// All credited artists as rekordbox concatenates them.
    pub artist: String,
    pub album: String,
    pub year: Option<u16>,
    /// rekordbox key notation, e.g. `1A`.
    pub key: Option<String>,
    pub bpm: Option<f32>,
    pub duration_s: Option<f32>,
    /// The audio file's sample rate; rekordbox counts deck positions in these units.
    pub sample_rate: Option<u32>,
    pub file_path: Option<PathBuf>,
    pub artwork_path: Option<PathBuf>,
    /// rekordbox-relative path of the `.DAT` analysis file, e.g. `/PIONEER/USBANLZ/…/ANLZ0000.DAT`.
    pub analysis_path: Option<String>,
    pub isrc: Option<String>,
}

/// A memory cue or hot cue.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct HotCue {
    /// 0 = memory cue, 1..=8 = hot cue A..H.
    pub slot: u8,
    pub time_ms: u32,
    /// The DJ's label for the cue (often empty).
    pub name: String,
    pub color: Option<[u8; 3]>,
}
