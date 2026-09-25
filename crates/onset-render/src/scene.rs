//! Scenes and the frame bindings they read. A `FullscreenScene` is one fragment shader over a
//! fullscreen triangle; most visuals are built this way, so shader authors only write WGSL.
use crate::gpu::Gpu;
use crate::uniforms::FrameUniforms;

/// Prepended to every scene shader: the `Frame` uniform, the fullscreen vertex stage, helpers.
pub const COMMON_WGSL: &str = include_str!("../../../assets/shaders/common.wgsl");

#[derive(Debug, Clone, thiserror::Error)]
#[error("shader `{name}` failed to compile: {message}")]
pub struct ShaderError {
    pub name: String,
    pub message: String,
}

/// Rows of spectrum history kept (about two seconds at 60 fps) and values per row: 24 band
/// levels, 6 groups, kick and snare.
pub const HISTORY_ROWS: u32 = 128;
pub const HISTORY_COLS: u32 = 32;
/// Side of the cover-art texture scenes sample.
pub const ART_SIZE: u32 = 256;
/// Format of the previous-frame texture scenes read (the renderer's HDR format).
pub const PREV_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;

fn texture_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
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

fn extent(size: (u32, u32)) -> wgpu::Extent3d {
    wgpu::Extent3d {
        width: size.0.max(1),
        height: size.1.max(1),
        depth_or_array_layers: 1,
    }
}

/// A sampled 2D texture that can be written from the CPU (and rendered to with `extra`).
pub fn texture_2d(
    gpu: &Gpu,
    label: &str,
    size: (u32, u32),
    format: wgpu::TextureFormat,
    extra: wgpu::TextureUsages,
) -> (wgpu::Texture, wgpu::TextureView) {
    let texture = gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: Some(label),
        size: extent(size),
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST | extra,
        view_formats: &[],
    });
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    (texture, view)
}

fn write_rgba(gpu: &Gpu, texture: &wgpu::Texture, size: (u32, u32), pixels: &[u8]) {
    gpu.queue.write_texture(
        texture.as_image_copy(),
        pixels,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(size.0 * 4),
            rows_per_image: Some(size.1),
        },
        extent(size),
    );
}

/// The group-0 resources every scene reads: the `Frame` uniform, a linear sampler, the
/// previous frame (two of them, ping-ponged by the renderer), the spectrum history and the
/// cover art.
pub struct FrameBindings {
    pub layout: wgpu::BindGroupLayout,
    pub buffer: wgpu::Buffer,
    sampler: wgpu::Sampler,
    history: wgpu::Texture,
    history_view: wgpu::TextureView,
    history_row: u32,
    art: wgpu::Texture,
    art_view: wgpu::TextureView,
    prev_views: [wgpu::TextureView; 2],
    groups: [wgpu::BindGroup; 2],
    current: usize,
}

struct Parts<'a> {
    layout: &'a wgpu::BindGroupLayout,
    buffer: &'a wgpu::Buffer,
    sampler: &'a wgpu::Sampler,
    history: &'a wgpu::TextureView,
    art: &'a wgpu::TextureView,
}

impl Parts<'_> {
    fn group(&self, gpu: &Gpu, prev: &wgpu::TextureView) -> wgpu::BindGroup {
        gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("frame"),
            layout: self.layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: self.buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(self.sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::TextureView(prev),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::TextureView(self.history),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: wgpu::BindingResource::TextureView(self.art),
                },
            ],
        })
    }
}

impl FrameBindings {
    pub fn new(gpu: &Gpu) -> Self {
        let layout = gpu
            .device
            .create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("frame"),
                entries: &[
                    wgpu::BindGroupLayoutEntry {
                        binding: 0,
                        visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                        ty: wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Uniform,
                            has_dynamic_offset: false,
                            // A shader whose `Frame` is bigger than this fails at pipeline
                            // creation, inside our error scope, instead of at draw time.
                            min_binding_size: std::num::NonZeroU64::new(std::mem::size_of::<
                                FrameUniforms,
                            >(
                            )
                                as u64),
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 1,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                        count: None,
                    },
                    texture_entry(2),
                    texture_entry(3),
                    texture_entry(4),
                ],
            });
        let buffer = gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("frame uniforms"),
            size: std::mem::size_of::<FrameUniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let sampler = gpu.device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("frame"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::Repeat,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let (history, history_view) = texture_2d(
            gpu,
            "spectrum history",
            (HISTORY_COLS, HISTORY_ROWS),
            wgpu::TextureFormat::Rgba8Unorm,
            wgpu::TextureUsages::empty(),
        );
        let (art, art_view) = texture_2d(
            gpu,
            "cover art",
            (1, 1),
            wgpu::TextureFormat::Rgba8UnormSrgb,
            wgpu::TextureUsages::empty(),
        );
        write_rgba(gpu, &art, (1, 1), &[40, 40, 48, 255]);
        // Until the renderer hands over its own targets, scenes see a black previous frame.
        let (_blank, blank_view) = texture_2d(
            gpu,
            "blank previous frame",
            (1, 1),
            PREV_FORMAT,
            wgpu::TextureUsages::empty(),
        );
        let prev_views = [blank_view.clone(), blank_view];
        let groups = {
            let parts = Parts {
                layout: &layout,
                buffer: &buffer,
                sampler: &sampler,
                history: &history_view,
                art: &art_view,
            };
            [
                parts.group(gpu, &prev_views[0]),
                parts.group(gpu, &prev_views[1]),
            ]
        };
        Self {
            layout,
            buffer,
            sampler,
            history,
            history_view,
            history_row: 0,
            art,
            art_view,
            prev_views,
            groups,
            current: 0,
        }
    }

    fn rebuild(&mut self, gpu: &Gpu) {
        let parts = Parts {
            layout: &self.layout,
            buffer: &self.buffer,
            sampler: &self.sampler,
            history: &self.history_view,
            art: &self.art_view,
        };
        self.groups = [
            parts.group(gpu, &self.prev_views[0]),
            parts.group(gpu, &self.prev_views[1]),
        ];
    }

    pub fn write(&self, gpu: &Gpu, uniforms: &FrameUniforms) {
        gpu.queue
            .write_buffer(&self.buffer, 0, bytemuck::bytes_of(uniforms));
    }

    /// The bind group scenes use this frame.
    pub fn bind_group(&self) -> &wgpu::BindGroup {
        &self.groups[self.current]
    }

    /// The renderer's two feedback targets; group `i` reads `views[i]` as the previous frame.
    pub fn set_prev_views(&mut self, gpu: &Gpu, views: [wgpu::TextureView; 2]) {
        self.prev_views = views;
        self.rebuild(gpu);
    }

    /// Chooses which previous-frame texture scenes read this frame.
    pub fn select(&mut self, index: usize) {
        self.current = index % 2;
    }

    /// Appends one row of spectrum history (values 0..1).
    pub fn push_history(&mut self, gpu: &Gpu, values: &[f32; HISTORY_COLS as usize]) {
        self.history_row = (self.history_row + 1) % HISTORY_ROWS;
        let mut row = [0u8; HISTORY_COLS as usize * 4];
        for (px, v) in row.as_chunks_mut::<4>().0.iter_mut().zip(values) {
            let b = (v.clamp(0.0, 1.0) * 255.0).round() as u8;
            *px = [b, b, b, 255];
        }
        gpu.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &self.history,
                mip_level: 0,
                origin: wgpu::Origin3d {
                    x: 0,
                    y: self.history_row,
                    z: 0,
                },
                aspect: wgpu::TextureAspect::All,
            },
            &row,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(HISTORY_COLS * 4),
                rows_per_image: Some(1),
            },
            extent((HISTORY_COLS, 1)),
        );
    }

    /// The newest history row as a texture coordinate (what `Frame.vibe.w` carries).
    pub fn history_row(&self) -> f32 {
        (self.history_row as f32 + 0.5) / HISTORY_ROWS as f32
    }

    /// Replaces the cover art scenes sample (resized to `ART_SIZE` square).
    pub fn set_artwork(&mut self, gpu: &Gpu, image: &image::RgbaImage) {
        let img = image::imageops::resize(
            image,
            ART_SIZE,
            ART_SIZE,
            image::imageops::FilterType::Triangle,
        );
        let (art, art_view) = texture_2d(
            gpu,
            "cover art",
            (ART_SIZE, ART_SIZE),
            wgpu::TextureFormat::Rgba8UnormSrgb,
            wgpu::TextureUsages::empty(),
        );
        write_rgba(gpu, &art, (ART_SIZE, ART_SIZE), img.as_raw());
        self.art = art;
        self.art_view = art_view;
        self.rebuild(gpu);
    }
}

pub trait Scene {
    fn name(&self) -> &str;
    /// Scenes that are a single fragment shader expose themselves for hot reload.
    fn as_fullscreen_mut(&mut self) -> Option<&mut FullscreenScene> {
        None
    }
    /// Called when the output size changes; scenes with internal targets reallocate here.
    fn resize(&mut self, _gpu: &Gpu, _size: (u32, u32)) {}
    /// Record this scene's draw into `encoder`, writing `target`.
    fn render(
        &mut self,
        gpu: &Gpu,
        encoder: &mut wgpu::CommandEncoder,
        target: &wgpu::TextureView,
        frame: &FrameBindings,
    );
}

/// One fragment shader (`fs_main`) over a fullscreen triangle, reading `Frame`.
pub struct FullscreenScene {
    name: String,
    format: wgpu::TextureFormat,
    pipeline: wgpu::RenderPipeline,
    pipeline_layout: wgpu::PipelineLayout,
}

impl FullscreenScene {
    pub fn new(
        gpu: &Gpu,
        name: &str,
        fragment_wgsl: &str,
        format: wgpu::TextureFormat,
        layout: &wgpu::BindGroupLayout,
    ) -> Result<Self, ShaderError> {
        let pipeline_layout = gpu
            .device
            .create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some(name),
                bind_group_layouts: &[Some(layout)],
                immediate_size: 0,
            });
        let pipeline = build_pipeline(gpu, name, fragment_wgsl, format, &pipeline_layout)?;
        Ok(Self {
            name: name.to_string(),
            format,
            pipeline,
            pipeline_layout,
        })
    }

    /// Swap in a new fragment shader. On error the current pipeline stays in use.
    pub fn replace_shader(&mut self, gpu: &Gpu, fragment_wgsl: &str) -> Result<(), ShaderError> {
        self.replace_shader_with_common(gpu, COMMON_WGSL, fragment_wgsl)
    }

    /// Like [`Self::replace_shader`] but with a caller-supplied common prelude (the on-disk
    /// `common.wgsl`, so edits to it take effect without a rebuild).
    pub fn replace_shader_with_common(
        &mut self,
        gpu: &Gpu,
        common_wgsl: &str,
        fragment_wgsl: &str,
    ) -> Result<(), ShaderError> {
        let pipeline = build_pipeline_with_common(
            gpu,
            &self.name,
            common_wgsl,
            fragment_wgsl,
            self.format,
            &self.pipeline_layout,
        )?;
        self.pipeline = pipeline;
        Ok(())
    }
}

/// Compiles inside a validation error scope so a bad shader is an `Err`, never a panic.
fn build_pipeline(
    gpu: &Gpu,
    name: &str,
    fragment_wgsl: &str,
    format: wgpu::TextureFormat,
    layout: &wgpu::PipelineLayout,
) -> Result<wgpu::RenderPipeline, ShaderError> {
    build_pipeline_with_common(gpu, name, COMMON_WGSL, fragment_wgsl, format, layout)
}

fn build_pipeline_with_common(
    gpu: &Gpu,
    name: &str,
    common_wgsl: &str,
    fragment_wgsl: &str,
    format: wgpu::TextureFormat,
    layout: &wgpu::PipelineLayout,
) -> Result<wgpu::RenderPipeline, ShaderError> {
    let source = format!("{common_wgsl}\n{fragment_wgsl}");
    let scope = gpu.device.push_error_scope(wgpu::ErrorFilter::Validation);
    let module = gpu
        .device
        .create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some(name),
            source: wgpu::ShaderSource::Wgsl(source.into()),
        });
    let pipeline = gpu
        .device
        .create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some(name),
            layout: Some(layout),
            vertex: wgpu::VertexState {
                module: &module,
                entry_point: Some("vs_fullscreen"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                buffers: &[],
            },
            fragment: Some(wgpu::FragmentState {
                module: &module,
                entry_point: Some("fs_main"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });
    let error = pollster::block_on(scope.pop());
    match error {
        Some(e) => Err(ShaderError {
            name: name.to_string(),
            // The Display form is just "Validation Error"; the description has the line info.
            message: match &e {
                wgpu::Error::Validation { description, .. } => description.clone(),
                other => other.to_string(),
            },
        }),
        None => Ok(pipeline),
    }
}

impl Scene for FullscreenScene {
    fn name(&self) -> &str {
        &self.name
    }

    fn as_fullscreen_mut(&mut self) -> Option<&mut FullscreenScene> {
        Some(self)
    }

    fn render(
        &mut self,
        _gpu: &Gpu,
        encoder: &mut wgpu::CommandEncoder,
        target: &wgpu::TextureView,
        frame: &FrameBindings,
    ) {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some(&self.name),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: target,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                    store: wgpu::StoreOp::Store,
                },
            })],
            ..Default::default()
        });
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, frame.bind_group(), &[]);
        pass.draw(0..3, 0..1);
    }
}
