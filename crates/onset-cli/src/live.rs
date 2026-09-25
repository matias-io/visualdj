//! `sim`, `devices` and `listen`: the live pipeline without a renderer.
use std::path::PathBuf;
use std::time::{Duration, Instant};

use onset_audio::analyzer::Analyzer;
use onset_audio::capture::{LoopbackCapture, list_endpoints};
use onset_core::audio_features::AudioFeatures;
use onset_core::clock::Clock;
use onset_core::director::Director;
use onset_core::structure::Structure;
use onset_core::transport::TrackRef;
use onset_transport::sim::SimPlayer;
use onset_transport::source::TransportSource;

use crate::common::{Meter, open_library, quit_signal};

const TICK: Duration = Duration::from_millis(33);

fn fmt_time(seconds: f64) -> String {
    let total_tenths = (seconds.max(0.0) * 10.0).round() as u64;
    format!(
        "{:02}:{:02}.{}",
        total_tenths / 600,
        (total_tenths / 10) % 60,
        total_tenths % 10
    )
}

pub fn sim(
    app_dir: Option<PathBuf>,
    title: &str,
    seek: f64,
    gain: f32,
    analyze: bool,
    seconds: f64,
) -> anyhow::Result<()> {
    let (paths, lib) = open_library(app_dir)?;
    let track = lib
        .find_by_title_artist(title, "")
        .ok_or_else(|| anyhow::anyhow!("no track titled {title:?}"))?
        .clone();
    let file = track
        .file_path
        .as_ref()
        .filter(|p| p.exists())
        .ok_or_else(|| anyhow::anyhow!("audio file for {title:?} not found on disk"))?;
    let rel = track
        .analysis_path
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("{title:?} has no analysis file"))?;
    let analysis = onset_rekordbox::anlz::load_analysis(&paths, rel)?;
    let cues = lib.cues(track.id);
    let bpm = track.bpm.unwrap_or(120.0);

    let mut player = SimPlayer::start(file, TrackRef::Id(track.id), bpm)?;
    player.set_gain(gain);
    player.seek(seek);
    let tap = player.tap_audio();
    let mut analyzer = analyze.then(|| Analyzer::new(player.tap_sample_rate()));
    let mut meter = Meter::new();
    let mut last_audio = AudioFeatures::silent();

    let structure = Structure::new(&analysis.grid, analysis.phrases.as_ref(), &cues);
    let mut clock = Clock::new();
    let mut director = Director::new();
    let quit = quit_signal();
    let started = Instant::now();
    let mut last_tick = started;

    eprintln!(
        "{} - {}  ({bpm} BPM, {} phrases)   type q + Enter to stop",
        track.artist,
        track.title,
        analysis.phrases.as_ref().map_or(0, |p| p.phrases.len())
    );

    loop {
        if quit.try_recv().is_ok() {
            break;
        }
        if seconds > 0.0 && started.elapsed().as_secs_f64() >= seconds {
            break;
        }
        if let Some(snapshot) = player.poll() {
            clock.observe(&snapshot);
        }
        let now = Instant::now();
        let dt = now.duration_since(last_tick).as_secs_f32();
        last_tick = now;

        let playhead = clock.playhead_at(now).unwrap_or(0.0);
        if track.duration_s.is_some_and(|d| playhead >= f64::from(d)) {
            break;
        }
        let st = structure.at(playhead * 1000.0);
        let intensity = director.update(&st, dt);

        if let Some(an) = analyzer.as_mut() {
            while let Ok(block) = tap.try_recv() {
                if let Some(f) = an.push(&block) {
                    last_audio = f;
                }
            }
        }

        let beat = st
            .beat_index
            .map_or("   -".to_string(), |b| format!("{b:>4}"));
        let phrase = st.phrase_label.clone().unwrap_or_else(|| "-".to_string());
        let next = st
            .beats_to_next_phrase
            .map_or("  -".to_string(), |b| format!("{b:>3}"));
        let drop = st
            .drop_countdown_beats
            .map_or("  -".to_string(), |b| format!("{b:>3}"));
        let cue = st.next_cue.as_ref().map_or(String::new(), |(c, s)| {
            format!("  cue {} in {s:4.1}s", c.name)
        });
        let audio = if analyzer.is_some() {
            format!(
                "  {} rms {:.2}{}",
                meter.render(&last_audio.bands),
                last_audio.rms,
                if last_audio.onset { " *" } else { "  " }
            )
        } else {
            String::new()
        };
        eprint!(
            "\r{}  beat {beat}  ph {:.2}  bar {:.2}  {phrase:<9} next {next}  drop {drop}  int {intensity:.2}{cue}{audio}   ",
            fmt_time(playhead),
            st.beat_phase,
            st.bar_phase
        );
        std::thread::sleep(TICK);
    }
    eprintln!();
    Ok(())
}

pub fn devices() {
    for name in list_endpoints() {
        println!("{name}");
    }
}

pub fn listen(device: Option<&str>, seconds: f64) -> anyhow::Result<()> {
    let (_capture, rx, sample_rate) = LoopbackCapture::start(device)?;
    let mut analyzer = Analyzer::new(sample_rate);
    let mut meter = Meter::new();
    let quit = quit_signal();
    let started = Instant::now();
    let mut last_print = Instant::now();
    eprintln!("capturing at {sample_rate} Hz   type q + Enter to stop");
    loop {
        if quit.try_recv().is_ok() {
            break;
        }
        if seconds > 0.0 && started.elapsed().as_secs_f64() >= seconds {
            break;
        }
        match rx.recv_timeout(Duration::from_millis(200)) {
            Ok(block) => {
                if let Some(f) = analyzer.push(&block)
                    && last_print.elapsed() >= Duration::from_millis(50)
                {
                    last_print = Instant::now();
                    eprint!(
                        "\r{} rms {:.3} {}{}   ",
                        meter.render(&f.bands),
                        f.rms,
                        if f.onset { "ONSET" } else { "     " },
                        if f.silent { " silent" } else { "       " }
                    );
                }
            }
            Err(crossbeam_channel::RecvTimeoutError::Timeout) => {
                eprint!("\r(no audio blocks arriving)                                   ");
            }
            Err(crossbeam_channel::RecvTimeoutError::Disconnected) => break,
        }
    }
    eprintln!();
    Ok(())
}
