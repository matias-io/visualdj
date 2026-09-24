//! The output window: borderless fullscreen on the chosen monitor, a wgpu surface, and the
//! frame loop. Rendering content arrives in later tasks; this task proves the window, the
//! surface and the hotkeys.
use std::sync::Arc;
use std::time::{Duration, Instant};

use onset_render::gpu::Gpu;
use winit::application::ApplicationHandler;
use winit::dpi::PhysicalSize;
use winit::event::{ElementState, KeyEvent, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow};
use winit::keyboard::{KeyCode, PhysicalKey};
use winit::monitor::MonitorHandle;
use winit::window::{Fullscreen, Window, WindowAttributes, WindowId};

use crate::config::{Config, MonitorChoice, PresentModeChoice};
use crate::monitors::{MonitorInfo, choose};

pub struct AppOptions {
    pub config: Config,
    /// Command-line override of the configured monitor.
    pub monitor: Option<MonitorChoice>,
    /// Close automatically after this long (for unattended checks).
    pub exit_after: Option<Duration>,
}

struct Surface {
    window: Arc<Window>,
    surface: wgpu::Surface<'static>,
    gpu: Gpu,
    config: wgpu::SurfaceConfiguration,
}

pub struct OnsetApp {
    opts: AppOptions,
    surface: Option<Surface>,
    started: Instant,
    frames: u64,
    fullscreen: bool,
    monitor: Option<MonitorHandle>,
}

fn monitor_infos(event_loop: &ActiveEventLoop) -> (Vec<MonitorHandle>, Vec<MonitorInfo>) {
    let primary = event_loop.primary_monitor();
    let handles: Vec<MonitorHandle> = event_loop.available_monitors().collect();
    let infos = handles
        .iter()
        .map(|m| MonitorInfo {
            name: m.name().unwrap_or_default(),
            is_primary: primary.as_ref() == Some(m),
            size: (m.size().width, m.size().height),
            scale: m.scale_factor(),
        })
        .collect();
    (handles, infos)
}

impl OnsetApp {
    pub fn new(opts: AppOptions) -> Self {
        Self {
            opts,
            surface: None,
            started: Instant::now(),
            frames: 0,
            fullscreen: true,
            monitor: None,
        }
    }

    fn present_mode(&self) -> wgpu::PresentMode {
        match self.opts.config.present_mode {
            PresentModeChoice::Fifo => wgpu::PresentMode::Fifo,
            PresentModeChoice::Mailbox => wgpu::PresentMode::Mailbox,
        }
    }

    fn create_surface(&mut self, event_loop: &ActiveEventLoop) -> anyhow::Result<()> {
        let (handles, infos) = monitor_infos(event_loop);
        let choice = self
            .opts
            .monitor
            .clone()
            .unwrap_or_else(|| self.opts.config.output_monitor.clone());
        let (index, fell_back) = choose(&infos, &choice);
        if fell_back {
            tracing::warn!(?choice, "requested monitor not found, using the primary");
        }
        let monitor = handles.get(index).cloned();
        if let Some(info) = infos.get(index) {
            tracing::info!(
                monitor = %info.name,
                width = info.size.0,
                height = info.size.1,
                scale = info.scale,
                "output monitor"
            );
        }

        let mut attrs = WindowAttributes::default()
            .with_title("Onset")
            .with_decorations(false);
        attrs = if self.fullscreen {
            attrs.with_fullscreen(Some(Fullscreen::Borderless(monitor.clone())))
        } else {
            attrs.with_inner_size(PhysicalSize::new(1280u32, 720u32))
        };
        let window = Arc::new(event_loop.create_window(attrs)?);
        window.set_cursor_visible(false);

        let instance = Gpu::new_instance();
        let surface = instance.create_surface(window.clone())?;
        let gpu = Gpu::new_for_surface(instance, &surface)?;
        let caps = surface.get_capabilities(&gpu.adapter);
        let format = caps
            .formats
            .iter()
            .copied()
            .find(wgpu::TextureFormat::is_srgb)
            .unwrap_or(caps.formats[0]);
        let size = window.inner_size();
        let present_mode = if caps.present_modes.contains(&self.present_mode()) {
            self.present_mode()
        } else {
            wgpu::PresentMode::Fifo
        };
        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            color_space: wgpu::SurfaceColorSpace::Auto,
            width: size.width.max(1),
            height: size.height.max(1),
            present_mode,
            alpha_mode: caps.alpha_modes[0],
            view_formats: vec![],
            desired_maximum_frame_latency: 2,
        };
        surface.configure(&gpu.device, &config);
        tracing::info!(
            ?format,
            ?present_mode,
            width = config.width,
            height = config.height,
            "surface ready"
        );

        self.monitor = monitor;
        self.surface = Some(Surface {
            window,
            surface,
            gpu,
            config,
        });
        Ok(())
    }

    fn resize(&mut self, size: PhysicalSize<u32>) {
        if let Some(s) = self.surface.as_mut()
            && size.width > 0
            && size.height > 0
        {
            s.config.width = size.width;
            s.config.height = size.height;
            s.surface.configure(&s.gpu.device, &s.config);
            tracing::info!(
                width = size.width,
                height = size.height,
                "surface reconfigured"
            );
        }
    }

    fn render(&mut self) {
        let Some(s) = self.surface.as_mut() else {
            return;
        };
        let frame = match s.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(f) => f,
            wgpu::CurrentSurfaceTexture::Suboptimal(f) => {
                // Still presentable; reconfigure so the next frame matches the surface.
                s.surface.configure(&s.gpu.device, &s.config);
                f
            }
            wgpu::CurrentSurfaceTexture::Timeout | wgpu::CurrentSurfaceTexture::Occluded => {
                return;
            }
            wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Lost => {
                s.surface.configure(&s.gpu.device, &s.config);
                return;
            }
            other => {
                tracing::error!("surface acquisition failed: {other:?}");
                return;
            }
        };
        let view = frame
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let mut enc = s
            .gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("frame"),
            });
        {
            // A slow pulse so a human can tell the window is alive before scenes exist.
            let t = self.started.elapsed().as_secs_f64();
            let pulse = 0.04 + 0.03 * (t * 1.5).sin().mul_add(0.5, 0.5);
            let _pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("clear"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: pulse,
                            g: pulse,
                            b: pulse * 1.3,
                            a: 1.0,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                ..Default::default()
            });
        }
        s.gpu.queue.submit([enc.finish()]);
        s.window.pre_present_notify();
        s.gpu.queue.present(frame);
        self.frames += 1;
    }

    fn toggle_fullscreen(&mut self) {
        self.fullscreen = !self.fullscreen;
        if let Some(s) = &self.surface {
            let mode = self
                .fullscreen
                .then(|| Fullscreen::Borderless(self.monitor.clone()));
            s.window.set_fullscreen(mode);
        }
    }

    fn on_key(&mut self, event_loop: &ActiveEventLoop, key: KeyCode) {
        match key {
            KeyCode::Escape => event_loop.exit(),
            KeyCode::KeyF => self.toggle_fullscreen(),
            KeyCode::KeyH => {
                self.opts.config.show_hud = !self.opts.config.show_hud;
                tracing::info!(hud = self.opts.config.show_hud, "toggle");
            }
            KeyCode::KeyC => {
                self.opts.config.show_card = !self.opts.config.show_card;
                tracing::info!(card = self.opts.config.show_card, "toggle");
            }
            _ => {}
        }
    }
}

impl ApplicationHandler for OnsetApp {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.surface.is_none()
            && let Err(e) = self.create_surface(event_loop)
        {
            tracing::error!("cannot create the output window: {e:#}");
            event_loop.exit();
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Resized(size) => self.resize(size),
            WindowEvent::ScaleFactorChanged { .. } => {
                if let Some(s) = &self.surface {
                    let size = s.window.inner_size();
                    self.resize(size);
                }
            }
            WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        physical_key: PhysicalKey::Code(code),
                        state: ElementState::Pressed,
                        repeat: false,
                        ..
                    },
                ..
            } => self.on_key(event_loop, code),
            WindowEvent::RedrawRequested => {
                self.render();
                if let Some(s) = &self.surface {
                    s.window.request_redraw();
                }
            }
            _ => {}
        }
    }

    fn exiting(&mut self, _event_loop: &ActiveEventLoop) {
        if let Err(e) = self.opts.config.save() {
            tracing::warn!("could not save config: {e:#}");
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        if let Some(limit) = self.opts.exit_after
            && self.started.elapsed() >= limit
        {
            let secs = self.started.elapsed().as_secs_f64();
            tracing::info!(
                frames = self.frames,
                fps = self.frames as f64 / secs,
                "exit-after reached"
            );
            event_loop.exit();
        }
        event_loop.set_control_flow(ControlFlow::Poll);
    }
}
