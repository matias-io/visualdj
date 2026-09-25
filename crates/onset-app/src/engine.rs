//! The engine thread: polls the transport, drives the clock, structure, analyzer and
//! director at 120 Hz, and publishes one `MusicState` for the renderer to read each frame.
//!
//! The transport is rekordbox's memory by default. The simulator is a developer tool: it
//! only runs when asked for explicitly, and Onset never plays music on its own otherwise.
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use arc_swap::ArcSwap;
use onset_audio::analyzer::Analyzer;
use onset_audio::capture::LoopbackCapture;
use onset_core::audio_features::AudioFeatures;
use onset_core::clock::Clock;
use onset_core::director::Director;
use onset_core::music_state::MusicState;
use onset_core::structure::Structure;
use onset_core::track::{HotCue, TrackMeta};
use onset_core::transport::{TrackRef, TransportSnapshot};
use onset_rekordbox::anlz::Analysis;
use onset_rekordbox::library::Library;
use onset_rekordbox::paths::RekordboxPaths;
use onset_transport::sim::SimPlayer;
use onset_transport::source::{SourceStatus, TransportSource};

const TICK: Duration = Duration::from_micros(1_000_000 / 120);

pub struct EngineConfig {
    /// rekordbox data folder; `None` discovers `%APPDATA%\Pioneer\rekordbox`.
    pub app_dir: Option<PathBuf>,
    pub cache_dir: PathBuf,
    /// Where `<version>.toml` pointer-chain files live for the memory transport.
    pub offsets_dir: PathBuf,
    /// Developer mode: play this library title through the simulator instead of reading
    /// rekordbox. `None` is the normal case.
    pub sim_track: Option<String>,
    pub sim_seek_s: f64,
    pub sim_gain: f32,
    /// Loopback endpoint substring; default output device when `None`.
    pub audio_device: Option<String>,
    /// Capture the system mix (rekordbox's output) for the analyzer. The simulator taps its
    /// own audio instead.
    pub capture_audio: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub enum EngineCommand {
    Pause,
    Resume,
    Seek(f64),
    SetRate(f32),
    LoadTrack(String),
    /// Capture this device (name fragment), or pick automatically with `None`; applies now.
    SetAudioDevice(Option<String>),
    /// Look up and cache the lyrics of every track in the collection, in the background.
    PrefetchLyrics,
}

#[derive(Debug, Clone, PartialEq)]
pub enum EngineStatus {
    Starting,
    /// No usable source yet; the text says what is missing ("waiting for rekordbox").
    Waiting(String),
    /// A source is connected but no track is loaded on the master deck.
    Idle,
    /// A deck is playing but its track could not be identified: the show runs on audio
    /// and playhead alone, without the analysed structure.
    Unidentified {
        source: String,
    },
    Running {
        source: String,
        track: String,
    },
    Error(String),
}

impl EngineStatus {
    /// One line for the HUD.
    pub fn line(&self) -> String {
        match self {
            Self::Starting => "starting".to_string(),
            Self::Waiting(why) => why.clone(),
            Self::Idle => "connected, no track".to_string(),
            Self::Unidentified { source } => format!("{source}: unidentified track (audio only)"),
            Self::Running { source, track } => format!("{source}: {track}"),
            Self::Error(e) => format!("error: {e}"),
        }
    }
}

/// One library row as the settings panel lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrackEntry {
    /// What `EngineCommand::LoadTrack` takes.
    pub title: String,
    /// "Artist — Title", what the panel shows.
    pub label: String,
}

/// "Artist — Title", or just the title when rekordbox has no artist for it.
pub fn track_label(artist: &str, title: &str) -> String {
    if artist.trim().is_empty() {
        title.to_string()
    } else {
        format!("{artist} — {title}")
    }
}

pub struct Engine {
    state: Arc<ArcSwap<MusicState>>,
    status: Arc<ArcSwap<EngineStatus>>,
    commands: crossbeam_channel::Sender<EngineCommand>,
    tracks: Vec<TrackEntry>,
    simulator: bool,
    /// The audio device being captured, for the launcher.
    audio_device: Arc<ArcSwap<String>>,
    /// Progress of the whole-library lyrics look-up, empty when none has run.
    lyrics_progress: Arc<ArcSwap<String>>,
    _thread: std::thread::JoinHandle<()>,
}

/// Looks up the lyrics of every track (cached ones are instant) and reports progress.
fn prefetch_lyrics(tracks: &[TrackMeta], cache_dir: &std::path::Path, progress: &ArcSwap<String>) {
    use onset_lyrics::{LyricsStore, Outcome, Query};
    let store = LyricsStore::new(&cache_dir.join("lyrics"));
    let total = tracks.len();
    let mut synced = 0;
    for (i, t) in tracks.iter().enumerate() {
        let q = Query::from_meta(t);
        let cached = store.cached(&q).is_some();
        if matches!(store.lookup(&q, true), Outcome::Synced(_)) {
            synced += 1;
        }
        progress.store(Arc::new(format!(
            "Checked {} of {total} tracks; {synced} have synced lyrics",
            i + 1
        )));
        if !cached {
            std::thread::sleep(Duration::from_millis(300)); // gentle on the free service
        }
    }
    progress.store(Arc::new(format!(
        "Done: {synced} of {total} tracks have synced lyrics (the rest are instrumentals, samples or not in LRCLIB)"
    )));
}

/// The track the master deck holds, with everything the structure engine needs.
struct Current {
    meta: TrackMeta,
    analysis: Option<Analysis>,
    cues: Vec<HotCue>,
    /// How the source referred to it, so a repeat read is not a reload.
    track_ref: TrackRef,
    /// The deck it plays on (live only), so a switch to an unknown track clears it.
    deck: Option<u8>,
}

struct SimSource {
    player: SimPlayer,
    tap: crossbeam_channel::Receiver<Vec<f32>>,
    analyzer: Analyzer,
}

enum Source {
    Sim(Box<SimSource>),
    Live(Box<dyn TransportSource>),
}

impl Source {
    fn name(&self) -> &'static str {
        match self {
            Self::Sim(_) => "simulator",
            Self::Live(s) => s.name(),
        }
    }

    fn status(&self) -> SourceStatus {
        match self {
            Self::Sim(_) => SourceStatus::Connected,
            Self::Live(s) => s.status(),
        }
    }

    fn poll(&mut self) -> Option<TransportSnapshot> {
        match self {
            Self::Sim(s) => s.player.poll(),
            Self::Live(s) => s.poll(),
        }
    }
}

struct Capture {
    stream: LoopbackCapture,
    rx: crossbeam_channel::Receiver<Vec<f32>>,
    analyzer: Analyzer,
}

/// How often the capture checks that it still has the right device.
const AUDIO_CHECK: Duration = Duration::from_secs(3);

/// Asks every few seconds, off the engine thread (listing devices takes tens of
/// milliseconds), which device the capture should use for the current choice.
fn spawn_audio_watch(
    choice: Arc<parking_lot::Mutex<Option<String>>>,
) -> crossbeam_channel::Receiver<Option<String>> {
    let (tx, rx) = crossbeam_channel::bounded(1);
    let spawned = std::thread::Builder::new()
        .name("onset-audio-watch".into())
        .spawn(move || {
            loop {
                let wanted = choice.lock().clone();
                let name = onset_audio::capture::preferred_endpoint(wanted.as_deref());
                if tx.send(name).is_err() {
                    break;
                }
                std::thread::sleep(AUDIO_CHECK);
            }
        });
    if let Err(e) = spawned {
        tracing::warn!(error = %e, "cannot start the audio device watch");
    }
    rx
}

fn open_capture(choice: Option<&str>) -> Option<Capture> {
    match LoopbackCapture::start(choice) {
        Ok((stream, rx, rate)) => Some(Capture {
            stream,
            rx,
            analyzer: Analyzer::new(rate),
        }),
        Err(e) => {
            tracing::warn!("audio capture unavailable: {e:#}");
            None
        }
    }
}

/// Finds the library row a transport reference points at.
pub fn resolve_track(library: &Library, r: &TrackRef) -> Option<TrackMeta> {
    match r {
        TrackRef::Unknown => None,
        TrackRef::Id(id) => library.by_id(*id).cloned(),
        TrackRef::AnalysisPath(path) => library.by_analysis_path(path).cloned(),
        TrackRef::TitleArtist { title, artist, .. } => library
            .find_by_title_artist(title, artist)
            .or_else(|| library.find_by_title_artist(title, ""))
            .cloned(),
        TrackRef::FilePath(path) => library.by_file_path(path).cloned(),
    }
}

fn load_current(
    library: &Library,
    paths: &RekordboxPaths,
    meta: TrackMeta,
    track_ref: TrackRef,
) -> Current {
    let analysis = meta.analysis_path.as_deref().and_then(|rel| {
        onset_rekordbox::anlz::load_analysis(paths, rel)
            .map_err(|e| tracing::warn!(title = %meta.title, "analysis unavailable: {e}"))
            .ok()
    });
    let cues = library.cues(meta.id);
    Current {
        meta,
        analysis,
        cues,
        track_ref,
        deck: None,
    }
}

/// Library tracks rekordbox streams (TIDAL, Beatport...) rather than reads from a file,
/// with their 3-band waveforms, for [`crate::stream_match::StreamMatcher`].
fn streaming_candidates(library: &Library, paths: &RekordboxPaths) -> Vec<crate::stream_match::Candidate> {
    library
        .tracks()
        .iter()
        .filter(|t| {
            t.file_path.as_ref().is_some_and(|p| {
                let s = p.to_string_lossy();
                // A service URI such as "tidal:tracks:123" rather than a drive path.
                s.find(':').is_some_and(|i| i > 1) && !s.contains('\\')
            })
        })
        .filter_map(|t| {
            let rel = t.analysis_path.as_deref()?;
            let bytes = std::fs::read(paths.resolve_share(rel).with_extension("2EX")).ok()?;
            let bands = onset_rekordbox::anlz::parse_2ex(&bytes)?;
            (!bands.frames.is_empty()).then(|| crate::stream_match::Candidate {
                meta: t.clone(),
                bands: Arc::new(bands),
            })
        })
        .collect()
}

fn start_sim(
    library: &Library,
    paths: &RekordboxPaths,
    title: &str,
    cfg: &EngineConfig,
) -> anyhow::Result<(SimSource, Current)> {
    let meta = library
        .find_by_title_artist(title, "")
        .ok_or_else(|| anyhow::anyhow!("no track titled {title:?}"))?
        .clone();
    let file = meta
        .file_path
        .clone()
        .filter(|p| p.exists())
        .ok_or_else(|| anyhow::anyhow!("audio file for {title:?} not found"))?;
    let player = SimPlayer::start(&file, TrackRef::Id(meta.id), meta.bpm.unwrap_or(120.0))?;
    player.set_gain(cfg.sim_gain);
    player.seek(cfg.sim_seek_s);
    let tap = player.tap_audio();
    let analyzer = Analyzer::new(player.tap_sample_rate());
    let track_ref = TrackRef::Id(meta.id);
    let current = load_current(library, paths, meta, track_ref);
    Ok((
        SimSource {
            player,
            tap,
            analyzer,
        },
        current,
    ))
}

impl Engine {
    pub fn start(cfg: EngineConfig) -> anyhow::Result<Self> {
        let state = Arc::new(ArcSwap::from_pointee(MusicState::default()));
        let status = Arc::new(ArcSwap::from_pointee(EngineStatus::Starting));
        let (tx, rx) = crossbeam_channel::unbounded::<EngineCommand>();

        // Load the library on the caller's thread so configuration errors surface immediately.
        let paths = match &cfg.app_dir {
            Some(dir) => RekordboxPaths::from_app_dir(dir)?,
            None => RekordboxPaths::discover()?,
        };
        let library = Library::open(&paths, &cfg.cache_dir)?;
        let mut tracks: Vec<TrackEntry> = library
            .tracks()
            .iter()
            .filter(|t| t.file_path.as_ref().is_some_and(|p| p.exists()))
            .map(|t| TrackEntry {
                title: t.title.clone(),
                label: track_label(&t.artist, &t.title),
            })
            .collect();
        tracks.sort_by_key(|t| t.label.to_lowercase());

        let (source, current) = if let Some(title) = &cfg.sim_track {
            let (sim, current) = start_sim(&library, &paths, title, &cfg)?;
            (Source::Sim(Box::new(sim)), Some(current))
        } else {
            let mut live = live_source(&cfg);
            live.set_file_durations(
                library
                    .tracks()
                    .iter()
                    .filter_map(|t| Some((t.file_path.clone()?, f64::from(t.duration_s?))))
                    .collect(),
            );
            (Source::Live(live), None)
        };
        let simulator = matches!(source, Source::Sim(_));
        let capture = if cfg.capture_audio && !simulator {
            open_capture(cfg.audio_device.as_deref())
        } else {
            None
        };
        let audio_device = Arc::new(ArcSwap::from_pointee(
            capture
                .as_ref()
                .map_or_else(|| "none".to_string(), |c| c.stream.name().to_string()),
        ));
        let audio_device_w = audio_device.clone();
        let lyrics_progress = Arc::new(ArcSwap::from_pointee(String::new()));
        let lyrics_progress_w = lyrics_progress.clone();
        let audio_choice = Arc::new(parking_lot::Mutex::new(cfg.audio_device.clone()));

        let (state_w, status_w) = (state.clone(), status.clone());
        let thread = std::thread::Builder::new()
            .name("onset-engine".into())
            .spawn(move || {
                let mut engine = Running {
                    cfg,
                    paths,
                    library,
                    source,
                    current,
                    capture,
                    clock: Clock::new(),
                    director: Director::new(),
                    started: Instant::now(),
                    last_tick: Instant::now(),
                    last_status: None,
                    unresolved: None,
                    matcher: None,
                    deck: None,
                    audio_checked: Instant::now(),
                    audio_device: audio_device_w,
                    audio_choice: audio_choice.clone(),
                    audio_want: spawn_audio_watch(audio_choice),
                    lyrics_progress: lyrics_progress_w,
                    prefetching: Arc::new(std::sync::atomic::AtomicBool::new(false)),
                };

                engine.run(&rx, &state_w, &status_w);
            })?;
        Ok(Self {
            state,
            status,
            commands: tx,
            tracks,
            simulator,
            audio_device,
            lyrics_progress,
            _thread: thread,
        })
    }

    /// The latest assembled state; cheap to call every frame.
    pub fn state(&self) -> Arc<MusicState> {
        self.state.load_full()
    }

    pub fn status(&self) -> EngineStatus {
        (**self.status.load()).clone()
    }

    /// Playable library tracks (audio file present), sorted by artist and title.
    pub fn tracks(&self) -> &[TrackEntry] {
        &self.tracks
    }

    /// The audio device being captured ("none" when capture could not start).
    pub fn audio_device(&self) -> String {
        (**self.audio_device.load()).clone()
    }

    /// Progress of the whole-library lyrics look-up; empty when none has run.
    pub fn lyrics_progress(&self) -> String {
        (**self.lyrics_progress.load()).clone()
    }

    /// True when the developer simulator is the source (transport controls apply).
    pub fn is_simulator(&self) -> bool {
        self.simulator
    }

    pub fn command(&self, cmd: EngineCommand) {
        let _ = self.commands.send(cmd);
    }
}

#[cfg(windows)]
fn live_source(cfg: &EngineConfig) -> Box<dyn TransportSource> {
    Box::new(onset_transport::memory::reader::MemoryTransport::new(
        cfg.offsets_dir.clone(),
    ))
}

#[cfg(not(windows))]
fn live_source(_cfg: &EngineConfig) -> Box<dyn TransportSource> {
    struct Never;
    impl TransportSource for Never {
        fn name(&self) -> &'static str {
            "rekordbox"
        }
        fn status(&self) -> SourceStatus {
            SourceStatus::Unsupported("rekordbox memory reading needs Windows".into())
        }
        fn poll(&mut self) -> Option<TransportSnapshot> {
            None
        }
    }
    Box::new(Never)
}

/// State owned by the engine thread.
struct Running {
    cfg: EngineConfig,
    paths: RekordboxPaths,
    library: Library,
    source: Source,
    current: Option<Current>,
    capture: Option<Capture>,
    clock: Clock,
    director: Director,
    started: Instant,
    last_tick: Instant,
    last_status: Option<EngineStatus>,
    /// The last track reference the library could not resolve, so it is reported once.
    unresolved: Option<TrackRef>,
    /// Names streamed tracks by their sound when no file does; built the first time a
    /// deck plays something no file names.
    matcher: Option<crate::stream_match::StreamMatcher>,
    /// The deck the last live snapshot came from.
    deck: Option<u8>,
    /// When the capture last checked it has the right device.
    audio_checked: Instant,
    /// Shared with the handle: the device being captured.
    audio_device: Arc<ArcSwap<String>>,
    /// The device choice, shared with the watch thread.
    audio_choice: Arc<parking_lot::Mutex<Option<String>>>,
    /// The watch thread's latest answer: the device the capture should use.
    audio_want: crossbeam_channel::Receiver<Option<String>>,
    /// Shared with the handle: the whole-library lyrics look-up's progress.
    lyrics_progress: Arc<ArcSwap<String>>,
    /// Set while the whole-library look-up runs, so a second press does not start another.
    prefetching: Arc<std::sync::atomic::AtomicBool>,
}

impl Running {
    fn status_now(&self) -> EngineStatus {
        match (self.source.status(), &self.current) {
            (SourceStatus::Searching, _) => EngineStatus::Waiting("waiting for rekordbox".into()),
            (SourceStatus::Scanning, _) => {
                EngineStatus::Waiting("rekordbox found, finding its decks".into())
            }
            (SourceStatus::Unsupported(why) | SourceStatus::Error(why), _) => {
                EngineStatus::Waiting(why)
            }
            (SourceStatus::Connected, Some(c)) => EngineStatus::Running {
                source: self.source.name().to_string(),
                track: format!("{} - {}", c.meta.artist, c.meta.title),
            },
            (SourceStatus::Connected, None) if self.clock.is_playing() => {
                EngineStatus::Unidentified {
                    source: self.source.name().to_string(),
                }
            }
            (SourceStatus::Connected, None) => EngineStatus::Idle,
        }
    }

    fn publish_status(&mut self, status: &ArcSwap<EngineStatus>) {
        let s = self.status_now();
        if self.last_status.as_ref() != Some(&s) {
            tracing::info!(status = %s.line(), "engine");
            status.store(Arc::new(s.clone()));
            self.last_status = Some(s);
        }
    }

    fn handle(&mut self, cmd: &EngineCommand, status: &ArcSwap<EngineStatus>) {
        match (cmd, &mut self.source) {
            (EngineCommand::Pause, Source::Sim(s)) => s.player.pause(true),
            (EngineCommand::Resume, Source::Sim(s)) => s.player.pause(false),
            (EngineCommand::Seek(t), Source::Sim(s)) => s.player.seek(*t),
            (EngineCommand::SetRate(r), Source::Sim(s)) => s.player.set_rate(*r),
            (EngineCommand::LoadTrack(title), Source::Sim(_)) => {
                match start_sim(&self.library, &self.paths, title, &self.cfg) {
                    Ok((sim, current)) => {
                        self.source = Source::Sim(Box::new(sim));
                        self.current = Some(current);
                        self.clock = Clock::new();
                    }
                    Err(e) => {
                        tracing::error!("load failed: {e:#}");
                        status.store(Arc::new(EngineStatus::Error(format!("{e:#}"))));
                        self.last_status = None;
                    }
                }
            }
            (EngineCommand::PrefetchLyrics, _) => {
                use std::sync::atomic::Ordering;
                if !self.prefetching.swap(true, Ordering::AcqRel) {
                    let tracks = self.library.tracks().to_vec();
                    let cache = self.cfg.cache_dir.clone();
                    let progress = self.lyrics_progress.clone();
                    let running = self.prefetching.clone();
                    progress.store(Arc::new("Starting the lyrics look-up...".into()));
                    let spawned = std::thread::Builder::new()
                        .name("onset-lyrics-all".into())
                        .spawn(move || {
                            prefetch_lyrics(&tracks, &cache, &progress);
                            running.store(false, Ordering::Release);
                        });
                    if let Err(e) = spawned {
                        tracing::warn!(error = %e, "cannot start the lyrics look-up");
                        self.prefetching.store(false, Ordering::Release);
                    }
                }
            }
            (EngineCommand::SetAudioDevice(choice), _) => {
                self.cfg.audio_device.clone_from(choice);
                self.audio_choice.lock().clone_from(choice);
                self.tend_audio(true);
            }
            (_, Source::Live(_)) => {
                tracing::debug!(?cmd, "transport command ignored: rekordbox is in control");
            }
        }
    }

    /// A live snapshot names a track; keep `current` in step with it.
    fn follow_track(&mut self, snapshot: &TransportSnapshot) {
        if self.deck != Some(snapshot.deck) {
            self.deck = Some(snapshot.deck);
            if let Some(m) = self.matcher.as_mut() {
                m.reset();
            }
        }
        if snapshot.track == TrackRef::Unknown {
            // A deck with no file (a streamed track): keep what is known about this deck,
            // but never show the previous deck's track over it.
            if self.current.as_ref().is_some_and(|c| c.deck != Some(snapshot.deck)) {
                tracing::info!(deck = snapshot.deck, "playing deck's track is not identified yet");
                self.current = None;
                self.clock = Clock::new();
            }
            return;
        }
        let same = self
            .current
            .as_ref()
            .is_some_and(|c| c.track_ref == snapshot.track);
        if same {
            if let Some(c) = self.current.as_mut() {
                c.deck = Some(snapshot.deck);
            }
            return;
        }
        if let Some(meta) = resolve_track(&self.library, &snapshot.track) {
            tracing::info!(title = %meta.title, artist = %meta.artist, "master deck track");
            if let Source::Live(s) = &mut self.source {
                s.set_track_sample_rate(meta.sample_rate);
                s.set_track_bpm(meta.bpm);
            }
            let mut current = load_current(&self.library, &self.paths, meta, snapshot.track.clone());
            current.deck = Some(snapshot.deck);
            self.current = Some(current);
            self.clock = Clock::new();
            return;
        }
        if self.unresolved.as_ref() != Some(&snapshot.track) {
            tracing::warn!(track = ?snapshot.track, "master deck track not in the library");
            self.unresolved = Some(snapshot.track.clone());
        }
        if self.current.is_some() {
            self.current = None;
            self.clock = Clock::new();
        }
    }

    /// While the playing deck's track is unknown, listens for one of the library's streamed
    /// tracks and adopts it once the sound matches.
    fn recognise_stream(&mut self, now: Instant, time_s: f64) {
        if !matches!(self.source, Source::Live(_)) || !self.clock.is_playing() {
            return;
        }
        if self.matcher.is_none() {
            let candidates = streaming_candidates(&self.library, &self.paths);
            tracing::info!(tracks = candidates.len(), "streaming tracks that can be recognised by ear");
            self.matcher = Some(crate::stream_match::StreamMatcher::new(candidates));
        }
        if !self.matcher.as_ref().is_some_and(crate::stream_match::StreamMatcher::has_candidates) {
            return;
        }
        let Some(playhead) = self.clock.playhead_at(now) else {
            return;
        };
        let a = self.audio(now);
        let g = a.groups;
        let Some(matcher) = self.matcher.as_mut() else {
            return;
        };
        matcher.listen(
            time_s,
            playhead,
            [f32::midpoint(g[0], g[1]), f32::midpoint(g[2], g[3]), f32::midpoint(g[4], g[5])],
        );
        if let Some(meta) = matcher.best().cloned() {
            tracing::info!(title = %meta.title, artist = %meta.artist, "streamed track recognised by ear");
            if let Source::Live(s) = &mut self.source {
                s.set_track_sample_rate(meta.sample_rate);
                s.set_track_bpm(meta.bpm);
            }
            let mut current = load_current(&self.library, &self.paths, meta, TrackRef::Unknown);
            current.deck = self.deck;
            self.current = Some(current);
            if let Some(m) = self.matcher.as_mut() {
                m.reset();
            }
        }
    }

    /// Keeps the capture on the right device: reopens it when the device went away (a
    /// controller unplugged), and switches when the preferred one changes (the controller
    /// plugged back in, or a new choice in the launcher). Checks every few seconds, or now
    /// when `force` is set.
    fn tend_audio(&mut self, force: bool) {
        if !self.cfg.capture_audio || matches!(self.source, Source::Sim(_)) {
            return;
        }
        let lost = self.capture.as_ref().is_none_or(|c| c.stream.is_lost());
        let have = self.capture.as_ref().map(|c| c.stream.name().to_string());
        // The watch thread's newest answer, if it has one.
        let mut want = None;
        while let Ok(w) = self.audio_want.try_recv() {
            want = Some(w);
        }
        let switch = match &want {
            Some(Some(name)) => have.as_ref() != Some(name),
            _ => false,
        };
        let retry_lost = lost && self.audio_checked.elapsed() >= Duration::from_secs(1);
        if !(force || switch || retry_lost) {
            return;
        }
        self.audio_checked = Instant::now();
        tracing::info!(from = ?have, to = ?want.flatten(), lost, "switching audio capture");
        self.capture = None; // close the old stream before opening the new one
        self.capture = open_capture(self.cfg.audio_device.as_deref());
        let name = self
            .capture
            .as_ref()
            .map_or_else(|| "none".to_string(), |c| c.stream.name().to_string());
        self.audio_device.store(Arc::new(name));
    }

    fn audio(&mut self, now: Instant) -> AudioFeatures {
        if let Some(cap) = self.capture.as_mut() {
            while let Ok(block) = cap.rx.try_recv() {
                cap.analyzer.push(&block);
            }
            return cap.analyzer.latest(now);
        }
        if let Source::Sim(s) = &mut self.source {
            while let Ok(block) = s.tap.try_recv() {
                s.analyzer.push_at(&block, now);
            }
            return s.analyzer.latest(now);
        }
        AudioFeatures::silent()
    }

    fn tick(&mut self) -> MusicState {
        let now = Instant::now();
        let dt = now.duration_since(self.last_tick).as_secs_f32();
        self.last_tick = now;
        let time_s = self.started.elapsed().as_secs_f64();

        if let Some(mut snapshot) = self.source.poll() {
            if matches!(self.source, Source::Live(_)) {
                self.follow_track(&snapshot);
                // Memory reads carry the playing tempo only; the analysed tempo is ours.
                if let Some(bpm) = self.current.as_ref().and_then(|c| c.meta.bpm)
                    && bpm > 0.0
                {
                    snapshot.bpm_original = bpm;
                }
            }
            self.clock.observe(&snapshot);
        }

        self.tend_audio(false);
        if self.current.is_none() {
            self.recognise_stream(now, time_s);
        }
        let Some(current) = self.current.as_ref() else {
            // No identified track: the visuals still get the audio and the playhead.
            let st = onset_core::structure::StructureState::default();
            let intensity = self.director.update(&st, dt);
            let audio = self.audio(now);
            return MusicState::assemble(
                time_s,
                self.clock.playhead_at(now).unwrap_or(0.0),
                self.clock.is_playing(),
                self.clock.rate(),
                &st,
                audio,
                intensity,
                None,
            );
        };
        let playhead = self.clock.playhead_at(now).unwrap_or(0.0);
        let meta = current.meta.clone();
        let st = match &current.analysis {
            Some(a) => {
                Structure::new(&a.grid, a.phrases.as_ref(), &current.cues).at(playhead * 1000.0)
            }
            None => onset_core::structure::StructureState {
                bpm: meta.bpm.unwrap_or(0.0),
                ..Default::default()
            },
        };
        let analysis_now = current
            .analysis
            .as_ref()
            .and_then(|a| a.bands.as_ref())
            .map(|b| b.at(playhead));
        let intensity = self.director.update(&st, dt);
        let audio = self.audio(now);
        let mut ms = MusicState::assemble(
            time_s,
            playhead,
            self.clock.is_playing(),
            self.clock.rate(),
            &st,
            audio,
            intensity,
            Some(meta),
        );
        if let Some(at) = analysis_now {
            ms.analysis = at;
        }
        ms
    }

    fn run(
        &mut self,
        rx: &crossbeam_channel::Receiver<EngineCommand>,
        state: &ArcSwap<MusicState>,
        status: &ArcSwap<EngineStatus>,
    ) {
        self.publish_status(status);
        loop {
            while let Ok(cmd) = rx.try_recv() {
                self.handle(&cmd, status);
            }
            let tick_started = Instant::now();
            let ms = self.tick();
            state.store(Arc::new(ms));
            self.publish_status(status);

            // Sleep the remainder of the tick; the renderer reads whatever is newest.
            let spent = tick_started.elapsed();
            if spent < TICK {
                std::thread::sleep(TICK.saturating_sub(spent));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};
    use std::time::Duration;

    #[test]
    fn track_label_drops_the_dash_without_an_artist() {
        assert_eq!(track_label("Adam Port", "Move"), "Adam Port — Move");
        assert_eq!(track_label("  ", "HORN"), "HORN");
    }

    #[test]
    fn status_lines_say_what_is_missing() {
        assert_eq!(
            EngineStatus::Waiting("waiting for rekordbox".into()).line(),
            "waiting for rekordbox"
        );
        assert_eq!(
            EngineStatus::Running {
                source: "rekordbox".into(),
                track: "A - B".into()
            }
            .line(),
            "rekordbox: A - B"
        );
    }

    fn fixture_root() -> Option<PathBuf> {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/private");
        root.join("master.db").exists().then_some(root)
    }

    /// The private fixtures, when present together with the audio file of the demo track.
    fn fixtures() -> Option<PathBuf> {
        let root = fixture_root()?;
        let paths = RekordboxPaths::from_app_dir(&root).ok()?;
        let cache = tempfile::tempdir().ok()?;
        let library = Library::open(&paths, cache.path()).ok()?;
        let meta = library.find_by_title_artist("Move", "")?;
        meta.file_path
            .as_ref()
            .is_some_and(|p| p.exists())
            .then_some(root)
    }

    fn config(app_dir: PathBuf, cache: &Path, sim_track: Option<&str>) -> EngineConfig {
        EngineConfig {
            app_dir: Some(app_dir),
            cache_dir: cache.to_path_buf(),
            offsets_dir: cache.join("no-offsets-here"),
            sim_track: sim_track.map(str::to_string),
            sim_seek_s: 30.0,
            sim_gain: 0.0,
            audio_device: None,
            capture_audio: false,
        }
    }

    #[test]
    fn without_a_simulator_the_engine_waits_for_rekordbox_and_plays_nothing() {
        let Some(app_dir) = fixture_root() else {
            eprintln!("skipping: fixtures missing");
            return;
        };
        let cache = tempfile::tempdir().unwrap();
        let engine = Engine::start(config(app_dir, cache.path(), None)).expect("engine");
        assert!(!engine.is_simulator());
        std::thread::sleep(Duration::from_millis(400));
        match engine.status() {
            EngineStatus::Waiting(why) => assert!(
                why.contains("rekordbox"),
                "the reason names rekordbox: {why}"
            ),
            other => panic!("expected Waiting, got {other:?}"),
        }
        let s = engine.state();
        assert!(!s.playing && s.track.is_none(), "nothing plays on its own");
    }

    #[test]
    fn resolve_track_by_analysis_path_and_by_title() {
        let Some(app_dir) = fixture_root() else {
            eprintln!("skipping: fixtures missing");
            return;
        };
        let paths = RekordboxPaths::from_app_dir(&app_dir).unwrap();
        let cache = tempfile::tempdir().unwrap();
        let library = Library::open(&paths, cache.path()).unwrap();
        let by_title = resolve_track(
            &library,
            &TrackRef::TitleArtist {
                title: "Move".into(),
                artist: "Adam Port".into(),
                album: String::new(),
            },
        )
        .expect("title match");
        let rel = by_title.analysis_path.clone().expect("analysis path");
        let by_path = resolve_track(&library, &TrackRef::AnalysisPath(rel)).expect("path match");
        assert_eq!(by_path.id, by_title.id);
        assert!(resolve_track(&library, &TrackRef::Unknown).is_none());
    }

    #[test]
    fn simulator_engine_publishes_live_state() {
        let Some(app_dir) = fixtures() else {
            eprintln!("skipping: fixtures or track missing");
            return;
        };
        let cache = tempfile::tempdir().unwrap();
        let Ok(engine) = Engine::start(config(app_dir, cache.path(), Some("Move"))) else {
            eprintln!("skipping: no audio device");
            return;
        };
        assert!(engine.is_simulator());
        std::thread::sleep(Duration::from_millis(600));
        let a = engine.state();
        assert!(a.playing, "{a:?}");
        assert!((a.bpm - 120.0).abs() < 0.1, "bpm {}", a.bpm);
        assert!(
            a.playhead_s >= 30.0 && a.playhead_s < 32.0,
            "{}",
            a.playhead_s
        );
        assert!(a.phrase.is_some(), "structure must be known for Move");
        assert!(a.track.as_ref().is_some_and(|t| t.title == "Move"));

        engine.command(EngineCommand::Pause);
        std::thread::sleep(Duration::from_millis(150));
        let b = engine.state();
        std::thread::sleep(Duration::from_millis(150));
        let c = engine.state();
        assert!(!b.playing);
        assert!(
            (c.playhead_s - b.playhead_s).abs() < 1e-3,
            "paused playhead moved"
        );
        assert!(matches!(engine.status(), EngineStatus::Running { .. }));
    }
}
