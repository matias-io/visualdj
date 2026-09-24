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

/// The uniform buffer, its layout and bind group (group 0, binding 0) shared by all scenes.
pub struct FrameBindings {
    pub layout: wgpu::BindGroupLayout,
    pub bind_group: wgpu::BindGroup,
    pub buffer: wgpu::Buffer,
}

impl FrameBindings {
    pub fn new(gpu: &Gpu) -> Self {
        let layout = gpu
            .device
            .create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("frame uniforms"),
                entries: &[wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        // A shader whose `Frame` is bigger than this fails at pipeline
                        // creation, inside our error scope, instead of at draw time.
                        min_binding_size: std::num::NonZeroU64::new(
                            std::mem::size_of::<FrameUniforms>() as u64,
                        ),
                    },
                    count: None,
                }],
            });
        let buffer = gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("frame uniforms"),
            size: std::mem::size_of::<FrameUniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let bind_group = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("frame uniforms"),
            layout: &layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: buffer.as_entire_binding(),
            }],
        });
        Self {
            layout,
            bind_group,
            buffer,
        }
    }

    pub fn write(&self, gpu: &Gpu, uniforms: &FrameUniforms) {
        gpu.queue
            .write_buffer(&self.buffer, 0, bytemuck::bytes_of(uniforms));
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
        pass.set_bind_group(0, &frame.bind_group, &[]);
        pass.draw(0..3, 0..1);
    }
}
