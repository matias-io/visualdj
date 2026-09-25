//! The collection as loaded from the plaintext copy of `master.db`.
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use onset_core::track::{HotCue, TrackId, TrackMeta};
use rusqlite::{Connection, OpenFlags};

use crate::cache;
use crate::paths::RekordboxPaths;

pub struct Library {
    tracks: Vec<TrackMeta>,
    by_id: HashMap<TrackId, usize>,
    by_analysis: HashMap<String, usize>,
    db_path: PathBuf,
}

/// Case-insensitive, separator-insensitive key for paths and titles.
fn norm(s: &str) -> String {
    s.trim().replace('\\', "/").to_lowercase()
}

/// `djmdCue.Kind` as observed in rekordbox 7.2.18: 0 = memory cue, 1..=3 = hot cues A..C,
/// 4 = the active loop, 5..=9 = hot cues D..H. Anything else is treated as a memory cue.
fn hot_cue_slot(kind: i64) -> u8 {
    match kind {
        1..=3 => u8::try_from(kind).unwrap_or(0),
        5..=9 => u8::try_from(kind - 1).unwrap_or(0),
        _ => 0,
    }
}

fn open_read_only(path: &Path) -> rusqlite::Result<Connection> {
    Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
}

impl Library {
    /// Decrypts (or reuses) the plaintext database in `cache_dir` and loads the collection.
    pub fn open(paths: &RekordboxPaths, cache_dir: &Path) -> anyhow::Result<Self> {
        let db_path = cache::plaintext_db(paths, cache_dir)?;
        let conn = open_read_only(&db_path)?;
        let mut stmt = conn.prepare(
            "SELECT c.ID, c.Title, IFNULL(a.Name, ''), IFNULL(al.Name, ''), c.ReleaseYear,
                    k.ScaleName, c.BPM, c.Length, c.FolderPath, c.ImagePath,
                    c.AnalysisDataPath, c.ISRC, c.SampleRate, g.Name
             FROM djmdContent c
             LEFT JOIN djmdGenre g ON g.ID = c.GenreID
             LEFT JOIN djmdArtist a ON a.ID = c.ArtistID
             LEFT JOIN djmdAlbum al ON al.ID = c.AlbumID
             LEFT JOIN djmdKey k ON k.ID = c.KeyID
             WHERE c.rb_local_deleted = 0",
        )?;
        let rows = stmt.query_map([], |r| {
            let id: String = r.get(0)?;
            let year: Option<i64> = r.get(4)?;
            let bpm: Option<i64> = r.get(6)?;
            let len: Option<i64> = r.get(7)?;
            let folder: Option<String> = r.get(8)?;
            let image: Option<String> = r.get(9)?;
            let sample_rate: Option<i64> = r.get(12)?;
            Ok(TrackMeta {
                id: TrackId(id.parse().unwrap_or(0)),
                title: r.get::<_, Option<String>>(1)?.unwrap_or_default(),
                artist: r.get(2)?,
                album: r.get(3)?,
                year: year.filter(|y| *y > 0).and_then(|y| u16::try_from(y).ok()),
                key: r.get::<_, Option<String>>(5)?.filter(|k| !k.is_empty()),
                bpm: bpm.filter(|b| *b > 0).map(|b| b as f32 / 100.0),
                duration_s: len.filter(|l| *l > 0).map(|l| l as f32),
                sample_rate: sample_rate
                    .filter(|s| *s > 0)
                    .and_then(|s| u32::try_from(s).ok()),
                file_path: folder.filter(|f| !f.is_empty()).map(PathBuf::from),
                artwork_path: image
                    .filter(|i| !i.is_empty())
                    .map(|i| paths.resolve_share(&i)),
                analysis_path: r.get::<_, Option<String>>(10)?.filter(|p| !p.is_empty()),
                isrc: r.get::<_, Option<String>>(11)?.filter(|s| !s.is_empty()),
                genre: r
                    .get::<_, Option<String>>(13)?
                    .filter(|s| !s.trim().is_empty()),
            })
        })?;
        let tracks: Vec<TrackMeta> = rows.collect::<Result<_, _>>()?;

        let by_id = tracks.iter().enumerate().map(|(i, t)| (t.id, i)).collect();
        let by_analysis = tracks
            .iter()
            .enumerate()
            .filter_map(|(i, t)| t.analysis_path.as_deref().map(|p| (norm(p), i)))
            .collect();
        tracing::info!(tracks = tracks.len(), "loaded rekordbox collection");
        Ok(Self {
            tracks,
            by_id,
            by_analysis,
            db_path,
        })
    }

    pub fn tracks(&self) -> &[TrackMeta] {
        &self.tracks
    }

    pub fn by_id(&self, id: TrackId) -> Option<&TrackMeta> {
        self.by_id.get(&id).map(|&i| &self.tracks[i])
    }

    /// Looks up a track by its rekordbox-relative `.DAT` path, tolerant of case and separators.
    pub fn by_analysis_path(&self, rel: &str) -> Option<&TrackMeta> {
        self.by_analysis.get(&norm(rel)).map(|&i| &self.tracks[i])
    }

    /// The track whose file is `path`, tolerant of case and separators.
    pub fn by_file_path(&self, path: &std::path::Path) -> Option<&TrackMeta> {
        let wanted = norm(&path.to_string_lossy());
        self.tracks.iter().find(|t| {
            t.file_path
                .as_ref()
                .is_some_and(|p| norm(&p.to_string_lossy()) == wanted)
        })
    }

    /// Case-insensitive title match; `artist` may be a prefix of the stored artist string
    /// (rekordbox concatenates collaborators, and memory reads may truncate).
    pub fn find_by_title_artist(&self, title: &str, artist: &str) -> Option<&TrackMeta> {
        let (t, a) = (norm(title), norm(artist));
        self.tracks
            .iter()
            .find(|x| norm(&x.title) == t && (a.is_empty() || norm(&x.artist).starts_with(&a)))
    }

    /// Memory cues and hot cues of a track, sorted by time.
    pub fn cues(&self, id: TrackId) -> Vec<HotCue> {
        let Ok(conn) = open_read_only(&self.db_path) else {
            return Vec::new();
        };
        let Ok(mut stmt) = conn.prepare(
            "SELECT Kind, InMsec, IFNULL(Comment, '') FROM djmdCue
             WHERE ContentID = ?1 AND rb_local_deleted = 0 AND InMsec >= 0
             ORDER BY InMsec",
        ) else {
            return Vec::new();
        };
        stmt.query_map([id.0.to_string()], |r| {
            let kind: i64 = r.get(0)?;
            let ms: i64 = r.get(1)?;
            Ok(HotCue {
                slot: hot_cue_slot(kind),
                time_ms: u32::try_from(ms.max(0)).unwrap_or(0),
                name: r.get(2)?,
                color: None,
            })
        })
        .map(|rows| rows.flatten().collect())
        .unwrap_or_default()
    }
}
