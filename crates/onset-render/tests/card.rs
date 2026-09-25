//! The Now Playing card: draws over the scene when a track is loaded, leaves the frame
//! untouched when there is none.
use onset_core::music_state::MusicState;
use onset_core::track::{TrackId, TrackMeta};
use onset_render::headless::{Headless, hash_rgba};
use onset_render::renderer::Renderer;
use onset_render::scene::FullscreenScene;

const FLAT: &str = "@fragment fn fs_main(in: VsOut) -> @location(0) vec4<f32> { return vec4<f32>(0.1, 0.2, 0.3, 1.0); }";

fn move_track() -> TrackMeta {
    TrackMeta {
        id: TrackId(42),
        title: "Move".into(),
        artist: "Adam Port, Stryv, Keinemusik, Orso, Malachiii".into(),
        album: "Move".into(),
        year: Some(2024),
        key: Some("11B".into()),
        bpm: Some(120.0),
        duration_s: Some(180.0),
        sample_rate: Some(44_100),
        file_path: None,
        artwork_path: None,
        analysis_path: None,
        isrc: None,
        genre: None,
    }
}

fn frame(h: &Headless, r: &mut Renderer, ms: &MusicState, time_s: f32) -> Vec<u8> {
    h.render_with(|gpu, view| {
        let mut enc = gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("f") });
        r.render(gpu, &mut enc, view, ms, time_s);
        gpu.queue.submit([enc.finish()]);
    })
}

fn setup(size: (u32, u32)) -> (Headless, Renderer) {
    let h = Headless::new(size, true).expect("adapter");
    let mut r = Renderer::new(&h.gpu, size, h.format);
    let scene = FullscreenScene::new(&h.gpu, "flat", FLAT, h.format, &r.bindings().layout).unwrap();
    r.add_scene(Box::new(scene));
    r.set_scale(1.0);
    (h, r)
}

/// Pixels of `frame` inside the rectangle, as RGBA quads.
fn region(frame: &[u8], width: u32, x0: u32, y0: u32, x1: u32, y1: u32) -> Vec<[u8; 4]> {
    let mut out = Vec::new();
    for y in y0..y1 {
        for x in x0..x1 {
            let i = ((y * width + x) * 4) as usize;
            out.push([frame[i], frame[i + 1], frame[i + 2], frame[i + 3]]);
        }
    }
    out
}

/// Saves a frame as PNG when `ONSET_DUMP_DIR` is set, for eyeballing layout.
fn dump(name: &str, frame: &[u8], size: (u32, u32)) {
    if let Some(dir) = std::env::var_os("ONSET_DUMP_DIR") {
        let path = std::path::Path::new(&dir).join(format!("{name}.png"));
        let img = image::RgbaImage::from_raw(size.0, size.1, frame.to_vec()).unwrap();
        img.save(&path).unwrap();
        eprintln!("wrote {}", path.display());
    }
}

fn close(a: [u8; 4], b: [u8; 4], tol: i16) -> bool {
    a.iter()
        .zip(b.iter())
        .all(|(x, y)| (i16::from(*x) - i16::from(*y)).abs() <= tol)
}

#[test]
fn card_renders_title_text() {
    let (h, mut r) = setup((640, 360));
    let plain = frame(&h, &mut r, &MusicState::default(), 0.0);
    let background = region(&plain, 640, 0, 0, 1, 1)[0];

    let mut ms = MusicState {
        track: Some(move_track()),
        ..MusicState::default()
    };
    ms.bpm = 120.0;

    // First frame starts the fade-in; a later frame shows the card fully.
    let fading = frame(&h, &mut r, &ms, 0.1);
    let shown = frame(&h, &mut r, &ms, 5.0);
    dump("card_fading", &fading, (640, 360));
    dump("card_shown", &shown, (640, 360));

    let bottom_left = region(&shown, 640, 0, 200, 420, 360);
    let changed = bottom_left
        .iter()
        .filter(|p| !close(**p, background, 12))
        .count();
    assert!(
        changed > 200,
        "the card region must contain drawn pixels, found {changed}"
    );
    // The fade is soft: the early frame sits between the scene and the settled card.
    let title_zone = |f: &[u8]| region(f, 640, 120, 280, 420, 345);
    let settled_ink = title_zone(&shown)
        .iter()
        .filter(|p| !close(**p, background, 40))
        .count();
    let early_ink = title_zone(&fading)
        .iter()
        .filter(|p| !close(**p, background, 40))
        .count();
    assert!(settled_ink > 50, "title ink {settled_ink}");
    assert!(
        early_ink < settled_ink,
        "early {early_ink} vs settled {settled_ink}"
    );

    let top_right = region(&shown, 640, 420, 0, 640, 120);
    assert!(
        top_right.iter().all(|p| close(*p, background, 1)),
        "the card must not touch the top-right of the frame"
    );
}

#[test]
fn no_track_renders_the_scene_alone() {
    let (h, mut r) = setup((320, 180));
    let plain = frame(&h, &mut r, &MusicState::default(), 0.0);
    let again = frame(&h, &mut r, &MusicState::default(), 3.0);
    assert_eq!(hash_rgba(&plain), hash_rgba(&again));
    // Every pixel is the scene colour: the overlay pass drew nothing.
    let first = region(&plain, 320, 0, 0, 1, 1)[0];
    assert!(
        plain
            .chunks(4)
            .all(|p| p == first && p[3] == 255 && p[2] > p[0])
    );
}

#[test]
fn card_can_be_hidden() {
    let (h, mut r) = setup((320, 180));
    let ms = MusicState {
        track: Some(move_track()),
        ..MusicState::default()
    };
    let _ = frame(&h, &mut r, &ms, 0.0);
    let visible = frame(&h, &mut r, &ms, 5.0);
    r.set_show_card(false);
    let hidden = frame(&h, &mut r, &ms, 6.0);
    assert_ne!(hash_rgba(&visible), hash_rgba(&hidden));
    assert!(
        hidden.chunks(4).all(|p| p[2] > p[0]),
        "hidden card leaves the scene"
    );
}

#[test]
fn hud_draws_when_enabled() {
    let (h, mut r) = setup((320, 180));
    let ms = MusicState::default();
    let without = frame(&h, &mut r, &ms, 0.0);
    r.set_show_hud(true);
    let with = frame(&h, &mut r, &ms, 0.1);
    assert_ne!(
        hash_rgba(&without),
        hash_rgba(&with),
        "HUD text must be drawn"
    );
}

#[test]
fn blackout_hides_scene_and_overlays() {
    let (h, mut r) = setup((320, 180));
    let ms = MusicState {
        track: Some(move_track()),
        ..MusicState::default()
    };
    let _ = frame(&h, &mut r, &ms, 0.0);
    r.set_show_hud(true);
    r.set_blackout(true);
    let dark = frame(&h, &mut r, &ms, 5.0);
    assert!(
        dark.chunks(4).all(|p| p == [0, 0, 0, 255]),
        "blackout must be pure black"
    );
    r.set_blackout(false);
    let back = frame(&h, &mut r, &ms, 6.0);
    assert!(
        back.chunks(4).any(|p| p[2] > 40),
        "scene returns after blackout"
    );
}
