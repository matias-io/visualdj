//! Every scene through the whole stage (bloom, effects, transitions), driven by a synthetic
//! build-up and drop. Each scene must show structure at idle, respond to the drop, and
//! respond to the bass separately from the treble.
//!
//! Set `ONSET_DUMP_DIR` to save 960x540 frames of every scene (idle, build-up, drop,
//! chorus) for looking at; `ONSET_GALLERY_ART` points at a cover to use for the palette.
use onset_core::audio_features::{AudioFeatures, BANDS};
use onset_core::music_state::MusicState;
use onset_core::phrase::{Mood, PhraseKind};
use onset_core::show::AutoChange;
use onset_core::track::{TrackId, TrackMeta};
use onset_render::headless::Headless;
use onset_render::renderer::{Quality, RenderSettings, Renderer};
use onset_render::scenes::{builtin_scenes, scene_info};

const FPS: f32 = 30.0;
const BPM: f32 = 128.0;

fn shader_dir() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../assets/shaders")
}

fn track() -> TrackMeta {
    TrackMeta {
        id: TrackId(42),
        title: "Gallery".into(),
        artist: "Onset".into(),
        album: String::new(),
        year: None,
        key: Some("8A".into()),
        bpm: Some(BPM),
        duration_s: Some(240.0),
        sample_rate: Some(44_100),
        file_path: None,
        artwork_path: std::env::var_os("ONSET_GALLERY_ART").map(Into::into),
        analysis_path: None,
        isrc: None,
        genre: Some("Techno".into()),
    }
}

/// Four-on-the-floor with hats on the offbeat and a snare on 2 and 4. `bass` and `treble`
/// scale those halves of the spectrum.
fn audio(t: f32, bass: f32, treble: f32) -> AudioFeatures {
    let beat = t * BPM / 60.0;
    let phase = beat.fract();
    let kick = (1.0 - phase).powf(6.0);
    let hat = (1.0 - (phase + 0.5).fract()).powf(10.0);
    let snare = if (beat as u32) % 2 == 1 { (1.0 - phase).powf(8.0) } else { 0.0 };
    let mut a = AudioFeatures::silent();
    a.silent = false;
    for (i, l) in a.levels.iter_mut().enumerate() {
        let x = i as f32 / (BANDS - 1) as f32;
        let wobble = 0.5 + 0.5 * (t * 1.7 + x * 9.0).sin();
        *l = if x < 0.3 {
            bass * (0.4 + 0.6 * kick)
        } else if x < 0.7 {
            0.3 + 0.4 * wobble * (0.5 + snare)
        } else {
            treble * (0.3 + 0.7 * hat) * wobble
        }
        .clamp(0.0, 1.0);
    }
    a.bands = a.levels;
    for (g, (lo, hi)) in a
        .groups
        .iter_mut()
        .zip([(0, 3), (3, 6), (6, 10), (10, 14), (14, 18), (18, 24)])
    {
        *g = a.levels[lo..hi].iter().sum::<f32>() / (hi - lo) as f32;
    }
    a.kick = kick * bass;
    a.snare = snare;
    a.hat = hat * treble;
    a.loudness = 0.5 + 0.4 * bass;
    a.brightness = 0.3 + 0.4 * treble;
    a.rms = 0.3;
    a
}

/// The state at `t` seconds: a build-up until `drop_at`, then the chorus.
fn state(t: f32, drop_at: f32, bass: f32, treble: f32) -> MusicState {
    let beats = t * BPM / 60.0;
    let chorus = t >= drop_at;
    let beats_to_drop = ((drop_at - t) * BPM / 60.0).ceil().max(0.0) as u32;
    MusicState {
        time_s: f64::from(t),
        playhead_s: f64::from(t) + 60.0,
        playing: true,
        bpm: BPM,
        bpm_grid: BPM,
        beat_phase: beats.fract(),
        bar_phase: (beats / 4.0).fract(),
        phrase_phase: (beats / 32.0).fract(),
        phrase: Some(if chorus { PhraseKind::Chorus } else { PhraseKind::Up }),
        next_phrase: Some(if chorus { PhraseKind::Down } else { PhraseKind::Chorus }),
        beats_to_next_phrase: Some(if chorus { 32 } else { beats_to_drop }),
        drop_countdown_beats: (!chorus).then_some(beats_to_drop),
        intensity: if chorus { 1.0 } else { 0.55 + 0.25 * (t / drop_at) },
        audio: audio(t, bass, treble),
        track: Some(track()),
        beat_index: Some(128 + beats as u32),
        mood: Some(Mood::High),
        ..MusicState::default()
    }
}

fn luma_stats(px: &[u8]) -> (f64, f64) {
    let lum: Vec<f64> = px
        .as_chunks::<4>()
        .0
        .iter()
        .map(|p| 0.2126 * f64::from(p[0]) + 0.7152 * f64::from(p[1]) + 0.0722 * f64::from(p[2]))
        .collect();
    let mean = lum.iter().sum::<f64>() / lum.len() as f64;
    let spread = (lum.iter().map(|l| (l - mean).powi(2)).sum::<f64>() / lum.len() as f64).sqrt();
    (mean, spread)
}

fn diff(a: &[u8], b: &[u8]) -> f64 {
    a.iter()
        .zip(b)
        .map(|(x, y)| f64::from((i16::from(*x) - i16::from(*y)).unsigned_abs()))
        .sum::<f64>()
        / a.len() as f64
}

struct Gallery {
    h: Headless,
    r: Renderer,
    dump: Option<std::path::PathBuf>,
}

impl Gallery {
    fn new() -> Self {
        let dump = std::env::var_os("ONSET_DUMP_DIR").map(std::path::PathBuf::from);
        let size = if dump.is_some() { (960, 540) } else { (192, 108) };
        let h = Headless::new(size, dump.is_none()).expect("adapter");
        let mut r = Renderer::new(&h.gpu, size, h.format);
        let loaded = builtin_scenes(&h.gpu, r.scene_format(), &r.bindings().layout, &shader_dir());
        assert!(loaded.errors.is_empty(), "{:?}", loaded.errors);
        for s in loaded.scenes {
            r.add_scene(s);
        }
        r.set_show_card(false);
        r.set_settings(
            &h.gpu,
            RenderSettings {
                auto: false,
                auto_change: AutoChange::Off,
                quality: if dump.is_some() { Quality::High } else { Quality::Low },
                ..RenderSettings::default()
            },
        );
        Self { h, r, dump }
    }

    fn frame(&mut self, ms: &MusicState, t: f32, read: bool) -> Option<Vec<u8>> {
        let r = &mut self.r;
        let mut draw = |gpu: &onset_render::gpu::Gpu, view: &wgpu::TextureView| {
            let mut enc = gpu
                .device
                .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("g") });
            r.render(gpu, &mut enc, view, ms, t);
            gpu.queue.submit([enc.finish()]);
        };
        if read {
            Some(self.h.render_with(draw))
        } else {
            let view = self.h.view();
            draw(&self.h.gpu, &view);
            None
        }
    }

    /// Idle, build-up, drop and chorus as one 2x2 sheet at the frame size.
    fn sheet(&self, name: &str, frames: [&[u8]; 4]) {
        let Some(dir) = &self.dump else { return };
        let (w, h) = self.h.size;
        let mut out = image::RgbaImage::new(w, h);
        for (i, px) in frames.iter().enumerate() {
            let img = image::RgbaImage::from_raw(w, h, px.to_vec()).unwrap();
            let small = image::imageops::resize(&img, w / 2, h / 2, image::imageops::FilterType::Triangle);
            let (x, y) = ((i as u32 % 2) * w / 2, (i as u32 / 2) * h / 2);
            image::imageops::overlay(&mut out, &small, i64::from(x), i64::from(y));
        }
        out.save(dir.join(format!("{name}_sheet.png"))).unwrap();
    }

    fn save(&self, name: &str, px: &[u8]) {
        if let Some(dir) = &self.dump {
            let img = image::RgbaImage::from_raw(self.h.size.0, self.h.size.1, px.to_vec()).unwrap();
            img.save(dir.join(format!("{name}.png"))).unwrap();
        }
    }
}

#[test]
fn every_scene_reacts_to_the_music_and_the_drop() {
    let mut g = Gallery::new();
    let names = g.r.scene_names();
    let drop_at = 1.6;
    let only = std::env::var("ONSET_GALLERY_ONLY").ok();
    for name in names {
        if only.as_deref().is_some_and(|o| !o.split(',').any(|x| x == name)) {
            continue;
        }
        assert!(g.r.set_scene(&name));
        // Let the crossfade into this scene finish while idle.
        let mut idle = MusicState::default();
        let mut px_idle = Vec::new();
        for f in 0..40 {
            let t = f as f32 / FPS;
            idle.time_s = f64::from(t);
            if let Some(px) = g.frame(&idle, 100.0 + t, f == 39) {
                px_idle = px;
            }
        }
        g.save(&format!("{name}_0_idle"), &px_idle);
        let (_, spread) = luma_stats(&px_idle);
        assert!(spread > 3.0, "{name}: idle frame is flat (spread {spread:.1})");

        let mut build = Vec::new();
        let mut drop = Vec::new();
        let mut chorus = Vec::new();
        for f in 0..(3.0 * FPS) as u32 {
            let t = f as f32 / FPS;
            let ms = state(t, drop_at, 1.0, 1.0);
            let want = |at: f32| (t - at).abs() < 0.5 / FPS;
            let read = want(1.4) || want(drop_at + 0.1) || want(2.8);
            if let Some(px) = g.frame(&ms, 200.0 + t, read) {
                if want(1.4) {
                    build = px;
                } else if want(drop_at + 0.1) {
                    drop = px;
                } else {
                    chorus = px;
                }
            }
        }
        g.save(&format!("{name}_1_build"), &build);
        g.save(&format!("{name}_2_drop"), &drop);
        g.save(&format!("{name}_3_chorus"), &chorus);
        g.sheet(&name, [&px_idle, &build, &drop, &chorus]);
        let d = diff(&build, &drop);
        assert!(d > 4.0, "{name}: the drop barely changes the picture ({d:.1})");

        // Same moment, bass only versus treble only: the picture must differ. The classic
        // scenes predate the frequency split and stay out of Auto mode.
        if !scene_info(&name).is_some_and(|i| i.auto) {
            continue;
        }
        let t = 2.0;
        let bass_only = state(t, drop_at, 1.0, 0.0);
        let treble_only = state(t, drop_at, 0.0, 1.0);
        let a = g.frame(&bass_only, 300.0, true).unwrap();
        let b = g.frame(&treble_only, 300.0, true).unwrap();
        let d = diff(&a, &b);
        assert!(d > 1.0, "{name}: bass and treble look the same ({d:.2})");
    }
}
