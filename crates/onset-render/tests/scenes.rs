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
        // Phrases come from the beat grid, so a playing state with phrases has a beat index.
        beat_index: Some(82),
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
    let loaded = builtin_scenes(&h.gpu, h.format, &b.layout, &shader_dir());
    assert!(loaded.errors.is_empty(), "{:?}", loaded.errors);
    let scenes = loaded.scenes;
    let names: Vec<&str> = scenes.iter().map(|s| s.name()).collect();
    assert_eq!(names, BUILTIN_SCENE_NAMES);
    assert!(names.len() >= 4, "{names:?}");
}

#[test]
fn scenes_react_to_the_beat() {
    let h = Headless::new(SIZE, true).expect("adapter");
    let b = FrameBindings::new(&h.gpu);
    let mut scenes = builtin_scenes(&h.gpu, h.format, &b.layout, &shader_dir()).scenes;
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
    let mut scenes = builtin_scenes(&h.gpu, h.format, &b.layout, &shader_dir()).scenes;
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

fn copy_shaders_to(dir: &std::path::Path) {
    for entry in std::fs::read_dir(shader_dir()).unwrap().flatten() {
        std::fs::copy(entry.path(), dir.join(entry.file_name())).unwrap();
    }
}

#[test]
fn broken_builtin_shader_is_replaced_by_the_embedded_copy_and_reported() {
    let h = Headless::new(SIZE, true).expect("adapter");
    let b = FrameBindings::new(&h.gpu);
    let dir = tempfile::tempdir().unwrap();
    copy_shaders_to(dir.path());
    std::fs::write(dir.path().join("ring.wgsl"), "fn broken(").unwrap();

    let loaded = builtin_scenes(&h.gpu, h.format, &b.layout, dir.path());
    let names: Vec<&str> = loaded.scenes.iter().map(|s| s.name()).collect();
    assert_eq!(names, BUILTIN_SCENE_NAMES, "every scene is still available");
    assert_eq!(loaded.errors.len(), 1);
    assert_eq!(loaded.errors[0].name, "ring");
}

#[test]
fn missing_shader_files_fall_back_to_embedded_sources() {
    let h = Headless::new(SIZE, true).expect("adapter");
    let b = FrameBindings::new(&h.gpu);
    let dir = tempfile::tempdir().unwrap();
    let loaded = builtin_scenes(&h.gpu, h.format, &b.layout, dir.path());
    assert_eq!(loaded.scenes.len(), BUILTIN_SCENE_NAMES.len());
    assert!(
        loaded.errors.is_empty(),
        "a missing file is not a shader error"
    );
}

fn distinct_colours(px: &[u8]) -> usize {
    let mut set = std::collections::HashSet::new();
    for p in px.as_chunks::<4>().0 {
        set.insert([p[0] >> 2, p[1] >> 2, p[2] >> 2]);
    }
    set.len()
}

#[test]
fn warp_keeps_its_detail_after_hours_of_runtime() {
    let h = Headless::new(SIZE, true).expect("adapter");
    let b = FrameBindings::new(&h.gpu);
    let mut scenes = builtin_scenes(&h.gpu, h.format, &b.layout, &shader_dir()).scenes;
    let warp = scenes
        .iter_mut()
        .find(|s| s.name() == "warp")
        .expect("warp scene");
    let mut early = playing_state(0.3);
    early.time_s = 10.0;
    let mut late = playing_state(0.3);
    late.time_s = 20_000.0;
    let a = distinct_colours(&render(&h, warp.as_mut(), &b, &early));
    let c = distinct_colours(&render(&h, warp.as_mut(), &b, &late));
    assert!(
        c as f64 > 0.6 * a as f64,
        "warp collapsed from {a} to {c} distinct colours after 20000 s"
    );
}
