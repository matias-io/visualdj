//! The engine thread: polls the transport, drives the clock, structure, analyzer and
//! director at 120 Hz, and publishes one `MusicState` for the renderer to read each frame.
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use arc_swap::ArcSwap;
use onset_audio::analyzer::Analyzer;
use onset_audio::capture::LoopbackCapture;
use onset_core::clock::Clock;
use onset_core::director::Director;
use onset_core::music_state::MusicState;
use onset_core::structure::Structure;
use onset_core::track::{HotCue, TrackMeta};
use onset_core::transport::TrackRef;
use onset_rekordbox::anlz::Analysis;
use onset_rekordbox::library::Library;
use onset_rekordbox::paths::RekordboxPaths;
use onset_transport::sim::SimPlayer;
use onset_transport::source::TransportSource;

const TICK: Duration = Duration::from_micros(1_000_000 / 120);

pub struct EngineConfig {
    /// rekordbox data folder; `None` discovers `%APPDATA%\Pioneer\rekordbox`.
    pub app_dir: Option<PathBuf>,
    pub cache_dir: PathBuf,
    /// Title of the track the simulator plays; `None` starts idle.
    pub sim_track: Option<String>,
    pub sim_seek_s: f64,
    pub sim_gain: f32,
    /// Loopback endpoint substring; default output device when `None`.
    pub audio_device: Option<String>,
    /// Capture the system mix (rekordbox) instead of tapping the simulator's audio.
    pub capture_audio: bool,
}

#[derive(Debug, Clone)]
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
    Idle,
    Running { source: String, track: String },
    Error(String),
}

pub struct Engine {
    state: Arc<ArcSwap<MusicState>>,
    status: Arc<ArcSwap<EngineStatus>>,
    commands: crossbeam_channel::Sender<EngineCommand>,
    _thread: std::thread::JoinHandle<()>,
}

struct Loaded {
    meta: TrackMeta,
    analysis: Analysis,
    cues: Vec<HotCue>,
    player: SimPlayer,
    tap: crossbeam_channel::Receiver<Vec<f32>>,
    analyzer: Analyzer,
}

fn load_track(
    library: &Library,
    paths: &RekordboxPaths,
    title: &str,
    cfg: &EngineConfig,
) -> anyhow::Result<Loaded> {
    let meta = library
        .find_by_title_artist(title, "")
        .ok_or_else(|| anyhow::anyhow!("no track titled {title:?}"))?
        .clone();
    let file = meta
        .file_path
        .clone()
        .filter(|p| p.exists())
        .ok_or_else(|| anyhow::anyhow!("audio file for {title:?} not found"))?;
    let rel = meta
        .analysis_path
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("{title:?} has no analysis file"))?;
    let analysis = onset_rekordbox::anlz::load_analysis(paths, rel)?;
    let cues = library.cues(meta.id);
    let player = SimPlayer::start(&file, TrackRef::Id(meta.id), meta.bpm.unwrap_or(120.0))?;
    player.set_gain(cfg.sim_gain);
    player.seek(cfg.sim_seek_s);
    let tap = player.tap_audio();
    let analyzer = Analyzer::new(player.tap_sample_rate());
    Ok(Loaded {
        meta,
        analysis,
        cues,
        player,
        tap,
        analyzer,
    })
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
        let initial = match &cfg.sim_track {
            Some(title) => Some(load_track(&library, &paths, title, &cfg)?),
            None => None,
        };
        let capture = if cfg.capture_audio {
            match LoopbackCapture::start(cfg.audio_device.as_deref()) {
                Ok((cap, rx, rate)) => Some((cap, rx, Analyzer::new(rate))),
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
                run(
                    &cfg, &paths, &library, initial, capture, &rx, &state_w, &status_w,
                );
            })?;
        Ok(Self {
            state,
            status,
            commands: tx,
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

    pub fn command(&self, cmd: EngineCommand) {
        let _ = self.commands.send(cmd);
    }
}

#[allow(clippy::too_many_arguments)]
fn run(
    cfg: &EngineConfig,
    paths: &RekordboxPaths,
    library: &Library,
    mut loaded: Option<Loaded>,
    mut capture: Option<(
        LoopbackCapture,
        crossbeam_channel::Receiver<Vec<f32>>,
        Analyzer,
    )>,
    rx: &crossbeam_channel::Receiver<EngineCommand>,
    state: &ArcSwap<MusicState>,
    status: &ArcSwap<EngineStatus>,
) {
    let started = Instant::now();
    let mut clock = Clock::new();
    let mut director = Director::new();
    let mut last_tick = started;

    let publish_status = |loaded: &Option<Loaded>| {
        let s = match loaded {
            Some(l) => EngineStatus::Running {
                source: l.player.name().to_string(),
                track: format!("{} - {}", l.meta.artist, l.meta.title),
            },
            None => EngineStatus::Idle,
        };
        status.store(Arc::new(s));
    };
    publish_status(&loaded);

    loop {
        while let Ok(cmd) = rx.try_recv() {
            match (&cmd, loaded.as_mut()) {
                (EngineCommand::Pause, Some(l)) => l.player.pause(true),
                (EngineCommand::Resume, Some(l)) => l.player.pause(false),
                (EngineCommand::Seek(s), Some(l)) => l.player.seek(*s),
                (EngineCommand::SetRate(r), Some(l)) => l.player.set_rate(*r),
                (EngineCommand::LoadTrack(title), _) => {
                    match load_track(library, paths, title, cfg) {
                        Ok(l) => {
                            loaded = Some(l);
                            clock = Clock::new();
                            publish_status(&loaded);
                        }
                        Err(e) => {
                            tracing::error!("load failed: {e:#}");
                            status.store(Arc::new(EngineStatus::Error(format!("{e:#}"))));
                        }
                    }
                }
                _ => {}
            }
        }

        let now = Instant::now();
        let dt = now.duration_since(last_tick).as_secs_f32();
        last_tick = now;
        let time_s = started.elapsed().as_secs_f64();

        let ms = if let Some(l) = loaded.as_mut() {
            if let Some(snapshot) = l.player.poll() {
                clock.observe(&snapshot);
            }
            let playhead = clock.playhead_at(now).unwrap_or(0.0);
            let structure = Structure::new(&l.analysis.grid, l.analysis.phrases.as_ref(), &l.cues);
            let st = structure.at(playhead * 1000.0);
            let intensity = director.update(&st, dt);

            let audio = if let Some((_, rx, an)) = capture.as_mut() {
                while let Ok(block) = rx.try_recv() {
                    an.push(&block);
                }
                an.latest(now)
            } else {
                while let Ok(block) = l.tap.try_recv() {
                    l.analyzer.push_at(&block, now);
                }
                l.analyzer.latest(now)
            };

            MusicState::assemble(
                time_s,
                playhead,
                clock.is_playing(),
                clock.rate(),
                &st,
                audio,
                intensity,
                Some(l.meta.clone()),
            )
        } else {
            MusicState {
                time_s,
                ..MusicState::default()
            }
        };
        state.store(Arc::new(ms));

        // Sleep the remainder of the tick; the renderer reads whatever is newest.
        let spent = Instant::now().duration_since(now);
        if spent < TICK {
            std::thread::sleep(TICK.saturating_sub(spent));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::time::Duration;

    fn fixtures() -> Option<PathBuf> {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/private");
        (root.join("master.db").exists()
            && std::path::Path::new(r"C:\Users\Reima\Music\DJ\Adam Port - Move.flac").exists())
        .then_some(root)
    }

    #[test]
    fn simulator_engine_publishes_live_state() {
        let Some(app_dir) = fixtures() else {
            eprintln!("skipping: fixtures or track missing");
            return;
        };
        let cache = tempfile::tempdir().unwrap();
        let Ok(engine) = Engine::start(EngineConfig {
            app_dir: Some(app_dir),
            cache_dir: cache.path().to_path_buf(),
            sim_track: Some("Move".into()),
            sim_seek_s: 30.0,
            sim_gain: 0.0,
            audio_device: None,
            capture_audio: false,
        }) else {
            eprintln!("skipping: no audio device");
            return;
        };
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
