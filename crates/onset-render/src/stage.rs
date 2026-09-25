//! The stage: HDR render targets, scene transitions, bloom and the final composite. Scenes
//! draw into an HDR texture at the internal resolution; the previous frame is kept for
//! feedback effects; bloom works at half resolution and below; the composite tonemaps,
//! applies the show director's effects and writes the output at full resolution.
use crate::gpu::Gpu;
use crate::scene::{PREV_FORMAT, texture_2d};

/// Scenes and effects render in this format so highlights can exceed 1.0 and bloom.
pub const HDR_FORMAT: wgpu::TextureFormat = PREV_FORMAT;

const SHADER: &str = include_str!("../../../assets/shaders/post.wgsl");

/// Mirrors `struct Post` in `post.wgsl`.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct PostUniforms {
    /// Output width, height, 1/width, 1/height.
    pub res_texel: [f32; 4],
    /// Time, flash, invert, hue shift (turns).
    pub a: [f32; 4],
    /// Flash colour rgb, strobe.
    pub flash_col: [f32; 4],
    /// Shake x, shake y (uv), zoom punch, glitch.
    pub motion: [f32; 4],
    /// Bloom strength, grain, vignette, chromatic aberration.
    pub look: [f32; 4],
    /// Exposure, seed, bloom threshold, blackout.
    pub tone: [f32; 4],
    /// Transition progress, transition kind, emphasis, tension.
    pub trans: [f32; 4],
}

impl Default for PostUniforms {
    fn default() -> Self {
        Self {
            res_texel: [1.0, 1.0, 1.0, 1.0],
            a: [0.0; 4],
            flash_col: [1.0, 1.0, 1.0, 0.0],
            motion: [0.0; 4],
            look: [0.8, 0.2, 0.35, 0.002],
            tone: [1.0, 0.0, 1.0, 0.0],
            trans: [0.0; 4],
        }
    }
}

/// A render target that later passes sample.
struct Target {
    _texture: wgpu::Texture,
    view: wgpu::TextureView,
}

impl Target {
    fn new(gpu: &Gpu, label: &str, size: (u32, u32)) -> Self {
        let (texture, view) = texture_2d(
            gpu,
            label,
            size,
            HDR_FORMAT,
            wgpu::TextureUsages::RENDER_ATTACHMENT,
        );
        Self {
            _texture: texture,
            view,
        }
    }
}

fn half(size: (u32, u32)) -> (u32, u32) {
    ((size.0 / 2).max(1), (size.1 / 2).max(1))
}

pub struct Stage {
    out_format: wgpu::TextureFormat,
    layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    buffer: wgpu::Buffer,
    transition: wgpu::RenderPipeline,
    bright: wgpu::RenderPipeline,
    down: wgpu::RenderPipeline,
    up: wgpu::RenderPipeline,
    composite: wgpu::RenderPipeline,
    /// The two scenes of a transition (outgoing, incoming).
    scenes: [Target; 2],
    /// Feedback ping-pong: this frame's picture, and last frame's for scenes to read.
    mix: [Target; 2],
    /// Bloom chain, half resolution and down.
    bloom: Vec<Target>,
    blank: Target,
    size: (u32, u32),
    bloom_levels: usize,
}

impl Stage {
    #[allow(clippy::too_many_lines)] // one pipeline table
    pub fn new(
        gpu: &Gpu,
        out_format: wgpu::TextureFormat,
        internal: (u32, u32),
        bloom_levels: usize,
    ) -> Self {
        let layout = gpu
            .device
            .create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("post"),
                entries: &[
                    wgpu::BindGroupLayoutEntry {
                        binding: 0,
                        visibility: wgpu::ShaderStages::FRAGMENT,
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
                        ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                        count: None,
                    },
                    tex_entry(2),
                    tex_entry(3),
                ],
            });
        let sampler = gpu.device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("post"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let buffer = gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("post uniforms"),
            size: std::mem::size_of::<PostUniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let module = gpu
            .device
            .create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("post"),
                source: wgpu::ShaderSource::Wgsl(SHADER.into()),
            });
        let pipeline_layout = gpu
            .device
            .create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("post"),
                bind_group_layouts: &[Some(&layout)],
                immediate_size: 0,
            });
        let make = |entry: &str, format: wgpu::TextureFormat, additive: bool| {
            let blend = additive.then_some(wgpu::BlendState {
                color: wgpu::BlendComponent {
                    src_factor: wgpu::BlendFactor::One,
                    dst_factor: wgpu::BlendFactor::One,
                    operation: wgpu::BlendOperation::Add,
                },
                alpha: wgpu::BlendComponent::OVER,
            });
            gpu.device
                .create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                    label: Some(entry),
                    layout: Some(&pipeline_layout),
                    vertex: wgpu::VertexState {
                        module: &module,
                        entry_point: Some("vs_fullscreen"),
                        compilation_options: wgpu::PipelineCompilationOptions::default(),
                        buffers: &[],
                    },
                    fragment: Some(wgpu::FragmentState {
                        module: &module,
                        entry_point: Some(entry),
                        compilation_options: wgpu::PipelineCompilationOptions::default(),
                        targets: &[Some(wgpu::ColorTargetState {
                            format,
                            blend,
                            write_mask: wgpu::ColorWrites::ALL,
                        })],
                    }),
                    primitive: wgpu::PrimitiveState::default(),
                    depth_stencil: None,
                    multisample: wgpu::MultisampleState::default(),
                    multiview_mask: None,
                    cache: None,
                })
        };
        let transition = make("fs_transition", HDR_FORMAT, false);
        let bright = make("fs_bright", HDR_FORMAT, false);
        let down = make("fs_down", HDR_FORMAT, false);
        let up = make("fs_up", HDR_FORMAT, true);
        let composite = make("fs_composite", out_format, false);
        let mut stage = Self {
            out_format,
            layout,
            sampler,
            buffer,
            transition,
            bright,
            down,
            up,
            composite,
            scenes: [
                Target::new(gpu, "scene a", (1, 1)),
                Target::new(gpu, "scene b", (1, 1)),
            ],
            mix: [
                Target::new(gpu, "mix 0", (1, 1)),
                Target::new(gpu, "mix 1", (1, 1)),
            ],
            bloom: Vec::new(),
            blank: Target::new(gpu, "blank", (1, 1)),
            size: (0, 0),
            bloom_levels,
        };
        stage.resize(gpu, internal, bloom_levels);
        stage
    }

    pub fn out_format(&self) -> wgpu::TextureFormat {
        self.out_format
    }

    /// Reallocates every target for a new internal size or bloom depth.
    pub fn resize(&mut self, gpu: &Gpu, internal: (u32, u32), bloom_levels: usize) {
        let internal = (internal.0.max(1), internal.1.max(1));
        if internal == self.size && bloom_levels == self.bloom_levels && !self.bloom.is_empty() {
            return;
        }
        self.size = internal;
        self.bloom_levels = bloom_levels.clamp(1, 6);
        self.scenes = [
            Target::new(gpu, "scene a", internal),
            Target::new(gpu, "scene b", internal),
        ];
        self.mix = [
            Target::new(gpu, "mix 0", internal),
            Target::new(gpu, "mix 1", internal),
        ];
        let mut size = half(internal);
        self.bloom = (0..self.bloom_levels)
            .map(|i| {
                let t = Target::new(gpu, &format!("bloom {i}"), size);
                size = half(size);
                t
            })
            .collect();
    }

    pub fn size(&self) -> (u32, u32) {
        self.size
    }

    /// The two feedback views, for `FrameBindings::set_prev_views`.
    pub fn mix_views(&self) -> [wgpu::TextureView; 2] {
        [self.mix[0].view.clone(), self.mix[1].view.clone()]
    }

    /// Where the frame's picture goes when no transition runs.
    pub fn mix_view(&self, index: usize) -> &wgpu::TextureView {
        &self.mix[index % 2].view
    }

    /// The outgoing (0) and incoming (1) scene targets of a transition.
    pub fn scene_view(&self, which: usize) -> &wgpu::TextureView {
        &self.scenes[which % 2].view
    }

    pub fn write(&self, gpu: &Gpu, uniforms: &PostUniforms) {
        gpu.queue
            .write_buffer(&self.buffer, 0, bytemuck::bytes_of(uniforms));
    }

    fn group(&self, gpu: &Gpu, a: &wgpu::TextureView, b: &wgpu::TextureView) -> wgpu::BindGroup {
        gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("post"),
            layout: &self.layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: self.buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::TextureView(a),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::TextureView(b),
                },
            ],
        })
    }

    fn pass(
        encoder: &mut wgpu::CommandEncoder,
        label: &str,
        target: &wgpu::TextureView,
        pipeline: &wgpu::RenderPipeline,
        group: &wgpu::BindGroup,
        load: wgpu::LoadOp<wgpu::Color>,
    ) {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some(label),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: target,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load,
                    store: wgpu::StoreOp::Store,
                },
            })],
            ..Default::default()
        });
        pass.set_pipeline(pipeline);
        pass.set_bind_group(0, group, &[]);
        pass.draw(0..3, 0..1);
    }

    /// Mixes the two scene targets into `mix[index]` (the uniforms carry progress and kind).
    pub fn run_transition(&self, gpu: &Gpu, encoder: &mut wgpu::CommandEncoder, index: usize) {
        let g = self.group(gpu, &self.scenes[0].view, &self.scenes[1].view);
        Self::pass(
            encoder,
            "transition",
            &self.mix[index % 2].view,
            &self.transition,
            &g,
            wgpu::LoadOp::Clear(wgpu::Color::BLACK),
        );
    }

    /// Bloom from `mix[index]`, then the composite into `target` at output size.
    pub fn finish(
        &self,
        gpu: &Gpu,
        encoder: &mut wgpu::CommandEncoder,
        index: usize,
        target: &wgpu::TextureView,
    ) {
        let picture = &self.mix[index % 2].view;
        let clear = wgpu::LoadOp::Clear(wgpu::Color::BLACK);
        let g = self.group(gpu, picture, &self.blank.view);
        Self::pass(
            encoder,
            "bloom bright",
            &self.bloom[0].view,
            &self.bright,
            &g,
            clear,
        );
        for i in 0..self.bloom.len() - 1 {
            let g = self.group(gpu, &self.bloom[i].view, &self.blank.view);
            Self::pass(
                encoder,
                "bloom down",
                &self.bloom[i + 1].view,
                &self.down,
                &g,
                clear,
            );
        }
        for i in (0..self.bloom.len() - 1).rev() {
            let g = self.group(gpu, &self.bloom[i + 1].view, &self.blank.view);
            Self::pass(
                encoder,
                "bloom up",
                &self.bloom[i].view,
                &self.up,
                &g,
                wgpu::LoadOp::Load,
            );
        }
        let g = self.group(gpu, picture, &self.bloom[0].view);
        Self::pass(encoder, "composite", target, &self.composite, &g, clear);
    }
}

fn tex_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Float { filterable: true },
            view_dimension: wgpu::TextureViewDimension::D2,
            multisampled: false,
        },
        count: None,
    }
}
