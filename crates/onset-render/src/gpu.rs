//! Instance, adapter, device and queue. One `Gpu` per process; scenes borrow it.
use anyhow::Context;

pub struct Gpu {
    pub instance: wgpu::Instance,
    pub adapter: wgpu::Adapter,
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
    /// Whether `Features::TIMESTAMP_QUERY` was enabled (for GPU timing in the HUD).
    pub supports_timestamps: bool,
}

impl Gpu {
    fn instance() -> wgpu::Instance {
        // Windows needs no display handle (that is for X11/Wayland surfaces).
        let mut desc = wgpu::InstanceDescriptor::new_without_display_handle();
        desc.backends = wgpu::Backends::DX12 | wgpu::Backends::VULKAN;
        wgpu::Instance::new(desc)
    }

    /// A GPU without a window. `prefer_software` asks for the fallback adapter (WARP on
    /// Windows), which lets tests run on machines with no usable hardware adapter.
    pub fn new_headless(prefer_software: bool) -> anyhow::Result<Self> {
        let instance = Self::instance();
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            force_fallback_adapter: prefer_software,
            compatible_surface: None,
            ..Default::default()
        }))
        .or_else(|_| {
            // Fall back to whatever exists if the preferred kind is missing.
            pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::None,
                force_fallback_adapter: false,
                compatible_surface: None,
                ..Default::default()
            }))
        })
        .context("no GPU adapter (DX12/Vulkan hardware or WARP)")?;
        Self::with_adapter(instance, adapter)
    }

    /// A GPU able to present to `surface`.
    pub fn new_for_surface(
        instance: wgpu::Instance,
        surface: &wgpu::Surface<'_>,
    ) -> anyhow::Result<Self> {
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            force_fallback_adapter: false,
            compatible_surface: Some(surface),
            ..Default::default()
        }))
        .context("no GPU adapter compatible with the window")?;
        Self::with_adapter(instance, adapter)
    }

    pub fn new_instance() -> wgpu::Instance {
        Self::instance()
    }

    fn with_adapter(instance: wgpu::Instance, adapter: wgpu::Adapter) -> anyhow::Result<Self> {
        let info = adapter.get_info();
        let supports_timestamps = adapter.features().contains(wgpu::Features::TIMESTAMP_QUERY);
        let required_features = if supports_timestamps {
            wgpu::Features::TIMESTAMP_QUERY
        } else {
            wgpu::Features::empty()
        };
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("onset"),
            required_features,
            ..Default::default()
        }))
        .context("device creation failed")?;
        tracing::info!(
            adapter = %info.name,
            backend = ?info.backend,
            kind = ?info.device_type,
            timestamps = supports_timestamps,
            "gpu ready"
        );
        Ok(Self {
            instance,
            adapter,
            device,
            queue,
            supports_timestamps,
        })
    }
}
