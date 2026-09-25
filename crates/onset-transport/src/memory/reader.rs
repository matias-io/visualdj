//! Turning calibrated chains into deck state, and deck state into `TransportSnapshot`s.
//! `ChainReader` is generic over where bytes come from so it is tested against fake memory;
//! `MemoryTransport` (Windows) drives it against the live rekordbox process.
use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use onset_core::transport::{TrackRef, TransportSnapshot};

use std::path::{Path, PathBuf};

use super::chain::Chain;
use super::offsets::{DeckSignature, Offsets, PositionFormat};

/// Longest text block read for track info and paths.
const TEXT_MAX: usize = 512;

/// A source of another process's bytes.
pub trait Mem {
    fn module_base(&self) -> u64;
    fn read_exact(&self, addr: u64, buf: &mut [u8]) -> anyhow::Result<()>;

    fn read_u64(&self, addr: u64) -> anyhow::Result<u64> {
        let mut b = [0u8; 8];
        self.read_exact(addr, &mut b)?;
        Ok(u64::from_le_bytes(b))
    }
    fn read_i64(&self, addr: u64) -> anyhow::Result<i64> {
        let mut b = [0u8; 8];
        self.read_exact(addr, &mut b)?;
        Ok(i64::from_le_bytes(b))
    }
    fn read_i32(&self, addr: u64) -> anyhow::Result<i32> {
        let mut b = [0u8; 4];
        self.read_exact(addr, &mut b)?;
        Ok(i32::from_le_bytes(b))
    }
    fn read_f32(&self, addr: u64) -> anyhow::Result<f32> {
        let mut b = [0u8; 4];
        self.read_exact(addr, &mut b)?;
        Ok(f32::from_le_bytes(b))
    }
    fn read_f64(&self, addr: u64) -> anyhow::Result<f64> {
        let mut b = [0u8; 8];
        self.read_exact(addr, &mut b)?;
        Ok(f64::from_le_bytes(b))
    }
    fn read_u8(&self, addr: u64) -> anyhow::Result<u8> {
        let mut b = [0u8; 1];
        self.read_exact(addr, &mut b)?;
        Ok(b[0])
    }
}

/// In-memory stand-in for a process: blocks of bytes at absolute addresses.
#[derive(Debug, Default, Clone)]
pub struct FakeMem {
    base: u64,
    blocks: BTreeMap<u64, Vec<u8>>,
}

impl FakeMem {
    pub fn new(module_base: u64) -> Self {
        Self {
            base: module_base,
            blocks: BTreeMap::new(),
        }
    }

    pub fn put_bytes(&mut self, addr: u64, bytes: &[u8]) {
        self.blocks.insert(addr, bytes.to_vec());
    }

    pub fn put_u64(&mut self, addr: u64, v: u64) {
        self.put_bytes(addr, &v.to_le_bytes());
    }

    pub fn put_i64(&mut self, addr: u64, v: i64) {
        self.put_bytes(addr, &v.to_le_bytes());
    }

    pub fn put_f32(&mut self, addr: u64, v: f32) {
        self.put_bytes(addr, &v.to_le_bytes());
    }
}

impl Mem for FakeMem {
    fn module_base(&self) -> u64 {
        self.base
    }

    fn read_exact(&self, addr: u64, buf: &mut [u8]) -> anyhow::Result<()> {
        // Reads may span the end of a block: the rest is zero, like untouched heap.
        let (start, block) = self
            .blocks
            .range(..=addr)
            .next_back()
            .ok_or_else(|| anyhow::anyhow!("unmapped {addr:#x}"))?;
        let off = (addr - start) as usize;
        if off >= block.len() + 4096 {
            anyhow::bail!("unmapped {addr:#x}");
        }
        for (i, b) in buf.iter_mut().enumerate() {
            *b = block.get(off + i).copied().unwrap_or(0);
        }
        Ok(())
    }
}

/// Resolves a chain against any [`Mem`].
pub fn resolve_chain(mem: &impl Mem, chain: &Chain) -> Option<u64> {
    let mut addr = mem.module_base().checked_add(chain.root)?;
    for hop in &chain.hops {
        let next = mem.read_u64(addr).ok()?;
        if next == 0 {
            return None;
        }
        addr = next.checked_add(*hop)?;
    }
    Some(addr)
}

/// What one deck holds right now.
#[derive(Debug, Clone, PartialEq)]
pub struct DeckState {
    pub bpm: f32,
    /// Seconds of track time, when the position's unit is known.
    pub position_s: f64,
    /// The position as read, in the offsets' unit.
    pub position_raw: f64,
    pub track_info: Option<String>,
    pub anlz_path: Option<String>,
}

pub struct ChainReader<M: Mem> {
    mem: M,
    offsets: Offsets,
    /// Sample rate of the loaded track, used when the offsets count file samples.
    track_rate_hz: Option<f64>,
    /// Analysed tempo of the loaded track, for decks without a readable tempo field.
    track_bpm: Option<f32>,
    /// Position field addresses found through the signature, in deck order.
    found: Vec<u64>,
}

/// True when every anchor of `sig` reads as expected around a position field at `pos`.
pub fn signature_matches(mem: &impl Mem, sig: &DeckSignature, pos: u64) -> bool {
    let base = mem.module_base();
    sig.anchors.iter().all(|a| {
        let addr = pos.checked_add_signed(a.offset);
        addr.and_then(|addr| mem.read_u64(addr).ok()) == Some(base.wrapping_add(a.module_offset))
    })
}

/// Turns hits of the first anchor's bytes into confirmed position fields: each hit minus
/// the anchor's offset, kept when the other anchors agree, sorted by address (deck order)
/// and capped at the signature's deck count.
pub fn decks_from_hits(mem: &impl Mem, sig: &DeckSignature, hits: &[u64]) -> Vec<u64> {
    let Some(first) = sig.anchors.first() else {
        return Vec::new();
    };
    let mut out: Vec<u64> = hits
        .iter()
        .filter_map(|h| h.checked_add_signed(-first.offset))
        .filter(|pos| signature_matches(mem, sig, *pos))
        .collect();
    out.sort_unstable();
    out.dedup();
    out.truncate(sig.max_decks.max(1));
    out
}

impl<M: Mem> ChainReader<M> {
    pub fn new(mem: M, offsets: Offsets) -> Self {
        Self {
            mem,
            offsets,
            track_rate_hz: None,
            track_bpm: None,
            found: Vec::new(),
        }
    }

    /// Records the position fields a signature scan found, in deck order.
    pub fn set_found_decks(&mut self, positions: Vec<u64>) {
        self.found = positions;
    }

    /// Decks readable right now: the found ones with a signature, else the chained ones.
    pub fn deck_count(&self) -> usize {
        if self.offsets.signature.is_some() {
            self.found.len()
        } else {
            self.offsets.decks.len()
        }
    }

    /// The bytes to search for when locating decks by signature: the first anchor's value.
    pub fn signature_needle(&self) -> Option<[u8; 8]> {
        let sig = self.offsets.signature.as_ref()?;
        let first = sig.anchors.first()?;
        Some(self.mem.module_base().wrapping_add(first.module_offset).to_le_bytes())
    }

    /// Confirms hits of the needle and remembers the decks they identify.
    pub fn adopt_hits(&mut self, hits: &[u64]) -> usize {
        let Some(sig) = self.offsets.signature.clone() else {
            return 0;
        };
        self.found = decks_from_hits(&self.mem, &sig, hits);
        self.found.len()
    }

    /// True when a found deck no longer carries its signature (rekordbox rebuilt it).
    pub fn found_decks_stale(&self) -> bool {
        let Some(sig) = self.offsets.signature.as_ref() else {
            return false;
        };
        self.found
            .iter()
            .any(|pos| !signature_matches(&self.mem, sig, *pos))
    }

    pub fn set_track_sample_rate(&mut self, hz: Option<u32>) {
        self.track_rate_hz = hz.map(f64::from);
    }

    pub fn set_track_bpm(&mut self, bpm: Option<f32>) {
        self.track_bpm = bpm;
    }

    pub fn track_bpm(&self) -> Option<f32> {
        self.track_bpm
    }

    /// Units per second for the position: the offsets' fixed rate, else the track's.
    fn position_rate(&self) -> f64 {
        if self.offsets.position_rate_hz > 0.0 {
            self.offsets.position_rate_hz
        } else {
            self.track_rate_hz.unwrap_or(44_100.0)
        }
    }

    pub fn offsets(&self) -> &Offsets {
        &self.offsets
    }

    pub fn mem(&self) -> &M {
        &self.mem
    }

    pub fn master_deck(&self) -> Option<u8> {
        if let Some(sig) = self.offsets.signature.as_ref() {
            let flag = sig.master_flag.as_ref()?;
            return self.found.iter().position(|pos| {
                pos.checked_add_signed(flag.offset)
                    .and_then(|a| self.mem.read_u8(a).ok())
                    .is_some_and(|b| b & flag.mask != 0)
            })
            .and_then(|i| u8::try_from(i).ok());
        }
        let chain = self.offsets.master_deck.as_ref()?;
        let addr = resolve_chain(&self.mem, chain)?;
        let v = self.mem.read_u8(addr).ok()?;
        (usize::from(v) < self.offsets.decks.len().max(1)).then_some(v)
    }

    fn read_text(&self, chain: &Chain) -> Option<String> {
        let addr = resolve_chain(&self.mem, chain)?;
        let mut buf = vec![0u8; TEXT_MAX];
        // Shorten the read until it succeeds: the text may sit near the end of a page.
        let mut len = TEXT_MAX;
        while len >= 32 {
            if self.mem.read_exact(addr, &mut buf[..len]).is_ok() {
                break;
            }
            len /= 2;
        }
        if len < 32 {
            return None;
        }
        let end = buf[..len].iter().position(|b| *b == 0).unwrap_or(len);
        let s = String::from_utf8_lossy(&buf[..end]).into_owned();
        (!s.is_empty()).then_some(s)
    }

    /// Deck `index`, or `None` when its chains do not resolve (mid-load, no track).
    pub fn deck(&self, index: usize) -> Option<DeckState> {
        if self.offsets.signature.is_some() {
            let pos_addr = *self.found.get(index)?;
            let raw = self.read_position(pos_addr)?;
            return Some(DeckState {
                bpm: 0.0,
                position_s: raw / self.position_rate(),
                position_raw: raw,
                track_info: None,
                anlz_path: None,
            });
        }
        let d = self.offsets.decks.get(index)?;
        let bpm = match &d.bpm {
            Some(chain) => self.mem.read_f32(resolve_chain(&self.mem, chain)?).ok()?,
            None => 0.0,
        };
        let pos_addr = resolve_chain(&self.mem, &d.position)?;
        let raw = match self.offsets.position_format {
            PositionFormat::I64 => self.mem.read_i64(pos_addr).ok()? as f64,
            PositionFormat::I32 => f64::from(self.mem.read_i32(pos_addr).ok()?),
            PositionFormat::F64 => self.mem.read_f64(pos_addr).ok()?,
        };
        if !raw.is_finite() || !bpm.is_finite() {
            return None;
        }
        Some(DeckState {
            bpm,
            position_s: raw / self.position_rate(),
            position_raw: raw,
            track_info: d.track_info.as_ref().and_then(|c| self.read_text(c)),
            anlz_path: d.anlz_path.as_ref().and_then(|c| self.read_text(c)),
        })
    }
}

impl<M: Mem> ChainReader<M> {
    /// The raw position at `addr` in the offsets' format, when finite.
    fn read_position(&self, addr: u64) -> Option<f64> {
        let raw = match self.offsets.position_format {
            PositionFormat::I64 => self.mem.read_i64(addr).ok()? as f64,
            PositionFormat::I32 => f64::from(self.mem.read_i32(addr).ok()?),
            PositionFormat::F64 => self.mem.read_f64(addr).ok()?,
        };
        raw.is_finite().then_some(raw)
    }
}

/// `Track Title: X\nArtist: Y\nAlbum: Z` as rekordbox writes it.
pub fn parse_track_info(text: &str) -> TrackRef {
    let mut title = None;
    let mut artist = String::new();
    let mut album = String::new();
    for line in text.lines() {
        if let Some(v) = line.strip_prefix("Track Title: ") {
            title = Some(v.trim().to_string());
        } else if let Some(v) = line.strip_prefix("Artist: ") {
            artist = v.trim().to_string();
        } else if let Some(v) = line.strip_prefix("Album: ") {
            album = v.trim().to_string();
        }
    }
    match title {
        Some(title) => TrackRef::TitleArtist {
            title,
            artist,
            album,
        },
        None => TrackRef::Unknown,
    }
}

/// Which deck holds which open audio file. rekordbox keeps a track's file open while it is
/// loaded, so the open files name the loaded tracks; this pairs them with decks. A file
/// that appears while one deck is free goes to that deck; with several free decks, a deck
/// whose position fits the file's length (and no other) takes it; the rest wait until the
/// picture clears. Assignments persist until the file closes.
#[derive(Debug, Clone, Default)]
pub struct DeckFiles {
    assigned: Vec<Option<PathBuf>>,
}

impl DeckFiles {
    /// `positions[i]` is deck i's position in seconds (`None` when unreadable);
    /// `duration_of` gives a file's length when the library knows it. Returns the
    /// assignment after this update.
    pub fn update(
        &mut self,
        open: &[PathBuf],
        positions: &[Option<f64>],
        duration_of: &dyn Fn(&Path) -> Option<f64>,
    ) -> &[Option<PathBuf>] {
        self.assigned.resize(positions.len(), None);
        for slot in &mut self.assigned {
            if slot.as_ref().is_some_and(|f| !open.contains(f)) {
                *slot = None;
            }
        }
        loop {
            let new_files: Vec<&PathBuf> = open
                .iter()
                .filter(|f| !self.assigned.iter().any(|a| a.as_ref() == Some(*f)))
                .collect();
            let free: Vec<usize> = (0..self.assigned.len())
                .filter(|i| self.assigned[*i].is_none())
                .collect();
            if new_files.is_empty() || free.is_empty() {
                break;
            }
            if new_files.len() == 1 && free.len() == 1 {
                self.assigned[free[0]] = Some(new_files[0].clone());
                continue;
            }
            let mut progress = false;
            for file in &new_files {
                let fits: Vec<usize> = free
                    .iter()
                    .copied()
                    .filter(|i| self.assigned[*i].is_none())
                    .filter(|i| match (positions[*i], duration_of(file)) {
                        (Some(p), Some(d)) => p <= d + 1.0,
                        _ => true,
                    })
                    .collect();
                // Several decks could hold it: an unloaded deck reads zero, so a lone deck
                // that has moved off zero is the one with a track.
                let moved: Vec<usize> = fits
                    .iter()
                    .copied()
                    .filter(|i| positions[*i].is_some_and(|p| p > 0.5))
                    .collect();
                let pick = match (fits.as_slice(), moved.as_slice()) {
                    ([one], _) | (_, [one]) => Some(*one),
                    _ => None,
                };
                if let Some(i) = pick {
                    self.assigned[i] = Some((*file).clone());
                    progress = true;
                }
            }
            if !progress {
                break;
            }
        }
        &self.assigned
    }

    pub fn file_for(&self, deck: usize) -> Option<&PathBuf> {
        self.assigned.get(deck).and_then(Option::as_ref)
    }
}

/// Which deck the show follows when more than one is loaded. The master deck wins while it
/// plays; otherwise the deck that is playing, and during a transition (both playing) the
/// one already being followed, so the visuals switch when the outgoing deck stops rather
/// than the moment the incoming one starts.
#[derive(Debug, Clone, Copy, Default)]
pub struct DeckChooser {
    current: Option<usize>,
}

impl DeckChooser {
    /// `decks[i]` is `None` for a deck without a readable track, else whether it plays.
    pub fn choose(&mut self, master: Option<usize>, decks: &[Option<bool>]) -> Option<usize> {
        let loaded = |i: usize| decks.get(i).is_some_and(Option::is_some);
        let playing = |i: usize| decks.get(i).copied().flatten().unwrap_or(false);
        let any_playing = decks.contains(&Some(true));
        let master = master.filter(|m| loaded(*m));
        let pick = match master {
            Some(m) if playing(m) || !any_playing => Some(m),
            _ => self
                .current
                .filter(|c| playing(*c))
                .or_else(|| (0..decks.len()).find(|i| playing(*i)))
                .or_else(|| self.current.filter(|c| loaded(*c)))
                .or(master)
                .or_else(|| (0..decks.len()).find(|i| loaded(*i))),
        };
        self.current = pick;
        pick
    }
}

/// Decides "playing" from position movement, since rekordbox exposes no play flag we read,
/// and measures the playback rate (1.0 = nominal pitch) from how fast the position moves.
/// A position that has not changed for [`PlayTracker::GRACE`] counts as paused.
#[derive(Debug, Clone, Copy, Default)]
pub struct PlayTracker {
    last_position: Option<f64>,
    last_movement: Option<Instant>,
    /// Position and time of the last rate measurement.
    rate_anchor: Option<(f64, Instant)>,
    rate: Option<f64>,
}

impl PlayTracker {
    /// Longest gap between position changes still considered playing (covers 120 Hz reads
    /// landing between audio callbacks).
    pub const GRACE: Duration = Duration::from_millis(250);

    /// Rate measurements span at least this long, so read jitter averages out.
    pub const RATE_WINDOW: Duration = Duration::from_millis(500);

    pub fn update(&mut self, position_s: f64, now: Instant) -> bool {
        let moved = self
            .last_position
            .is_some_and(|p| (p - position_s).abs() > 1e-9);
        self.last_position = Some(position_s);
        if moved {
            self.last_movement = Some(now);
        }
        let playing = self
            .last_movement
            .is_some_and(|t| now.duration_since(t) <= Self::GRACE);
        match self.rate_anchor {
            Some((p0, t0)) if now.duration_since(t0) >= Self::RATE_WINDOW => {
                let dt = now.duration_since(t0).as_secs_f64();
                let r = (position_s - p0) / dt;
                // A jump (cue, loop, seek) is not a rate; keep the previous estimate. Pitch
                // faders reach ±16 %, so anything past ±20 % had a jump in the window.
                if playing && (0.8..=1.2).contains(&r) {
                    self.rate = Some(self.rate.map_or(r, |old| old + 0.3 * (r - old)));
                }
                self.rate_anchor = Some((position_s, now));
            }
            Some(_) => {}
            None => self.rate_anchor = Some((position_s, now)),
        }
        if !playing {
            self.rate_anchor = None;
        }
        playing
    }

    /// Measured playback rate, once half a second of movement has been seen.
    pub fn rate(&self) -> Option<f64> {
        self.rate
    }
}

/// Builds the snapshot the engine consumes from the master deck's state. With no tempo
/// field, `track_bpm` (from the library) times the measured `rate` gives the playing tempo.
pub fn snapshot_from_deck(
    deck: u8,
    state: &DeckState,
    playing: bool,
    now: Instant,
    track_bpm: Option<f32>,
    rate: Option<f64>,
) -> TransportSnapshot {
    let track = match (&state.anlz_path, &state.track_info) {
        (Some(path), _) if path.contains("ANLZ") => TrackRef::AnalysisPath(path.clone()),
        (_, Some(info)) => parse_track_info(info),
        _ => TrackRef::Unknown,
    };
    let (bpm_now, bpm_original) = if state.bpm > 0.0 {
        (state.bpm, track_bpm.unwrap_or(state.bpm))
    } else {
        let original = track_bpm.unwrap_or(0.0);
        (original * rate.unwrap_or(1.0) as f32, original)
    };
    TransportSnapshot {
        deck,
        track,
        playhead_s: state.position_s,
        bpm_now,
        bpm_original,
        playing,
        read_at: now,
    }
}

#[cfg(windows)]
pub use live::MemoryTransport;

#[cfg(windows)]
mod live {
    use std::path::PathBuf;
    use std::time::{Duration, Instant};

    use onset_core::transport::TransportSnapshot;

    use std::collections::HashMap;
    use std::path::Path;
    use std::sync::mpsc;

    use onset_core::transport::TrackRef;

    use super::super::REKORDBOX_EXE;
    use super::super::handles::open_audio_files;
    use super::super::process::Process;
    use super::super::scan::find_bytes;
    use super::{
        ChainReader, DeckChooser, DeckFiles, DeckState, Mem, PlayTracker, snapshot_from_deck,
    };

    /// How often the open-file list is refreshed.
    const FILES_EVERY: Duration = Duration::from_millis(1500);

    /// Starts the thread that lists rekordbox's open audio files; it stops when the
    /// receiver is dropped. Enumeration goes through every handle in the system and a few
    /// can stall, so it never runs on the poll thread.
    fn spawn_file_watcher(pid: u32) -> mpsc::Receiver<Vec<PathBuf>> {
        let (tx, rx) = mpsc::channel();
        std::thread::Builder::new()
            .name("onset-open-files".into())
            .spawn(move || {
                loop {
                    let files = open_audio_files(pid).unwrap_or_default();
                    if tx.send(files).is_err() {
                        break;
                    }
                    std::thread::sleep(FILES_EVERY);
                }
            })
            .ok();
        rx
    }

    fn norm_path(p: &Path) -> String {
        p.to_string_lossy().replace('\\', "/").to_lowercase()
    }

    /// Shortest file a deck is assumed to play; sampler pads are seconds long.
    const MIN_TRACK_S: f64 = 30.0;

    /// Files a deck could be playing: library tracks (when the library is known) outside
    /// any Sampler folder and at least half a minute long. rekordbox keeps every loaded
    /// sampler slot open too.
    fn is_deck_candidate(path: &Path, durations: &HashMap<String, f64>) -> bool {
        if path
            .components()
            .any(|c| c.as_os_str().to_string_lossy().eq_ignore_ascii_case("sampler"))
        {
            return false;
        }
        if durations.is_empty() {
            return true;
        }
        durations
            .get(&norm_path(path))
            .is_some_and(|d| *d >= MIN_TRACK_S)
    }

    /// Most needle hits considered per scan; the decks are the first few real ones.
    const SIGNATURE_HITS: usize = 64;
    /// Least time between two signature scans (each is a pass over the whole heap).
    const RESCAN: Duration = Duration::from_secs(5);
    /// Polls (about 120 per second) a deck must fail its signature before a rescan; a single
    /// failed read is a glitch, a second of them is a rebuilt player.
    const STALE_POLLS: u32 = 120;

    /// Scans the heap for the signature and hands the decks to the reader. Returns how
    /// many were found.
    fn discover_decks(reader: &mut ChainReader<Process>) -> usize {
        let Some(needle) = reader.signature_needle() else {
            return reader.deck_count();
        };
        let hits = find_bytes(reader.mem(), &needle, SIGNATURE_HITS);
        let n = reader.adopt_hits(&hits);
        tracing::info!(hits = hits.len(), decks = n, "signature scan");
        n
    }
    use crate::memory::offsets::Offsets;
    use crate::source::{SourceStatus, TransportSource};

    impl Mem for Process {
        fn module_base(&self) -> u64 {
            self.module_base
        }

        fn read_exact(&self, addr: u64, buf: &mut [u8]) -> anyhow::Result<()> {
            Process::read_exact(self, addr, buf)
        }
    }

    enum State {
        Searching {
            next_try: Instant,
        },
        Unsupported {
            version: String,
            next_try: Instant,
        },
        Connected {
            reader: Box<ChainReader<Process>>,
            /// One per configured deck.
            trackers: Vec<PlayTracker>,
            chooser: DeckChooser,
            failures: u32,
            last_scan: Instant,
            /// Consecutive polls in which a found deck lost its signature.
            stale_polls: u32,
            files_rx: mpsc::Receiver<Vec<PathBuf>>,
            open_files: Vec<PathBuf>,
            deck_files: DeckFiles,
        },
    }

    /// The rekordbox memory transport: finds the process, loads the offsets for its version,
    /// and reports the master deck at every poll.
    pub struct MemoryTransport {
        offsets_dir: PathBuf,
        state: State,
        /// Library file lengths by normalised path, for deck attribution.
        durations: HashMap<String, f64>,
    }

    const RETRY: Duration = Duration::from_secs(2);
    /// Consecutive failed reads before the process is considered gone.
    const MAX_FAILURES: u32 = 30;

    impl MemoryTransport {
        pub fn new(offsets_dir: PathBuf) -> Self {
            Self {
                offsets_dir,
                state: State::Searching {
                    next_try: Instant::now(),
                },
                durations: HashMap::new(),
            }
        }

        fn try_connect(&mut self) {
            let Ok(process) = Process::open(REKORDBOX_EXE) else {
                self.state = State::Searching {
                    next_try: Instant::now() + RETRY,
                };
                return;
            };
            let version = process.file_version().unwrap_or_else(|| "unknown".into());
            if let Some(offsets) = Offsets::for_version(&self.offsets_dir, &version) {
                tracing::info!(pid = process.pid, %version, "rekordbox connected");
                let pid = process.pid;
                let mut reader = Box::new(ChainReader::new(process, offsets));
                let decks = discover_decks(&mut reader);
                self.state = State::Connected {
                    reader,
                    trackers: vec![PlayTracker::default(); decks],
                    chooser: DeckChooser::default(),
                    failures: 0,
                    last_scan: Instant::now(),
                    stale_polls: 0,
                    files_rx: spawn_file_watcher(pid),
                    open_files: Vec::new(),
                    deck_files: DeckFiles::default(),
                };
            } else {
                tracing::warn!(%version, dir = %self.offsets_dir.display(), "no offsets for this rekordbox version; run the calibrator");
                self.state = State::Unsupported {
                    version,
                    next_try: Instant::now() + RETRY * 5,
                };
            }
        }
    }

    impl TransportSource for MemoryTransport {
        fn name(&self) -> &'static str {
            "rekordbox"
        }

        fn status(&self) -> SourceStatus {
            match &self.state {
                State::Searching { .. } => SourceStatus::Searching,
                State::Unsupported { version, .. } => {
                    SourceStatus::Unsupported(format!("no offsets for rekordbox {version}"))
                }
                State::Connected { .. } => SourceStatus::Connected,
            }
        }

        fn set_track_sample_rate(&mut self, hz: Option<u32>) {
            if let State::Connected { reader, .. } = &mut self.state {
                reader.set_track_sample_rate(hz);
            }
        }

        fn set_track_bpm(&mut self, bpm: Option<f32>) {
            if let State::Connected { reader, .. } = &mut self.state {
                reader.set_track_bpm(bpm);
            }
        }

        fn set_file_durations(&mut self, table: Vec<(PathBuf, f64)>) {
            self.durations = table
                .into_iter()
                .map(|(p, d)| (norm_path(&p), d))
                .collect();
            tracing::info!(files = self.durations.len(), "library file lengths received");
        }

        fn poll(&mut self) -> Option<TransportSnapshot> {
            let now = Instant::now();
            match &mut self.state {
                State::Searching { next_try } | State::Unsupported { next_try, .. } => {
                    if now >= *next_try {
                        self.try_connect();
                    }
                    None
                }
                State::Connected {
                    reader,
                    trackers,
                    chooser,
                    failures,
                    last_scan,
                    stale_polls,
                    files_rx,
                    open_files,
                    deck_files,
                } => {
                    while let Ok(files) = files_rx.try_recv() {
                        let files: Vec<PathBuf> = files
                            .into_iter()
                            .filter(|f| is_deck_candidate(f, &self.durations))
                            .collect();
                        if files != *open_files {
                            tracing::info!(files = ?files, "loaded tracks changed");
                            *open_files = files;
                        }
                    }
                    // Decks vanish when rekordbox rebuilds its players (a restart, a mode
                    // change); a fresh scan finds the new ones.
                    *stale_polls = if reader.found_decks_stale() {
                        *stale_polls + 1
                    } else {
                        0
                    };
                    if (trackers.is_empty() || *stale_polls >= STALE_POLLS)
                        && now.duration_since(*last_scan) >= RESCAN
                    {
                        tracing::info!(stale_polls, "looking for the decks again");
                        *last_scan = now;
                        *stale_polls = 0;
                        let decks = discover_decks(reader);
                        trackers.resize(decks, PlayTracker::default());
                    }
                    let states: Vec<Option<DeckState>> =
                        (0..trackers.len()).map(|i| reader.deck(i)).collect();
                    let playing: Vec<Option<bool>> = states
                        .iter()
                        .zip(trackers.iter_mut())
                        .map(|(s, t)| s.as_ref().map(|s| t.update(s.position_s, now)))
                        .collect();
                    let positions: Vec<Option<f64>> =
                        states.iter().map(|s| s.as_ref().map(|s| s.position_s)).collect();
                    let durations = &self.durations;
                    let before: Vec<Option<PathBuf>> = (0..positions.len())
                        .map(|i| deck_files.file_for(i).cloned())
                        .collect();
                    let after = deck_files.update(open_files, &positions, &|p| {
                        durations.get(&norm_path(p)).copied()
                    });
                    if after != before.as_slice() {
                        tracing::info!(decks = ?after, ?positions, "deck tracks");
                    }
                    let master = reader.master_deck().map(usize::from);
                    if let Some(i) = chooser.choose(master, &playing)
                        && let Some(state) = &states[i]
                    {
                        *failures = 0;
                        let mut snapshot = snapshot_from_deck(
                            u8::try_from(i).unwrap_or(0),
                            state,
                            playing[i].unwrap_or(false),
                            now,
                            reader.track_bpm(),
                            trackers[i].rate(),
                        );
                        if snapshot.track == TrackRef::Unknown
                            && let Some(file) = deck_files.file_for(i)
                        {
                            snapshot.track = TrackRef::FilePath(file.clone());
                        }
                        return Some(snapshot);
                    }
                    *failures += 1;
                    if *failures >= MAX_FAILURES
                        && super::super::process::find_pid(REKORDBOX_EXE).is_none()
                    {
                        tracing::warn!("rekordbox is gone; searching again");
                        self.state = State::Searching {
                            next_try: now + RETRY,
                        };
                    }
                    None
                }
            }
        }
    }
}
