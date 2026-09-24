//! The output window: borderless fullscreen on the chosen monitor, a wgpu surface, and the
//! frame loop that renders the engine's latest `MusicState` through the active scene.
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use onset_core::music_state::MusicState;
use onset_render::assets::shader_dir;
use onset_render::gpu::Gpu;
use onset_render::headless::read_texture;
use onset_render::hot_reload::ShaderWatcher;
use onset_render::renderer::Renderer;
use onset_render::scenes::builtin_scenes;
use winit::application::ApplicationHandler;
use winit::dpi::PhysicalSize;
use winit::event::{ElementState, KeyEvent, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow};
use winit::keyboard::{KeyCode, PhysicalKey};
use winit::monitor::MonitorHandle;
use winit::window::{Fullscreen, Window, WindowAttributes, WindowId, WindowLevel};

use crate::bench::BenchStats;
use crate::config::{Config, MonitorChoice, PresentModeChoice};
use crate::engine::{Engine, EngineCommand, EngineStatus};
use crate::monitors::{MonitorInfo, choose};
use crate::overlay::{DrawResult, DrawTarget, Overlay, OverlayAction, OverlayView};

pub struct AppOptions {
    pub config: Config,
    /// Command-line override of the configured monitor.
    pub monitor: Option<MonitorChoice>,
    /// Close automatically after this long (for unattended checks).
    pub exit_after: Option<Duration>,
    /// Present without `VSync` and print frame statistics on exit.
    pub bench: bool,
    /// Start with the settings panel open.
    pub settings_open: bool,
    /// Save one frame here shortly before exit.
    pub screenshot: Option<PathBuf>,
    pub engine: Option<Engine>,
}

struct Surface {
    window: Arc<Window>,
    target: wgpu::Surface<'static>,
    gpu: Gpu,
    config: wgpu::SurfaceConfiguration,
    renderer: Renderer,
    watcher: Option<ShaderWatcher>,
    overlay: Overlay,
}

pub struct OnsetApp {
    opts: AppOptions,
    surface: Option<Surface>,
    started: Instant,
    frames: u64,
    fullscreen: bool,
    paused: bool,
    rate: f32,
    monitor: Option<MonitorHandle>,
    monitor_names: Vec<String>,
    last_scene_log: Instant,
    last_frame: Option<Instant>,
    /// Frame intervals in ms, kept only in bench mode.
    frame_log: Vec<f32>,
    /// `exit()` was requested; `about_to_wait` can run again before the loop stops.
    exiting: bool,
    /// Where the one requested screenshot goes; taken out once it is written.
    screenshot_pending: Option<PathBuf>,
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

/// Loads the built-in scenes; a shader that fails to compile is reported and skipped so a
/// broken file cannot take the whole output down.
fn load_scenes(gpu: &Gpu, format: wgpu::TextureFormat, renderer: &mut Renderer) {
    let dir = shader_dir();
    match builtin_scenes(gpu, format, &renderer.bindings().layout, &dir) {
        Ok(scenes) => {
            for scene in scenes {
                renderer.add_scene(scene);
            }
        }
        Err(e) => tracing::error!("{e}"),
    }
    tracing::info!(scenes = ?renderer.scene_names(), dir = %dir.display(), "scenes loaded");
}

/// The configured mode; the bench wants the GPU's real pace, so it asks for no `VSync`.
fn wanted_present_mode(bench: bool, choice: PresentModeChoice) -> wgpu::PresentMode {
    if bench {
        return wgpu::PresentMode::Immediate;
    }
    match choice {
        PresentModeChoice::Fifo => wgpu::PresentMode::Fifo,
        PresentModeChoice::Mailbox => wgpu::PresentMode::Mailbox,
    }
}

/// Picks a supported mode, falling back towards `VSync`.
fn pick_present_mode(
    bench: bool,
    choice: PresentModeChoice,
    caps: &wgpu::SurfaceCapabilities,
) -> wgpu::PresentMode {
    let wanted = wanted_present_mode(bench, choice);
    if caps.present_modes.contains(&wanted) {
        return wanted;
    }
    if bench && caps.present_modes.contains(&wgpu::PresentMode::Mailbox) {
        return wgpu::PresentMode::Mailbox;
    }
    wgpu::PresentMode::Fifo
}

/// Reads the frame back and writes it as PNG; failures are logged, never fatal.
fn save_frame(s: &Surface, texture: &wgpu::Texture, path: &std::path::Path) {
    if !s.config.usage.contains(wgpu::TextureUsages::COPY_SRC) {
        tracing::warn!("screenshot skipped: the surface cannot be read back on this adapter");
        return;
    }
    let size = (s.config.width, s.config.height);
    let mut pixels = read_texture(&s.gpu, texture, size);
    let bgra = matches!(
        s.config.format,
        wgpu::TextureFormat::Bgra8Unorm | wgpu::TextureFormat::Bgra8UnormSrgb
    );
    for px in pixels.as_chunks_mut::<4>().0 {
        if bgra {
            px.swap(0, 2);
        }
        px[3] = 255;
    }
    let Some(img) = image::RgbaImage::from_raw(size.0, size.1, pixels) else {
        tracing::warn!("screenshot buffer had the wrong size");
        return;
    };
    match img.save(path) {
        Ok(()) => tracing::info!(path = %path.display(), "screenshot saved"),
        Err(e) => tracing::warn!("screenshot not saved: {e}"),
    }
}

/// Runs the settings panel for this frame when it is open.
fn draw_overlay(
    s: &mut Surface,
    opts: &mut AppOptions,
    monitor_names: &[String],
    enc: &mut wgpu::CommandEncoder,
    view: &wgpu::TextureView,
    ms: &MusicState,
) -> DrawResult {
    if !s.overlay.is_open() {
        return DrawResult {
            buffers: Vec::new(),
            actions: Vec::new(),
            changed: false,
        };
    }
    let idle = EngineStatus::Idle;
    let (engine_status, tracks): (EngineStatus, &[crate::engine::TrackEntry]) =
        match opts.engine.as_ref() {
            Some(e) => (e.status(), e.tracks()),
            None => (idle, &[]),
        };
    let scenes = s.renderer.scene_names();
    let overlay_view = OverlayView {
        monitors: monitor_names,
        scenes: &scenes,
        active_scene: s.renderer.active_scene().unwrap_or("-"),
        tracks,
        status: &engine_status,
        playhead_s: ms.playhead_s,
        duration_s: ms.track.as_ref().and_then(|t| t.duration_s),
        playing: ms.playing,
        frame_ms: s.renderer.hud().last_frame_ms(),
    };
    let target = DrawTarget {
        window: &s.window,
        gpu: &s.gpu,
        view,
        size: (s.config.width, s.config.height),
    };
    s.overlay
        .draw(&target, enc, &mut opts.config, &overlay_view)
}

impl OnsetApp {
    pub fn new(opts: AppOptions) -> Self {
        let screenshot_pending = opts.screenshot.clone();
        Self {
            opts,
            surface: None,
            started: Instant::now(),
            frames: 0,
            fullscreen: true,
            paused: false,
            rate: 1.0,
            monitor: None,
            monitor_names: Vec::new(),
            last_scene_log: Instant::now(),
            last_frame: None,
            frame_log: Vec::new(),
            exiting: false,
            screenshot_pending,
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
            // The projector output must not be covered by stray windows.
            attrs
                .with_fullscreen(Some(Fullscreen::Borderless(monitor.clone())))
                .with_window_level(WindowLevel::AlwaysOnTop)
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
        let present_mode = pick_present_mode(self.opts.bench, self.opts.config.present_mode, &caps);
        // Screenshots copy the presented frame; ask for COPY_SRC only where it is offered.
        let mut usage = wgpu::TextureUsages::RENDER_ATTACHMENT;
        if self.opts.screenshot.is_some() && caps.usages.contains(wgpu::TextureUsages::COPY_SRC) {
            usage |= wgpu::TextureUsages::COPY_SRC;
        }
        let config = wgpu::SurfaceConfiguration {
            usage,
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

        let mut renderer = Renderer::new(&gpu, (config.width, config.height), format);
        renderer.set_scale(window.scale_factor() as f32);
        renderer.set_show_card(self.opts.config.show_card);
        renderer.set_show_hud(self.opts.config.show_hud);
        renderer.set_internal_scale(&gpu, self.opts.config.internal_scale);
        load_scenes(&gpu, format, &mut renderer);
        if !renderer.set_scene(&self.opts.config.scene) {
            tracing::info!(requested = %self.opts.config.scene, "scene not found, using the first");
        }

        let watcher = match ShaderWatcher::new(&shader_dir()) {
            Ok(w) => Some(w),
            Err(e) => {
                tracing::warn!("shader hot reload disabled: {e:#}");
                None
            }
        };

        let mut overlay = Overlay::new(&window, &gpu, format);
        if self.opts.settings_open {
            overlay.set_open(&window, true);
        }
        self.monitor = monitor;
        self.monitor_names = infos.iter().map(|m| m.name.clone()).collect();
        self.surface = Some(Surface {
            window,
            target: surface,
            gpu,
            config,
            renderer,
            watcher,
            overlay,
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
            s.target.configure(&s.gpu.device, &s.config);
            s.renderer.resize(&s.gpu, (size.width, size.height));
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
        if let Some(w) = s.watcher.as_mut() {
            s.renderer.poll_hot_reload(&s.gpu, w, &shader_dir());
        }
        let frame = match s.target.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(f) => f,
            wgpu::CurrentSurfaceTexture::Suboptimal(f) => {
                s.target.configure(&s.gpu.device, &s.config);
                f
            }
            wgpu::CurrentSurfaceTexture::Timeout | wgpu::CurrentSurfaceTexture::Occluded => {
                return;
            }
            wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Lost => {
                s.target.configure(&s.gpu.device, &s.config);
                return;
            }
            wgpu::CurrentSurfaceTexture::Validation => {
                tracing::error!("surface acquisition failed validation");
                return;
            }
        };
        let view = frame
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let ms: Arc<MusicState> = self
            .opts
            .engine
            .as_ref()
            .map_or_else(|| Arc::new(MusicState::default()), Engine::state);
        let mut enc = s
            .gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("frame"),
            });
        let time_s = self.started.elapsed().as_secs_f32();
        let stats = s.renderer.render(&s.gpu, &mut enc, &view, &ms, time_s);

        let DrawResult {
            buffers,
            actions: pending,
            changed: save,
        } = draw_overlay(s, &mut self.opts, &self.monitor_names, &mut enc, &view, &ms);
        s.gpu
            .queue
            .submit(buffers.into_iter().chain(std::iter::once(enc.finish())));
        if self.screenshot_due()
            && let Some(path) = self.screenshot_pending.take()
        {
            let s = self.surface.as_ref().expect("surface exists");
            save_frame(s, &frame.texture, &path);
        }
        let s = self.surface.as_mut().expect("surface exists");
        s.window.pre_present_notify();
        s.gpu.queue.present(frame);
        self.frames += 1;
        tracing::trace!(cpu_ms = stats.cpu_ms, "frame recorded");

        let now = Instant::now();
        if let Some(last) = self.last_frame
            && self.opts.bench
        {
            self.frame_log
                .push(now.duration_since(last).as_secs_f32() * 1000.0);
        }
        self.last_frame = Some(now);

        // A once-a-second line so an unattended run leaves a trace of what it showed.
        if self.last_scene_log.elapsed() >= Duration::from_secs(1) {
            self.last_scene_log = Instant::now();
            tracing::debug!(
                scene = s.renderer.active_scene().unwrap_or("-"),
                playhead = ms.playhead_s,
                phrase = ?ms.phrase,
                drop = ?ms.drop_countdown_beats,
                intensity = ms.intensity,
                "frame"
            );
        }

        for action in pending {
            self.apply(action);
        }
        if save && let Err(e) = self.opts.config.save() {
            tracing::warn!("could not save config: {e:#}");
        }
    }

    /// A second before exit, or three seconds in when the run is open-ended.
    fn screenshot_due(&self) -> bool {
        if self.screenshot_pending.is_none() {
            return false;
        }
        let at = self
            .opts
            .exit_after
            .map_or(3.0, |d| (d.as_secs_f32() - 1.0).max(0.5));
        self.started.elapsed().as_secs_f32() >= at
    }

    /// Applies one settings-panel edit to the running app.
    fn apply(&mut self, action: OverlayAction) {
        match action {
            OverlayAction::Scene(name) => {
                if let Some(s) = self.surface.as_mut()
                    && s.renderer.set_scene(&name)
                {
                    tracing::info!(scene = %name, "switched");
                }
            }
            OverlayAction::PresentMode(_) => {
                let (bench, choice) = (self.opts.bench, self.opts.config.present_mode);
                if let Some(s) = self.surface.as_mut() {
                    let caps = s.target.get_capabilities(&s.gpu.adapter);
                    s.config.present_mode = pick_present_mode(bench, choice, &caps);
                    s.target.configure(&s.gpu.device, &s.config);
                    tracing::info!(mode = ?s.config.present_mode, "present mode");
                }
            }
            OverlayAction::InternalScale(scale) => {
                if let Some(s) = self.surface.as_mut() {
                    s.renderer.set_internal_scale(&s.gpu, scale);
                }
            }
            OverlayAction::ShowHud(on) => {
                if let Some(s) = self.surface.as_mut() {
                    s.renderer.set_show_hud(on);
                }
            }
            OverlayAction::ShowCard(on) => {
                if let Some(s) = self.surface.as_mut() {
                    s.renderer.set_show_card(on);
                }
            }
            OverlayAction::Engine(cmd) => {
                if let EngineCommand::Pause = cmd {
                    self.paused = true;
                }
                if let EngineCommand::Resume = cmd {
                    self.paused = false;
                }
                if let EngineCommand::SetRate(r) = cmd {
                    self.rate = r;
                }
                if let Some(e) = &self.opts.engine {
                    e.command(cmd);
                }
            }
        }
    }

    fn toggle_fullscreen(&mut self) {
        self.fullscreen = !self.fullscreen;
        if let Some(s) = &self.surface {
            let mode = self
                .fullscreen
                .then(|| Fullscreen::Borderless(self.monitor.clone()));
            s.window.set_fullscreen(mode);
            s.window.set_window_level(if self.fullscreen {
                WindowLevel::AlwaysOnTop
            } else {
                WindowLevel::Normal
            });
        }
    }

    fn on_key(&mut self, event_loop: &ActiveEventLoop, key: KeyCode) {
        match key {
            KeyCode::Escape => event_loop.exit(),
            KeyCode::KeyF => self.toggle_fullscreen(),
            KeyCode::KeyH => {
                self.opts.config.show_hud = !self.opts.config.show_hud;
                if let Some(s) = self.surface.as_mut() {
                    s.renderer.set_show_hud(self.opts.config.show_hud);
                }
                tracing::info!(hud = self.opts.config.show_hud, "toggle");
            }
            KeyCode::KeyC => {
                self.opts.config.show_card = !self.opts.config.show_card;
                if let Some(s) = self.surface.as_mut() {
                    s.renderer.set_show_card(self.opts.config.show_card);
                }
                tracing::info!(card = self.opts.config.show_card, "toggle");
            }
            KeyCode::Space => {
                self.paused = !self.paused;
                if let Some(e) = &self.opts.engine {
                    e.command(if self.paused {
                        EngineCommand::Pause
                    } else {
                        EngineCommand::Resume
                    });
                }
            }
            KeyCode::Home => {
                if let Some(e) = &self.opts.engine {
                    e.command(EngineCommand::Seek(0.0));
                }
            }
            KeyCode::BracketLeft | KeyCode::BracketRight => {
                self.rate += if key == KeyCode::BracketRight {
                    0.01
                } else {
                    -0.01
                };
                self.rate = self.rate.clamp(0.5, 1.5);
                if let Some(e) = &self.opts.engine {
                    e.command(EngineCommand::SetRate(self.rate));
                }
                tracing::info!(rate = self.rate, "sim rate");
            }
            KeyCode::KeyR => {
                if let (Some(e), Some(title)) = (&self.opts.engine, &self.opts.config.sim_track) {
                    e.command(EngineCommand::LoadTrack(title.clone()));
                }
            }
            KeyCode::ArrowRight | KeyCode::ArrowLeft => {
                if let Some(s) = self.surface.as_mut() {
                    if key == KeyCode::ArrowRight {
                        s.renderer.next_scene();
                    } else {
                        s.renderer.prev_scene();
                    }
                    if let Some(name) = s.renderer.active_scene() {
                        self.opts.config.scene = name.to_string();
                        tracing::info!(scene = name, "switched");
                    }
                }
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
        if let WindowEvent::KeyboardInput {
            event:
                KeyEvent {
                    physical_key: PhysicalKey::Code(KeyCode::Tab),
                    state: ElementState::Pressed,
                    repeat: false,
                    ..
                },
            ..
        } = &event
        {
            if let Some(s) = self.surface.as_mut() {
                s.overlay.toggle(&s.window);
            }
            return;
        }
        let consumed = self
            .surface
            .as_mut()
            .is_some_and(|s| s.overlay.on_event(&s.window, &event));
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Resized(size) => self.resize(size),
            WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                if let Some(s) = self.surface.as_mut() {
                    s.renderer.set_scale(scale_factor as f32);
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
            } if !consumed => self.on_key(event_loop, code),
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
            && !self.exiting
            && self.started.elapsed() >= limit
        {
            self.exiting = true;
            let secs = self.started.elapsed().as_secs_f64();
            tracing::info!(
                frames = self.frames,
                fps = self.frames as f64 / secs,
                "exit-after reached"
            );
            if self.opts.bench {
                let stats = BenchStats::from_intervals(&self.frame_log);
                let (scene, size) = self
                    .surface
                    .as_ref()
                    .map_or(("-".to_string(), (0, 0)), |s| {
                        (
                            s.renderer.active_scene().unwrap_or("-").to_string(),
                            (s.config.width, s.config.height),
                        )
                    });
                println!("{}", stats.line(&scene, size));
            }
            event_loop.exit();
        }
        event_loop.set_control_flow(ControlFlow::Poll);
    }
}
