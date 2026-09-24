//! Render to an offscreen texture and read the pixels back. Used by tests and screenshots.
use crate::gpu::Gpu;

pub struct Headless {
    pub gpu: Gpu,
    pub size: (u32, u32),
    pub format: wgpu::TextureFormat,
    texture: wgpu::Texture,
}

/// Row stride of the readback buffer, padded to wgpu's 256-byte alignment.
fn padded_bytes_per_row(width: u32) -> u32 {
    let unpadded = width * 4;
    let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
    unpadded.div_ceil(align) * align
}

impl Headless {
    pub fn new(size: (u32, u32), prefer_software: bool) -> anyhow::Result<Self> {
        let gpu = Gpu::new_headless(prefer_software)?;
        Ok(Self::with_gpu(gpu, size))
    }

    pub fn with_gpu(gpu: Gpu, size: (u32, u32)) -> Self {
        let format = wgpu::TextureFormat::Rgba8UnormSrgb;
        let texture = gpu.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("headless target"),
            size: wgpu::Extent3d {
                width: size.0,
                height: size.1,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        Self {
            gpu,
            size,
            format,
            texture,
        }
    }

    pub fn view(&self) -> wgpu::TextureView {
        self.texture
            .create_view(&wgpu::TextureViewDescriptor::default())
    }

    /// Runs `f` against the target view, then reads the texture back as tightly packed RGBA8.
    pub fn render_with(&self, f: impl FnOnce(&Gpu, &wgpu::TextureView)) -> Vec<u8> {
        let view = self.view();
        f(&self.gpu, &view);
        self.read_back()
    }

    pub fn read_back(&self) -> Vec<u8> {
        let (width, height) = self.size;
        let stride = padded_bytes_per_row(width);
        let buffer = self.gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("headless readback"),
            size: u64::from(stride) * u64::from(height),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut enc = self
            .gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("readback"),
            });
        enc.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &self.texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(stride),
                    rows_per_image: Some(height),
                },
            },
            wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
        );
        self.gpu.queue.submit([enc.finish()]);

        let slice = buffer.slice(..);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |r| {
            let _ = tx.send(r);
        });
        self.gpu
            .device
            .poll(wgpu::PollType::wait_indefinitely())
            .expect("device poll");
        rx.recv().expect("map callback").expect("buffer mapped");

        let data = slice.get_mapped_range().expect("mapped range");
        let row_bytes = (width * 4) as usize;
        let mut out = Vec::with_capacity(row_bytes * height as usize);
        for row in 0..height as usize {
            let start = row * stride as usize;
            out.extend_from_slice(&data[start..start + row_bytes]);
        }
        drop(data);
        buffer.unmap();
        out
    }
}

/// FNV-1a over the pixel bytes; stable across runs for regression tests.
pub fn hash_rgba(pixels: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in pixels {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    h
}
