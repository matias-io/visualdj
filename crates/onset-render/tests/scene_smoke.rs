use onset_core::music_state::MusicState;
use onset_render::headless::Headless;
use onset_render::scene::{FrameBindings, FullscreenScene, Scene};
use onset_render::uniforms::FrameUniforms;

/// A fragment stage that paints the beat phase into the red channel.
const FRAG: &str = r"
@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    return vec4<f32>(frame.beat_phase, 0.0, 0.0, 1.0);
}
";

/// sRGB encoding of linear 0.5 is 188/255.
const SRGB_HALF: i32 = 188;

#[test]
fn fullscreen_scene_reads_frame_uniforms() {
    let h = Headless::new((64, 64), true).expect("adapter");
    let bindings = FrameBindings::new(&h.gpu);
    let mut scene =
        FullscreenScene::new(&h.gpu, "smoke", FRAG, h.format, &bindings.layout).expect("compile");

    let ms = MusicState {
        beat_phase: 0.5,
        ..MusicState::default()
    };
    bindings.write(&h.gpu, &FrameUniforms::from_state(&ms, (64, 64), 0.0));

    let pixels = h.render_with(|gpu, view| {
        let mut enc = gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("smoke"),
            });
        scene.render(gpu, &mut enc, view, &bindings);
        gpu.queue.submit([enc.finish()]);
    });
    let centre = (32 * 64 + 32) * 4;
    let r = i32::from(pixels[centre]);
    assert!(
        (r - SRGB_HALF).abs() <= 2,
        "red = {r}, expected about {SRGB_HALF}"
    );
    assert_eq!(pixels[centre + 1], 0);
    assert_eq!(pixels[centre + 3], 255);
    assert_eq!(scene.name(), "smoke");
}

#[test]
fn broken_shader_is_an_error_not_a_panic() {
    let h = Headless::new((8, 8), true).expect("adapter");
    let bindings = FrameBindings::new(&h.gpu);
    let result = FullscreenScene::new(&h.gpu, "bad", "fn broken(", h.format, &bindings.layout);
    assert!(result.is_err());
}
