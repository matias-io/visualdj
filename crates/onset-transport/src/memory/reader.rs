//! Turning calibrated chains into deck state, and deck state into `TransportSnapshot`s.
//! `ChainReader` is generic over where bytes come from so it is tested against fake memory;
//! `MemoryTransport` (Windows) drives it against the live rekordbox process.
use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use onset_core::transport::{TrackRef, TransportSnapshot};

use super::chain::Chain;
use super::offsets::{Offsets, PositionFormat};

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
}

impl<M: Mem> ChainReader<M> {
    pub fn new(mem: M, offsets: Offsets) -> Self {
        Self {
            mem,
            offsets,
            track_rate_hz: None,
        }
    }

    pub fn set_track_sample_rate(&mut self, hz: Option<u32>) {
        self.track_rate_hz = hz.map(f64::from);
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

    pub fn deck_count(&self) -> usize {
        self.offsets.decks.len()
    }

    pub fn master_deck(&self) -> Option<u8> {
        let addr = resolve_chain(&self.mem, &self.offsets.master_deck)?;
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
        let d = self.offsets.decks.get(index)?;
        let bpm = self.mem.read_f32(resolve_chain(&self.mem, &d.bpm)?).ok()?;
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

/// Decides "playing" from position movement, since rekordbox exposes no play flag we read.
/// A position that has not changed for [`PlayTracker::GRACE`] counts as paused.
#[derive(Debug, Clone, Copy, Default)]
pub struct PlayTracker {
    last_position: Option<f64>,
    last_movement: Option<Instant>,
}

impl PlayTracker {
    /// Longest gap between position changes still considered playing (covers 120 Hz reads
    /// landing between audio callbacks).
    pub const GRACE: Duration = Duration::from_millis(250);

    pub fn update(&mut self, position_s: f64, now: Instant) -> bool {
        let moved = self
            .last_position
            .is_some_and(|p| (p - position_s).abs() > 1e-9);
        self.last_position = Some(position_s);
        if moved {
            self.last_movement = Some(now);
        }
        self.last_movement
            .is_some_and(|t| now.duration_since(t) <= Self::GRACE)
    }
}

/// Builds the snapshot the engine consumes from the master deck's state.
pub fn snapshot_from_deck(
    deck: u8,
    state: &DeckState,
    playing: bool,
    now: Instant,
) -> TransportSnapshot {
    let track = match (&state.anlz_path, &state.track_info) {
        (Some(path), _) if path.contains("ANLZ") => TrackRef::AnalysisPath(path.clone()),
        (_, Some(info)) => parse_track_info(info),
        _ => TrackRef::Unknown,
    };
    TransportSnapshot {
        deck,
        track,
        playhead_s: state.position_s,
        bpm_now: state.bpm,
        // The analysed tempo is not in memory; the engine fills it in from the library.
        bpm_original: state.bpm,
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

    use super::super::REKORDBOX_EXE;
    use super::super::process::Process;
    use super::{ChainReader, Mem, PlayTracker, snapshot_from_deck};
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
            tracker: PlayTracker,
            failures: u32,
        },
    }

    /// The rekordbox memory transport: finds the process, loads the offsets for its version,
    /// and reports the master deck at every poll.
    pub struct MemoryTransport {
        offsets_dir: PathBuf,
        state: State,
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
                self.state = State::Connected {
                    reader: Box::new(ChainReader::new(process, offsets)),
                    tracker: PlayTracker::default(),
                    failures: 0,
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
                    tracker,
                    failures,
                } => {
                    let master = reader.master_deck();
                    let deck = master.and_then(|m| reader.deck(usize::from(m)).map(|d| (m, d)));
                    if let Some((m, state)) = deck {
                        *failures = 0;
                        let playing = tracker.update(state.position_s, now);
                        return Some(snapshot_from_deck(m, &state, playing, now));
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
