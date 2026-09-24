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
}

#[derive(Debug, Clone, PartialEq)]
pub enum EngineStatus {
    Starting,
    /// No usable source yet; the text says what is missing ("waiting for rekordbox").
    Waiting(String),
    /// A source is connected but no track is loaded on the master deck.
    Idle,
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
    _thread: std::thread::JoinHandle<()>,
}

/// The track the master deck holds, with everything the structure engine needs.
struct Current {
    meta: TrackMeta,
    analysis: Option<Analysis>,
    cues: Vec<HotCue>,
    /// How the source referred to it, so a repeat read is not a reload.
    track_ref: TrackRef,
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
    _stream: LoopbackCapture,
    rx: crossbeam_channel::Receiver<Vec<f32>>,
    analyzer: Analyzer,
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
    }
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

        let (source, current) = match &cfg.sim_track {
            Some(title) => {
                let (sim, current) = start_sim(&library, &paths, title, &cfg)?;
                (Source::Sim(Box::new(sim)), Some(current))
            }
            None => (Source::Live(live_source(&cfg)), None),
        };
        let simulator = matches!(source, Source::Sim(_));
        let capture = if cfg.capture_audio && !simulator {
            match LoopbackCapture::start(cfg.audio_device.as_deref()) {
                Ok((cap, rx, rate)) => Some(Capture {
                    _stream: cap,
                    rx,
                    analyzer: Analyzer::new(rate),
                }),
                Err(e) => {
                    tracing::warn!("loopback capture unavailable: {e:#}");
                    None
                }
            }
        } else {
            None
        };

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
                };
                engine.run(&rx, &state_w, &status_w);
            })?;
        Ok(Self {
            state,
            status,
            commands: tx,
            tracks,
            simulator,
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
}

impl Running {
    fn status_now(&self) -> EngineStatus {
        match (self.source.status(), &self.current) {
            (SourceStatus::Searching, _) => EngineStatus::Waiting("waiting for rekordbox".into()),
            (SourceStatus::Unsupported(why) | SourceStatus::Error(why), _) => {
                EngineStatus::Waiting(why)
            }
            (SourceStatus::Connected, Some(c)) => EngineStatus::Running {
                source: self.source.name().to_string(),
                track: format!("{} - {}", c.meta.artist, c.meta.title),
            },
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
            (_, Source::Live(_)) => {
                tracing::debug!(?cmd, "transport command ignored: rekordbox is in control");
            }
        }
    }

    /// A live snapshot names a track; keep `current` in step with it.
    fn follow_track(&mut self, snapshot: &TransportSnapshot) {
        let same = self
            .current
            .as_ref()
            .is_some_and(|c| c.track_ref == snapshot.track);
        if same || snapshot.track == TrackRef::Unknown {
            return;
        }
        if let Some(meta) = resolve_track(&self.library, &snapshot.track) {
            tracing::info!(title = %meta.title, artist = %meta.artist, "master deck track");
            self.current = Some(load_current(
                &self.library,
                &self.paths,
                meta,
                snapshot.track.clone(),
            ));
            self.clock = Clock::new();
            return;
        }
        tracing::warn!(track = ?snapshot.track, "master deck track not in the library");
        if self.current.is_some() {
            self.current = None;
            self.clock = Clock::new();
        }
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

        let Some(current) = self.current.as_ref() else {
            return MusicState {
                time_s,
                ..MusicState::default()
            };
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
        let intensity = self.director.update(&st, dt);
        let audio = self.audio(now);
        MusicState::assemble(
            time_s,
            playhead,
            self.clock.is_playing(),
            self.clock.rate(),
            &st,
            audio,
            intensity,
            Some(meta),
        )
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
