//! Scenes may render at a fraction of the output size and be upscaled; the overlays stay
//! at full resolution.
use onset_core::music_state::MusicState;
use onset_render::headless::Headless;
use onset_render::renderer::Renderer;
use onset_render::scene::FullscreenScene;

const FLAT: &str = "@fragment fn fs_main(in: VsOut) -> @location(0) vec4<f32> { return vec4<f32>(0.6, 0.3, 0.1, 1.0); }";
// Left half red, right half green: upscaling must keep the split in the middle.
const SPLIT: &str = "@fragment fn fs_main(in: VsOut) -> @location(0) vec4<f32> { if (in.uv.x < 0.5) { return vec4<f32>(1.0, 0.0, 0.0, 1.0); } return vec4<f32>(0.0, 1.0, 0.0, 1.0); }";

fn frame(h: &Headless, r: &mut Renderer) -> Vec<u8> {
    h.render_with(|gpu, view| {
        let mut enc = gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("f") });
        r.render(gpu, &mut enc, view, &MusicState::default(), 0.0);
        gpu.queue.submit([enc.finish()]);
    })
}

fn setup(size: (u32, u32), shader: &str) -> (Headless, Renderer) {
    let h = Headless::new(size, true).expect("adapter");
    let mut r = Renderer::new(&h.gpu, size, h.format);
    let scene = FullscreenScene::new(&h.gpu, "s", shader, h.format, &r.bindings().layout).unwrap();
    r.add_scene(Box::new(scene));
    r.set_show_card(false);
    (h, r)
}

#[test]
fn half_scale_fills_the_frame_with_the_same_colour() {
    let (h, mut r) = setup((64, 64), FLAT);
    let full = frame(&h, &mut r);
    r.set_internal_scale(&h.gpu, 0.5);
    assert!((r.internal_scale() - 0.5).abs() < f32::EPSILON);
    assert_eq!(r.internal_size(), (32, 32));
    let half = frame(&h, &mut r);
    assert_eq!(full.len(), half.len());
    for (a, b) in full.chunks(4).zip(half.chunks(4)) {
        assert!(
            a.iter()
                .zip(b)
                .all(|(x, y)| (i16::from(*x) - i16::from(*y)).abs() <= 1),
            "{a:?} vs {b:?}"
        );
    }
}

#[test]
fn upscaled_geometry_lands_where_full_resolution_puts_it() {
    let (h, mut r) = setup((64, 32), SPLIT);
    r.set_internal_scale(&h.gpu, 0.5);
    let px = frame(&h, &mut r);
    let at = |x: usize, y: usize| &px[(y * 64 + x) * 4..(y * 64 + x) * 4 + 4];
    assert_eq!(&at(4, 16)[..3], &[255, 0, 0]);
    assert_eq!(&at(60, 16)[..3], &[0, 255, 0]);
    assert_eq!(&at(28, 16)[..3], &[255, 0, 0]);
    assert_eq!(&at(36, 16)[..3], &[0, 255, 0]);
}

#[test]
fn scale_is_clamped_and_one_means_no_offscreen_pass() {
    let (h, mut r) = setup((16, 16), FLAT);
    r.set_internal_scale(&h.gpu, 0.1);
    assert!(
        (r.internal_scale() - 0.5).abs() < f32::EPSILON,
        "floor is 0.5"
    );
    r.set_internal_scale(&h.gpu, 2.0);
    assert!(
        (r.internal_scale() - 1.0).abs() < f32::EPSILON,
        "ceiling is 1.0"
    );
    assert_eq!(r.internal_size(), (16, 16));
}
