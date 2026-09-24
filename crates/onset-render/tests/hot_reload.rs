use onset_core::music_state::MusicState;
use onset_render::headless::{Headless, hash_rgba};
use onset_render::renderer::Renderer;
use onset_render::scene::FullscreenScene;

const RED: &str = "@fragment fn fs_main(in: VsOut) -> @location(0) vec4<f32> { return vec4<f32>(1.0, 0.0, 0.0, 1.0); }";
const BLUE: &str = "@fragment fn fs_main(in: VsOut) -> @location(0) vec4<f32> { return vec4<f32>(0.0, 0.0, 1.0, 1.0); }";
const BROKEN: &str = "fn broken(";

fn frame(h: &Headless, r: &mut Renderer) -> Vec<u8> {
    let ms = MusicState::default();
    h.render_with(|gpu, view| {
        let mut enc = gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("f") });
        r.render(gpu, &mut enc, view, &ms, 0.0);
        gpu.queue.submit([enc.finish()]);
    })
}

#[test]
fn broken_shader_keeps_previous_pipeline() {
    let h = Headless::new((16, 16), true).expect("adapter");
    let mut r = Renderer::new(&h.gpu, (16, 16), h.format);
    let scene = FullscreenScene::new(&h.gpu, "live", RED, h.format, &r.bindings().layout).unwrap();
    r.add_scene(Box::new(scene));

    let before = hash_rgba(&frame(&h, &mut r));

    let err = r.reload_scene(&h.gpu, "live", BROKEN).unwrap_err();
    assert!(err.message.contains("broken") || !err.message.is_empty());
    assert!(r.last_error().is_some(), "the HUD needs the message");
    assert_eq!(
        hash_rgba(&frame(&h, &mut r)),
        before,
        "old pipeline must keep rendering"
    );

    r.reload_scene(&h.gpu, "live", BLUE).unwrap();
    assert!(r.last_error().is_none(), "a good reload clears the error");
    let after = frame(&h, &mut r);
    assert_ne!(hash_rgba(&after), before);
    assert_eq!(&after[0..4], &[0, 0, 255, 255]);
}

#[test]
fn reloading_an_unknown_scene_is_an_error_not_a_panic() {
    let h = Headless::new((8, 8), true).expect("adapter");
    let mut r = Renderer::new(&h.gpu, (8, 8), h.format);
    assert!(r.reload_scene(&h.gpu, "nope", RED).is_err());
}

#[test]
fn common_prelude_with_a_bigger_frame_is_rejected() {
    let h = Headless::new((8, 8), true).expect("adapter");
    let mut r = Renderer::new(&h.gpu, (8, 8), h.format);
    let scene = FullscreenScene::new(&h.gpu, "live", RED, h.format, &r.bindings().layout).unwrap();
    r.add_scene(Box::new(scene));
    let before = hash_rgba(&frame(&h, &mut r));

    // An edit that adds a field to Frame: the layout no longer matches the 240-byte buffer.
    let bigger = onset_render::scene::COMMON_WGSL.replacen(
        "struct Frame {",
        "struct Frame {\n    extra: vec4<f32>,",
        1,
    );
    assert!(bigger.contains("extra"), "test edit did not apply");
    // The scene must read `frame`, as every real scene does, for the binding to be checked.
    let uses_frame = "@fragment fn fs_main(in: VsOut) -> @location(0) vec4<f32> { return vec4<f32>(0.0, 0.0, 1.0, 1.0 + 0.0 * frame.time); }";
    let err = r.reload_scene_with_common(&h.gpu, "live", &bigger, uses_frame);
    assert!(
        err.is_err(),
        "a Frame that outgrows the uniform buffer must fail to load"
    );
    assert!(r.last_error().is_some());
    assert_eq!(
        hash_rgba(&frame(&h, &mut r)),
        before,
        "old pipeline keeps rendering"
    );
}
