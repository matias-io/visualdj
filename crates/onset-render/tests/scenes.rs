//! Every built-in scene renders headlessly, reacts to the beat, and shows something calm in
//! the idle state (no track, silent audio).
use onset_core::audio_features::AudioFeatures;
use onset_core::music_state::MusicState;
use onset_core::phrase::PhraseKind;
use onset_render::headless::{Headless, hash_rgba};
use onset_render::scene::FrameBindings;
use onset_render::scenes::{BUILTIN_SCENE_NAMES, builtin_scenes};
use onset_render::uniforms::FrameUniforms;

const SIZE: (u32, u32) = (320, 180);

fn shader_dir() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../assets/shaders")
}

fn render(
    h: &Headless,
    scene: &mut dyn onset_render::scene::Scene,
    b: &FrameBindings,
    ms: &MusicState,
) -> Vec<u8> {
    b.write(
        &h.gpu,
        &FrameUniforms::from_state(ms, SIZE, ms.time_s as f32),
    );
    h.render_with(|gpu, view| {
        let mut enc = gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("scene"),
            });
        scene.render(gpu, &mut enc, view, b);
        gpu.queue.submit([enc.finish()]);
    })
}

/// Standard deviation of luminance over the frame, in 0..255 units.
fn luminance_spread(px: &[u8]) -> f64 {
    let lum: Vec<f64> = px
        .as_chunks::<4>()
        .0
        .iter()
        .map(|p| 0.2126 * f64::from(p[0]) + 0.7152 * f64::from(p[1]) + 0.0722 * f64::from(p[2]))
        .collect();
    let mean = lum.iter().sum::<f64>() / lum.len() as f64;
    (lum.iter().map(|l| (l - mean).powi(2)).sum::<f64>() / lum.len() as f64).sqrt()
}

fn playing_state(beat_phase: f32) -> MusicState {
    let mut audio = AudioFeatures::silent();
    audio.silent = false;
    audio.rms = 0.3;
    for (i, b) in audio.bands.iter_mut().enumerate() {
        *b = 0.6 * (1.0 - i as f32 / 24.0);
    }
    MusicState {
        time_s: 12.0,
        playhead_s: 40.0,
        playing: true,
        bpm: 124.0,
        bpm_grid: 124.0,
        beat_phase,
        bar_phase: 0.3,
        phrase_phase: 0.5,
        phrase: Some(PhraseKind::Up),
        next_phrase: Some(PhraseKind::Chorus),
        beats_to_next_phrase: Some(12),
        drop_countdown_beats: Some(12),
        intensity: 0.7,
        audio,
        ..MusicState::default()
    }
}

#[test]
fn every_builtin_scene_loads() {
    let h = Headless::new(SIZE, true).expect("adapter");
    let b = FrameBindings::new(&h.gpu);
    let scenes =
        builtin_scenes(&h.gpu, h.format, &b.layout, &shader_dir()).expect("all shaders compile");
    let names: Vec<&str> = scenes.iter().map(|s| s.name()).collect();
    assert_eq!(names, BUILTIN_SCENE_NAMES);
    assert!(names.len() >= 4, "{names:?}");
}

#[test]
fn scenes_react_to_the_beat() {
    let h = Headless::new(SIZE, true).expect("adapter");
    let b = FrameBindings::new(&h.gpu);
    let mut scenes = builtin_scenes(&h.gpu, h.format, &b.layout, &shader_dir()).unwrap();
    for scene in &mut scenes {
        let a = render(&h, scene.as_mut(), &b, &playing_state(0.05));
        let c = render(&h, scene.as_mut(), &b, &playing_state(0.9));
        assert_ne!(
            hash_rgba(&a),
            hash_rgba(&c),
            "{} ignores beat_phase",
            scene.name()
        );
    }
}

#[test]
fn scenes_render_idle_state() {
    let h = Headless::new(SIZE, true).expect("adapter");
    let b = FrameBindings::new(&h.gpu);
    let mut scenes = builtin_scenes(&h.gpu, h.format, &b.layout, &shader_dir()).unwrap();
    for scene in &mut scenes {
        let px = render(&h, scene.as_mut(), &b, &MusicState::default());
        assert!(
            px.iter().any(|v| *v != 0),
            "{} rendered all zeros",
            scene.name()
        );
        assert!(
            px.as_chunks::<4>().0.iter().all(|p| p[3] == 255),
            "{} produced transparent or NaN pixels",
            scene.name()
        );
        let spread = luminance_spread(&px);
        assert!(
            spread > 2.0,
            "{} is a flat colour when idle (spread {spread:.2})",
            scene.name()
        );
    }
}
