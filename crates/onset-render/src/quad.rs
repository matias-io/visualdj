//! A textured rectangle with opacity, in pixel coordinates. Artwork, the card's scrim and
//! any future image overlay draw through this one pipeline.
use image::RgbaImage;

use crate::gpu::Gpu;

const SHADER: &str = r"
struct Quad {
    rect: vec4<f32>,
    screen: vec2<f32>,
    opacity: f32,
    _pad: f32,
};
@group(0) @binding(0) var<uniform> quad: Quad;
@group(0) @binding(1) var tex: texture_2d<f32>;
@group(0) @binding(2) var samp: sampler;

struct VsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) i: u32) -> VsOut {
    var corners = array<vec2<f32>, 6>(
        vec2<f32>(0.0, 0.0), vec2<f32>(1.0, 0.0), vec2<f32>(0.0, 1.0),
        vec2<f32>(0.0, 1.0), vec2<f32>(1.0, 0.0), vec2<f32>(1.0, 1.0),
    );
    let c = corners[i];
    let px = quad.rect.xy + c * quad.rect.zw;
    let ndc = vec2<f32>(px.x / quad.screen.x * 2.0 - 1.0, 1.0 - px.y / quad.screen.y * 2.0);
    var out: VsOut;
    out.pos = vec4<f32>(ndc, 0.0, 1.0);
    out.uv = c;
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let s = textureSample(tex, samp, in.uv);
    return vec4<f32>(s.rgb, s.a * quad.opacity);
}
";

#[repr(C)]
#[derive(Debug, Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct QuadUniforms {
    /// x, y, width, height in pixels from the top-left.
    rect: [f32; 4],
    screen: [f32; 2],
    opacity: f32,
    _pad: f32,
}

pub struct QuadPipeline {
    pipeline: wgpu::RenderPipeline,
    layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
}

/// A texture plus the bind group and uniform buffer that place it on screen.
pub struct QuadTexture {
    _texture: wgpu::Texture,
    bind_group: wgpu::BindGroup,
    uniforms: wgpu::Buffer,
    pub size: (u32, u32),
}

impl QuadPipeline {
    pub fn new(gpu: &Gpu, format: wgpu::TextureFormat) -> Self {
        let layout = gpu
            .device
            .create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("quad"),
                entries: &[
                    wgpu::BindGroupLayoutEntry {
                        binding: 0,
                        visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                        ty: wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Uniform,
                            has_dynamic_offset: false,
                            min_binding_size: None,
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 1,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Texture {
                            sample_type: wgpu::TextureSampleType::Float { filterable: true },
                            view_dimension: wgpu::TextureViewDimension::D2,
                            multisampled: false,
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 2,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                        count: None,
                    },
                ],
            });
        let pipeline_layout = gpu
            .device
            .create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("quad"),
                bind_group_layouts: &[Some(&layout)],
                immediate_size: 0,
            });
        let module = gpu
            .device
            .create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("quad"),
                source: wgpu::ShaderSource::Wgsl(SHADER.into()),
            });
        let pipeline = gpu
            .device
            .create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("quad"),
                layout: Some(&pipeline_layout),
                vertex: wgpu::VertexState {
                    module: &module,
                    entry_point: Some("vs_main"),
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                    buffers: &[],
                },
                fragment: Some(wgpu::FragmentState {
                    module: &module,
                    entry_point: Some("fs_main"),
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                    targets: &[Some(wgpu::ColorTargetState {
                        format,
                        blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                }),
                primitive: wgpu::PrimitiveState::default(),
                depth_stencil: None,
                multisample: wgpu::MultisampleState::default(),
                multiview_mask: None,
                cache: None,
            });
        let sampler = gpu.device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("quad"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::MipmapFilterMode::Linear,
            ..Default::default()
        });
        Self {
            pipeline,
            layout,
            sampler,
        }
    }

    /// Uploads an sRGB image as a texture ready to draw.
    pub fn upload(&self, gpu: &Gpu, image: &RgbaImage) -> QuadTexture {
        let (width, height) = image.dimensions();
        let size = wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        };
        let texture = gpu.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("quad texture"),
            size,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        gpu.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            image.as_raw(),
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(width * 4),
                rows_per_image: Some(height),
            },
            size,
        );
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let uniforms = gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("quad uniforms"),
            size: std::mem::size_of::<QuadUniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let bind_group = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("quad"),
            layout: &self.layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: uniforms.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
            ],
        });
        QuadTexture {
            _texture: texture,
            bind_group,
            uniforms,
            size: (width, height),
        }
    }

    /// Records a draw of `tex` at the rectangle last set with [`QuadTexture::place`].
    pub fn draw(&self, pass: &mut wgpu::RenderPass<'_>, tex: &QuadTexture) {
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &tex.bind_group, &[]);
        pass.draw(0..6, 0..1);
    }
}

impl QuadTexture {
    /// Sets where and how opaque the next draw is. Each texture has its own uniform buffer,
    /// so several quads can be placed before one pass draws them all.
    pub fn place(&self, gpu: &Gpu, rect: [f32; 4], screen: (u32, u32), opacity: f32) {
        let u = QuadUniforms {
            rect,
            screen: [screen.0 as f32, screen.1 as f32],
            opacity: opacity.clamp(0.0, 1.0),
            _pad: 0.0,
        };
        gpu.queue
            .write_buffer(&self.uniforms, 0, bytemuck::bytes_of(&u));
    }
}

/// A vertical gradient from transparent (top) to `alpha` black (bottom), for scrims.
pub fn scrim_image(alpha: f32, steps: u32) -> RgbaImage {
    RgbaImage::from_fn(1, steps, |_, y| {
        let t = y as f32 / (steps - 1).max(1) as f32;
        let a = (t * t * alpha * 255.0).round() as u8;
        image::Rgba([0, 0, 0, a])
    })
}
