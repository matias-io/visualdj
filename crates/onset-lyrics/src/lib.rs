//! Synced lyrics for the track that is playing. Looks each track up on LRCLIB (a free,
//! open lyrics database with line-synced LRC), first by exact title, artist, album and
//! length, then by search; when the rekordbox title does not match, the track's ISRC is
//! resolved on MusicBrainz to the canonical title and artist and LRCLIB is asked again.
//! Every answer, including "not found", is cached on disk so a track is looked up once.
//!
//! Only the title, artist, album, length and ISRC leave the machine.
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use onset_core::lyrics::Lyrics;
use onset_core::track::TrackMeta;
use serde::{Deserialize, Serialize};

const USER_AGENT: &str = concat!(
    "Onset/",
    env!("CARGO_PKG_VERSION"),
    " (https://github.com/matias-io/visualdj)"
);
const LRCLIB: &str = "https://lrclib.net/api";
const MUSICBRAINZ: &str = "https://musicbrainz.org/ws/2";
/// A track that was not found is asked about again after this long.
const RETRY_NOT_FOUND_S: u64 = 14 * 24 * 3600;
/// Search results further than this from the track's length are a different version.
const MAX_LENGTH_GAP_S: f32 = 12.0;

/// What the lyrics lookup is asked about.
#[derive(Debug, Clone, PartialEq)]
pub struct Query {
    pub title: String,
    pub artist: String,
    pub album: String,
    pub duration_s: Option<f32>,
    pub isrc: Option<String>,
}

impl Query {
    pub fn from_meta(t: &TrackMeta) -> Self {
        Self {
            title: t.title.clone(),
            artist: t.artist.clone(),
            album: t.album.clone(),
            duration_s: t.duration_s,
            isrc: t.isrc.clone(),
        }
    }

    /// The cache file name: artist, title and rounded length, filesystem-safe.
    fn key(&self) -> String {
        let raw = format!(
            "{} - {} - {}",
            main_artist(&self.artist),
            clean_title(&self.title),
            self.duration_s.map_or(0, |d| d.round() as i64)
        )
        .to_lowercase();
        raw.chars()
            .map(|c| if c.is_alphanumeric() || c == ' ' || c == '-' { c } else { '_' })
            .collect::<String>()
            .trim()
            .to_string()
    }
}

/// The answer for one track.
#[derive(Debug, Clone, PartialEq)]
pub enum Outcome {
    Synced(Lyrics),
    /// Lyrics exist but carry no timing, so they cannot follow the playhead.
    Plain,
    Instrumental,
    NotFound,
    /// Nothing cached and looking online is off.
    Offline,
}

impl Outcome {
    /// One line for the launcher.
    pub fn describe(&self) -> String {
        match self {
            Self::Synced(l) => format!("Synced lyrics from {} ({} lines)", l.source, l.lines.len()),
            Self::Plain => "Only unsynced lyrics exist for this track, so none are shown".into(),
            Self::Instrumental => "Instrumental: no lyrics".into(),
            Self::NotFound => "No lyrics found for this track".into(),
            Self::Offline => "No lyrics cached; turn on Find lyrics online to look them up".into(),
        }
    }
}

/// What is stored per track.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct Cached {
    /// "synced", "plain", "instrumental" or "none".
    status: String,
    #[serde(default)]
    lrc: String,
    #[serde(default)]
    source: String,
    fetched_unix: u64,
}

/// LRCLIB's record for a track.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct LrcRecord {
    #[serde(default)]
    track_name: String,
    #[serde(default)]
    artist_name: String,
    #[serde(default)]
    duration: Option<f32>,
    #[serde(default)]
    instrumental: bool,
    #[serde(default)]
    synced_lyrics: Option<String>,
    #[serde(default)]
    plain_lyrics: Option<String>,
}

/// The title without the mix name and featured artists, which lyric databases rarely carry:
/// "Latch (Extended Mix) [feat. Sam Smith]" -> "Latch".
pub fn clean_title(title: &str) -> String {
    let mut out = String::new();
    let mut depth = 0i32;
    for c in title.chars() {
        match c {
            '(' | '[' => depth += 1,
            ')' | ']' => depth = (depth - 1).max(0),
            _ if depth == 0 => out.push(c),
            _ => {}
        }
    }
    let lower = out.to_lowercase();
    let cut = [" feat.", " feat ", " ft.", " ft ", " featuring "]
        .iter()
        .filter_map(|m| lower.find(m))
        .min()
        .unwrap_or(out.len());
    let t = out[..cut].trim().trim_end_matches(['-', ' ']).trim();
    if t.is_empty() { title.trim().to_string() } else { t.to_string() }
}

/// The first credited artist: "Disclosure, Sam Smith" or "Fred again.. & Swedish House
/// Mafia" -> the first name.
pub fn main_artist(artist: &str) -> String {
    let lower = artist.to_lowercase();
    let cut = [",", " & ", " x ", " feat", " ft.", " vs ", " and "]
        .iter()
        .filter_map(|m| lower.find(m))
        .min()
        .unwrap_or(artist.len());
    artist[..cut].trim().to_string()
}

/// What a track is without asking anyone: a sample (no artist, or shorter than 30 s) has no
/// lyrics, and a title marked "Instrumental" is one.
fn known_without_lookup(q: &Query) -> Option<&'static str> {
    if q.title.to_lowercase().contains("instrumental") {
        return Some("instrumental");
    }
    if q.artist.trim().is_empty() || q.duration_s.is_some_and(|d| d < 30.0) {
        return Some("none");
    }
    None
}

/// The best search result: synced before plain, then the closest length, within reason.
fn best_match(results: &[LrcRecord], duration_s: Option<f32>) -> Option<&LrcRecord> {
    let gap = |r: &LrcRecord| match (r.duration, duration_s) {
        (Some(a), Some(b)) => (a - b).abs(),
        _ => MAX_LENGTH_GAP_S * 0.5,
    };
    results
        .iter()
        .filter(|r| gap(r) <= MAX_LENGTH_GAP_S)
        .filter(|r| r.instrumental || r.synced_lyrics.is_some() || r.plain_lyrics.is_some())
        .min_by(|a, b| {
            let rank = |r: &LrcRecord| u8::from(r.synced_lyrics.is_none());
            rank(a).cmp(&rank(b)).then(gap(a).total_cmp(&gap(b)))
        })
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

fn to_cached(r: &LrcRecord) -> Cached {
    let (status, lrc) = if r.instrumental {
        ("instrumental", String::new())
    } else if let Some(s) = r.synced_lyrics.as_ref().filter(|s| !s.trim().is_empty()) {
        ("synced", s.clone())
    } else if r.plain_lyrics.as_ref().is_some_and(|p| !p.trim().is_empty()) {
        ("plain", String::new())
    } else {
        ("none", String::new())
    };
    Cached {
        status: status.into(),
        lrc,
        source: "LRCLIB".into(),
        fetched_unix: now_unix(),
    }
}

fn outcome(c: &Cached) -> Outcome {
    match c.status.as_str() {
        "synced" => {
            let l = Lyrics::parse_lrc(&c.lrc, &c.source);
            if l.lines.is_empty() { Outcome::Plain } else { Outcome::Synced(l) }
        }
        "plain" => Outcome::Plain,
        "instrumental" => Outcome::Instrumental,
        _ => Outcome::NotFound,
    }
}

/// Looks lyrics up and keeps what it finds.
pub struct LyricsStore {
    dir: PathBuf,
    agent: ureq::Agent,
    /// When MusicBrainz was last asked; it allows one request a second.
    last_musicbrainz: std::sync::Mutex<Option<std::time::Instant>>,
}

/// Calls `f`, and once more after a pause if the service answers 503 (busy).
fn with_retry<T>(what: &str, mut f: impl FnMut() -> Result<T, ureq::Error>) -> anyhow::Result<T> {
    match f() {
        Err(ureq::Error::StatusCode(503)) => {
            std::thread::sleep(Duration::from_millis(1500));
            f().map_err(|e| anyhow::anyhow!("{what}: {e}"))
        }
        other => other.map_err(|e| anyhow::anyhow!("{what}: {e}")),
    }
}

impl LyricsStore {
    pub fn new(cache_dir: &Path) -> Self {
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(10)))
            .user_agent(USER_AGENT)
            .build()
            .into();
        Self {
            dir: cache_dir.to_path_buf(),
            agent,
            last_musicbrainz: std::sync::Mutex::new(None),
        }
    }

    fn path(&self, q: &Query) -> PathBuf {
        self.dir.join(format!("{}.json", q.key()))
    }

    fn read_cache(&self, q: &Query) -> Option<Cached> {
        let text = std::fs::read_to_string(self.path(q)).ok()?;
        serde_json::from_str(&text).ok()
    }

    fn write_cache(&self, q: &Query, c: &Cached) {
        let _ = std::fs::create_dir_all(&self.dir);
        match serde_json::to_string_pretty(c) {
            Ok(text) => {
                if let Err(e) = std::fs::write(self.path(q), text) {
                    tracing::warn!(error = %e, "cannot cache lyrics");
                }
            }
            Err(e) => tracing::warn!(error = %e, "cannot encode lyrics"),
        }
    }

    /// The cached answer, if any; never goes online.
    pub fn cached(&self, q: &Query) -> Option<Outcome> {
        self.read_cache(q).map(|c| outcome(&c))
    }

    /// The cached answer, or (when `online`) a fresh lookup that is then cached. A cached
    /// "not found" is retried after two weeks, in case someone has added the lyrics since.
    pub fn lookup(&self, q: &Query, online: bool) -> Outcome {
        if let Some(c) = self.read_cache(q) {
            let stale = c.status == "none" && now_unix().saturating_sub(c.fetched_unix) > RETRY_NOT_FOUND_S;
            if !stale || !online {
                return outcome(&c);
            }
        }
        if !online {
            return Outcome::Offline;
        }
        match self.fetch(q) {
            Ok(c) => {
                self.write_cache(q, &c);
                outcome(&c)
            }
            Err(e) => {
                // Network trouble is not an answer: nothing is cached, it is tried next time.
                tracing::warn!(title = %q.title, error = %e, "lyrics lookup failed");
                Outcome::NotFound
            }
        }
    }

    fn fetch(&self, q: &Query) -> anyhow::Result<Cached> {
        if let Some(status) = known_without_lookup(q) {
            return Ok(Cached {
                status: status.into(),
                lrc: String::new(),
                source: "Onset".into(),
                fetched_unix: now_unix(),
            });
        }
        let title = clean_title(&q.title);
        let artist = main_artist(&q.artist);
        // The title as rekordbox has it first (brackets can be part of the name), then
        // without the mix name and features.
        let mut titles = vec![q.title.trim().to_string()];
        if title != titles[0] {
            titles.push(title.clone());
        }
        for t in &titles {
            if let Some(r) = self.lrclib_get(t, &artist, &q.album, q.duration_s)? {
                return Ok(to_cached(&r));
            }
        }
        for t in &titles {
            if let Some(c) = self.lrclib_search(t, &artist, q.duration_s)? {
                return Ok(c);
            }
        }
        if let Some(isrc) = q.isrc.as_deref().filter(|s| !s.is_empty())
            && let Some((t, a)) = self.musicbrainz_isrc(isrc)?
            && (t != title || a != artist)
        {
            if let Some(r) = self.lrclib_get(&t, &a, "", q.duration_s)? {
                return Ok(to_cached(&r));
            }
            if let Some(c) = self.lrclib_search(&t, &a, q.duration_s)? {
                return Ok(c);
            }
        }
        Ok(Cached {
            status: "none".into(),
            lrc: String::new(),
            source: "LRCLIB".into(),
            fetched_unix: now_unix(),
        })
    }

    /// LRCLIB's exact match; `None` when it has no such track.
    fn lrclib_get(
        &self,
        title: &str,
        artist: &str,
        album: &str,
        duration_s: Option<f32>,
    ) -> anyhow::Result<Option<LrcRecord>> {
        let call = || {
            let mut req = self
                .agent
                .get(format!("{LRCLIB}/get"))
                .query("track_name", title)
                .query("artist_name", artist);
            if !album.is_empty() {
                req = req.query("album_name", album);
            }
            if let Some(d) = duration_s {
                req = req.query("duration", (d.round() as i64).to_string());
            }
            req.call()
        };
        match with_retry("LRCLIB", call) {
            Ok(mut resp) => Ok(Some(resp.body_mut().read_json::<LrcRecord>()?)),
            Err(e) if e.to_string().contains("404") => Ok(None),
            Err(e) => Err(e),
        }
    }

    fn lrclib_search(
        &self,
        title: &str,
        artist: &str,
        duration_s: Option<f32>,
    ) -> anyhow::Result<Option<Cached>> {
        let call = || {
            self.agent
                .get(format!("{LRCLIB}/search"))
                .query("track_name", title)
                .query("artist_name", artist)
                .call()
        };
        let results: Vec<LrcRecord> = with_retry("LRCLIB search", call)?
            .body_mut()
            .read_json()?;
        Ok(best_match(&results, duration_s).map(|r| {
            tracing::debug!(found = %r.track_name, by = %r.artist_name, "lyrics by search");
            to_cached(r)
        }))
    }

    /// The canonical title and first artist MusicBrainz has for an ISRC.
    fn musicbrainz_isrc(&self, isrc: &str) -> anyhow::Result<Option<(String, String)>> {
        #[derive(Deserialize)]
        struct Credit {
            name: String,
        }
        #[derive(Deserialize)]
        struct Recording {
            title: String,
            #[serde(rename = "artist-credit", default)]
            credit: Vec<Credit>,
        }
        #[derive(Deserialize)]
        struct Reply {
            #[serde(default)]
            recordings: Vec<Recording>,
        }
        {
            let mut last = self
                .last_musicbrainz
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if let Some(t) = *last {
                let wait = Duration::from_millis(1100).saturating_sub(t.elapsed());
                std::thread::sleep(wait);
            }
            *last = Some(std::time::Instant::now());
        }
        let call = || {
            self.agent
                .get(format!("{MUSICBRAINZ}/isrc/{isrc}"))
                .query("fmt", "json")
                .query("inc", "artist-credits")
                .call()
        };
        let reply = match with_retry("MusicBrainz", call) {
            Ok(mut r) => r.body_mut().read_json::<Reply>()?,
            Err(e) if e.to_string().contains("404") => return Ok(None),
            Err(e) => return Err(e),
        };
        Ok(reply.recordings.into_iter().next().and_then(|r| {
            let artist = r.credit.into_iter().next()?.name;
            Some((clean_title(&r.title), artist))
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn titles_lose_mix_names_and_features() {
        assert_eq!(clean_title("Latch (Extended Mix)"), "Latch");
        assert_eq!(clean_title("Danielle (smile on my face)"), "Danielle");
        assert_eq!(clean_title("Rain [Original Mix] feat. Someone"), "Rain");
        assert_eq!(clean_title("Stay ft. Kita Alexander"), "Stay");
        assert_eq!(clean_title("(Intro)"), "(Intro)", "never empty");
    }

    #[test]
    fn the_first_artist_is_used() {
        assert_eq!(main_artist("Disclosure, Sam Smith"), "Disclosure");
        assert_eq!(main_artist("Fred again.. & Swedish House Mafia"), "Fred again..");
        assert_eq!(main_artist("FISHER x Kita Alexander"), "FISHER");
        assert_eq!(main_artist("Drake"), "Drake");
    }

    fn rec(duration: f32, synced: bool) -> LrcRecord {
        LrcRecord {
            track_name: "t".into(),
            artist_name: "a".into(),
            duration: Some(duration),
            instrumental: false,
            synced_lyrics: synced.then(|| "[00:01.00] hi".to_string()),
            plain_lyrics: Some("hi".into()),
        }
    }

    #[test]
    fn prefers_synced_then_the_closest_length() {
        let r = [rec(200.0, false), rec(230.0, true), rec(205.0, true)];
        let best = best_match(&r, Some(201.0)).unwrap();
        assert_eq!(best.duration, Some(205.0));
        assert!(best_match(&[rec(400.0, true)], Some(200.0)).is_none(), "a different version");
    }

    #[test]
    fn answers_are_cached_and_read_back() {
        let dir = tempfile::tempdir().unwrap();
        let store = LyricsStore::new(dir.path());
        let q = Query {
            title: "Latch (Extended Mix)".into(),
            artist: "Disclosure, Sam Smith".into(),
            album: String::new(),
            duration_s: Some(255.4),
            isrc: None,
        };
        assert!(store.cached(&q).is_none());
        store.write_cache(
            &q,
            &Cached {
                status: "synced".into(),
                lrc: "[00:16.46] You lift my heart up\n".into(),
                source: "LRCLIB".into(),
                fetched_unix: now_unix(),
            },
        );
        match store.lookup(&q, false) {
            Outcome::Synced(l) => assert_eq!(l.lines[0].text, "You lift my heart up"),
            other => panic!("{other:?}"),
        }
        assert!(q.key().contains("disclosure - latch - 255"), "{}", q.key());
    }

    #[test]
    fn offline_without_a_cache_says_so() {
        let dir = tempfile::tempdir().unwrap();
        let store = LyricsStore::new(dir.path());
        let q = Query {
            title: "x".into(),
            artist: "y".into(),
            album: String::new(),
            duration_s: None,
            isrc: None,
        };
        assert_eq!(store.lookup(&q, false), Outcome::Offline);
    }

    #[test]
    fn samples_and_instrumentals_need_no_lookup() {
        let q = |title: &str, artist: &str, d: f32| Query {
            title: title.into(),
            artist: artist.into(),
            album: String::new(),
            duration_s: Some(d),
            isrc: None,
        };
        assert_eq!(known_without_lookup(&q("HORN", "", 4.0)), Some("none"));
        assert_eq!(known_without_lookup(&q("Riser", "Someone", 12.0)), Some("none"));
        assert_eq!(
            known_without_lookup(&q("Danielle (Instrumental)", "Fred again..", 200.0)),
            Some("instrumental")
        );
        assert_eq!(known_without_lookup(&q("Latch", "Disclosure", 255.0)), None);
    }
}
