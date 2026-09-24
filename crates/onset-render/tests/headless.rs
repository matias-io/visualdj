use onset_render::headless::{Headless, hash_rgba};

#[test]
fn clears_to_a_solid_colour_and_reads_back() {
    let h = Headless::new((64, 32), true).expect("a DX12/Vulkan or WARP adapter");
    let pixels = h.render_with(|gpu, view| {
        let mut enc = gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("clear"),
            });
        {
            let _pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("clear"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: 1.0,
                            g: 0.0,
                            b: 0.0,
                            a: 1.0,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                ..Default::default()
            });
        }
        gpu.queue.submit([enc.finish()]);
    });
    assert_eq!(pixels.len(), 64 * 32 * 4);
    assert_eq!(&pixels[0..4], &[255, 0, 0, 255]);
    let h1 = hash_rgba(&pixels);
    assert_eq!(h1, hash_rgba(&pixels));
    assert_ne!(h1, hash_rgba(&[0u8; 8]));
}
