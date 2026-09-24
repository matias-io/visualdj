use std::path::Path;

use onset_core::transport::TrackRef;
use onset_transport::decode::decode_file;
use onset_transport::sim::SimPlayer;
use onset_transport::source::{SourceStatus, TransportSource};

const MOVE: &str = r"C:\Users\Reima\Music\DJ\Adam Port - Move.flac";

#[test]
fn decodes_move_flac() {
    let path = Path::new(MOVE);
    if !path.exists() {
        eprintln!("skipping: track not present");
        return;
    }
    let d = decode_file(path).unwrap();
    assert_eq!(d.channels, 2);
    assert!(
        d.sample_rate == 44_100 || d.sample_rate == 48_000,
        "{}",
        d.sample_rate
    );
    let secs = d.samples.len() as f64 / f64::from(d.channels) / f64::from(d.sample_rate);
    assert!((secs - 177.0).abs() < 2.0, "Move is 2:57, got {secs}");
}

#[test]
fn sim_reports_advancing_playhead() {
    let path = Path::new(MOVE);
    if !path.exists() {
        return;
    }
    let Ok(mut p) = SimPlayer::start(path, TrackRef::Unknown, 120.0) else {
        eprintln!("skipping: no output device");
        return;
    };
    assert_eq!(p.status(), SourceStatus::Connected);
    p.set_gain(0.05);
    p.seek(60.0);
    std::thread::sleep(std::time::Duration::from_millis(400));
    let a = p.poll().unwrap();
    std::thread::sleep(std::time::Duration::from_millis(400));
    let b = p.poll().unwrap();
    let advanced = b.playhead_s - a.playhead_s;
    assert!(
        (0.3..0.5).contains(&advanced),
        "{} -> {}",
        a.playhead_s,
        b.playhead_s
    );
    assert!(a.playing);
    assert!((a.bpm_now - 120.0).abs() < 1e-3);
    assert!(
        a.playhead_s >= 60.0 && a.playhead_s < 61.5,
        "{}",
        a.playhead_s
    );
}

#[test]
fn sim_pause_and_rate() {
    let path = Path::new(MOVE);
    if !path.exists() {
        return;
    }
    let Ok(mut p) = SimPlayer::start(path, TrackRef::Unknown, 120.0) else {
        return;
    };
    p.set_gain(0.0);
    p.pause(true);
    let a = p.poll().unwrap();
    std::thread::sleep(std::time::Duration::from_millis(200));
    let b = p.poll().unwrap();
    assert!(!a.playing);
    assert!(
        (b.playhead_s - a.playhead_s).abs() < 1e-6,
        "paused playhead moved"
    );

    p.set_rate(1.05);
    let c = p.poll().unwrap();
    assert!((c.bpm_now - 126.0).abs() < 1e-3, "{}", c.bpm_now);
}

#[test]
fn sim_taps_mono_audio_blocks() {
    let path = Path::new(MOVE);
    if !path.exists() {
        return;
    }
    let Ok(p) = SimPlayer::start(path, TrackRef::Unknown, 120.0) else {
        return;
    };
    p.set_gain(0.0);
    p.seek(30.0);
    let rx = p.tap_audio();
    let block = rx
        .recv_timeout(std::time::Duration::from_secs(2))
        .expect("audio block within 2 s");
    assert_eq!(block.len(), 256);
    assert!(
        block.iter().any(|s| s.abs() > 1e-4),
        "block should carry signal"
    );
}
