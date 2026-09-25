//! What a transport source reports about the master deck at one instant.
use std::time::Instant;

use crate::track::TrackId;

/// How a source identifies the loaded track. Sources differ: the simulator knows the id,
/// the memory reader sees an analysis path and a title/artist/album text block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TrackRef {
    Unknown,
    Id(TrackId),
    /// rekordbox-relative `.DAT` path, e.g. `/PIONEER/USBANLZ/…/ANLZ0000.DAT`.
    AnalysisPath(String),
    TitleArtist {
        title: String,
        artist: String,
        album: String,
    },
    /// The audio file's path as the library stores it (rekordbox keeps it open while the
    /// track is loaded).
    FilePath(std::path::PathBuf),
}

#[derive(Debug, Clone, PartialEq)]
pub struct TransportSnapshot {
    /// Deck index, 0-based.
    pub deck: u8,
    pub track: TrackRef,
    /// Playhead in seconds of track time.
    pub playhead_s: f64,
    /// Current (pitched) tempo.
    pub bpm_now: f32,
    /// The track's analysed tempo; `bpm_now / bpm_original` is the playback rate.
    pub bpm_original: f32,
    pub playing: bool,
    /// When the source read this state (monotonic).
    pub read_at: Instant,
}
