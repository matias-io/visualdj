//! The in-window settings panel (egui), toggled with `Tab`. It edits the `Config` directly
//! and reports the runtime effects the app has to apply.
use std::sync::Arc;

use egui_wgpu::{Renderer as EguiRenderer, RendererOptions, ScreenDescriptor};
use onset_render::gpu::Gpu;
use winit::event::WindowEvent;
use winit::window::Window;

use crate::config::{Config, MonitorChoice, PresentModeChoice};
use crate::engine::{EngineCommand, EngineStatus, TrackEntry};

/// Most tracks the search list shows at once.
pub const LIST_CAP: usize = 60;

/// Panel magnification on top of the window's DPI scale.
pub const PANEL_ZOOM: f32 = 1.35;

/// Runtime effects of an edit; config persistence is the caller's job.
#[derive(Debug, Clone, PartialEq)]
pub enum OverlayAction {
    Scene(String),
    PresentMode(PresentModeChoice),
    InternalScale(f32),
    ShowHud(bool),
    ShowCard(bool),
    Engine(EngineCommand),
}

/// Read-only state the panel displays.
pub struct OverlayView<'a> {
    pub monitors: &'a [String],
    pub scenes: &'a [String],
    pub active_scene: &'a str,
    pub tracks: &'a [TrackEntry],
    pub status: &'a EngineStatus,
    pub playhead_s: f64,
    pub duration_s: Option<f32>,
    pub playing: bool,
    pub frame_ms: f32,
}

/// Where a frame's panel is drawn.
pub struct DrawTarget<'a> {
    pub window: &'a Window,
    pub gpu: &'a Gpu,
    pub view: &'a wgpu::TextureView,
    /// Output size in physical pixels.
    pub size: (u32, u32),
}

/// What [`Overlay::draw`] hands back for one frame.
pub struct DrawResult {
    /// Submit these before the frame's own command buffer.
    pub buffers: Vec<wgpu::CommandBuffer>,
    pub actions: Vec<OverlayAction>,
    /// The config changed and should be saved.
    pub changed: bool,
}

/// Edits collected while the panel runs.
#[derive(Default)]
struct Edits {
    actions: Vec<OverlayAction>,
    changed: bool,
}

impl Edits {
    fn push(&mut self, action: OverlayAction) {
        self.actions.push(action);
        self.changed = true;
    }
}

pub struct Overlay {
    ctx: egui::Context,
    state: egui_winit::State,
    renderer: EguiRenderer,
    open: bool,
    search: String,
    endpoints: Vec<String>,
    rate: f32,
    seek: f32,
    dragging_seek: bool,
}

/// Case-insensitive substring match on the label, capped so the panel stays small.
pub fn filter_tracks<'a>(tracks: &'a [TrackEntry], query: &str, cap: usize) -> Vec<&'a TrackEntry> {
    let q = query.trim().to_lowercase();
    tracks
        .iter()
        .filter(|t| q.is_empty() || t.label.to_lowercase().contains(&q))
        .take(cap)
        .collect()
}

fn monitor_label(monitors: &[String], choice: &MonitorChoice) -> String {
    match choice {
        MonitorChoice::Primary => "Primary".to_string(),
        MonitorChoice::Index(i) => monitors
            .get(*i)
            .map_or_else(|| format!("#{i} (not connected)"), |n| format!("#{i} {n}")),
        MonitorChoice::NameContains(s) => format!("name contains {s:?}"),
    }
}

fn status_text(status: &EngineStatus) -> String {
    match status {
        EngineStatus::Starting => "starting".to_string(),
        EngineStatus::Idle => "idle (no track loaded)".to_string(),
        EngineStatus::Running { source, track } => format!("{source}: {track}"),
        EngineStatus::Error(e) => format!("error: {e}"),
    }
}

impl Overlay {
    pub fn new(window: &Arc<Window>, gpu: &Gpu, format: wgpu::TextureFormat) -> Self {
        let ctx = egui::Context::default();
        ctx.set_visuals(egui::Visuals::dark());
        // The panel is read from a DJ booth, not a desk: a third larger than egui's default.
        ctx.set_zoom_factor(PANEL_ZOOM);
        let state = egui_winit::State::new(
            ctx.clone(),
            egui::ViewportId::ROOT,
            window.as_ref(),
            Some(window.scale_factor() as f32),
            None,
            None,
        );
        let renderer = EguiRenderer::new(&gpu.device, format, RendererOptions::default());
        Self {
            ctx,
            state,
            renderer,
            open: false,
            search: String::new(),
            endpoints: onset_audio::capture::list_output_endpoints(),
            rate: 1.0,
            seek: 0.0,
            dragging_seek: false,
        }
    }

    pub fn is_open(&self) -> bool {
        self.open
    }

    pub fn toggle(&mut self, window: &Window) {
        self.set_open(window, !self.open);
    }

    pub fn set_open(&mut self, window: &Window, open: bool) {
        self.open = open;
        window.set_cursor_visible(self.open);
        if self.open {
            self.endpoints = onset_audio::capture::list_output_endpoints();
        }
        tracing::info!(open = self.open, "settings panel");
    }

    /// Feeds an event to egui while the panel is open; true when egui wants it exclusively.
    pub fn on_event(&mut self, window: &Window, event: &WindowEvent) -> bool {
        if !self.open {
            return false;
        }
        self.state.on_window_event(window, event).consumed
    }

    /// Runs the UI and records its draw into `encoder` over the target.
    pub fn draw(
        &mut self,
        target: &DrawTarget<'_>,
        encoder: &mut wgpu::CommandEncoder,
        config: &mut Config,
        view: &OverlayView<'_>,
    ) -> DrawResult {
        let mut edits = Edits::default();
        if !self.open {
            return DrawResult {
                buffers: Vec::new(),
                actions: edits.actions,
                changed: edits.changed,
            };
        }
        if !self.dragging_seek {
            self.seek = view.playhead_s as f32;
        }

        let raw = self.state.take_egui_input(target.window);
        self.ctx.begin_pass(raw);
        self.panel(config, view, &mut edits);
        let mut out = self.ctx.end_pass();
        self.state
            .handle_platform_output(target.window, out.platform_output);

        let gpu = target.gpu;
        let screen = ScreenDescriptor {
            size_in_pixels: [target.size.0, target.size.1],
            pixels_per_point: out.pixels_per_point,
        };
        let primitives = self.ctx.tessellate(out.shapes, out.pixels_per_point);
        // Deltas are drained, not borrowed: epaint asserts every one was handled on drop.
        for (id, deltas) in out.textures_delta.set.drain() {
            for delta in deltas {
                self.renderer
                    .update_texture(&gpu.device, &gpu.queue, id, &delta);
            }
        }
        let buffers =
            self.renderer
                .update_buffers(&gpu.device, &gpu.queue, encoder, &primitives, &screen);
        {
            let pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("overlay"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: target.view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    },
                })],
                ..Default::default()
            });
            self.renderer
                .render(&mut pass.forget_lifetime(), &primitives, &screen);
        }
        for id in out.textures_delta.free.drain() {
            self.renderer.free_texture(&id);
        }
        DrawResult {
            buffers,
            actions: edits.actions,
            changed: edits.changed,
        }
    }

    fn panel(&mut self, config: &mut Config, view: &OverlayView<'_>, edits: &mut Edits) {
        let ctx = self.ctx.clone();
        egui::Window::new("Onset")
            .default_width(380.0)
            .collapsible(false)
            .resizable(false)
            .show(&ctx, |ui| {
                ui.label(format!("frame {:.1} ms", view.frame_ms));
                ui.separator();
                ui.heading("Output");
                output_section(ui, config, view, edits);
                ui.separator();
                ui.heading("Scene");
                scene_section(ui, config, view, edits);
                ui.separator();
                ui.heading("Transport");
                self.transport_section(ui, config, view, edits);
                ui.separator();
                ui.heading("Audio");
                audio_section(ui, config, &self.endpoints, edits);
                ui.separator();
                ui.small(
                    "Tab hides this panel  ·  H HUD  ·  C card  ·  B blackout  ·  Left/Right scene  ·  F fullscreen  ·  Esc closes, then quits",
                );
            });
    }

    fn transport_section(
        &mut self,
        ui: &mut egui::Ui,
        config: &mut Config,
        view: &OverlayView<'_>,
        edits: &mut Edits,
    ) {
        ui.label(status_text(view.status));
        ui.horizontal(|ui| {
            if ui
                .button(if view.playing { "Pause" } else { "Play" })
                .clicked()
            {
                edits.actions.push(OverlayAction::Engine(if view.playing {
                    EngineCommand::Pause
                } else {
                    EngineCommand::Resume
                }));
            }
            let duration = view.duration_s.unwrap_or(0.0).max(1.0);
            let label = format!("{:.1} s", self.seek);
            let seek = ui.add(
                egui::Slider::new(&mut self.seek, 0.0..=duration)
                    .show_value(false)
                    .text(label),
            );
            if seek.drag_started() {
                self.dragging_seek = true;
            }
            if seek.drag_stopped() {
                self.dragging_seek = false;
                edits
                    .actions
                    .push(OverlayAction::Engine(EngineCommand::Seek(f64::from(
                        self.seek,
                    ))));
            }
        });
        if ui
            .add(egui::Slider::new(&mut self.rate, 0.5..=1.5).text("Rate"))
            .changed()
        {
            edits
                .actions
                .push(OverlayAction::Engine(EngineCommand::SetRate(self.rate)));
        }
        ui.horizontal(|ui| {
            ui.label("Load");
            ui.text_edit_singleline(&mut self.search);
        });
        egui::ScrollArea::vertical()
            .max_height(160.0)
            .show(ui, |ui| {
                for entry in filter_tracks(view.tracks, &self.search, LIST_CAP) {
                    let current = config.sim_track.as_deref() == Some(entry.title.as_str());
                    if ui.selectable_label(current, &entry.label).clicked() {
                        config.sim_track = Some(entry.title.clone());
                        edits.push(OverlayAction::Engine(EngineCommand::LoadTrack(
                            entry.title.clone(),
                        )));
                    }
                }
            });
    }
}

fn output_section(
    ui: &mut egui::Ui,
    config: &mut Config,
    view: &OverlayView<'_>,
    edits: &mut Edits,
) {
    egui::ComboBox::from_label("Monitor")
        .selected_text(monitor_label(view.monitors, &config.output_monitor))
        .show_ui(ui, |ui| {
            let primary = config.output_monitor == MonitorChoice::Primary;
            if ui.selectable_label(primary, "Primary").clicked() {
                config.output_monitor = MonitorChoice::Primary;
                edits.changed = true;
            }
            for (i, name) in view.monitors.iter().enumerate() {
                let selected = config.output_monitor == MonitorChoice::Index(i);
                if ui
                    .selectable_label(selected, format!("#{i} {name}"))
                    .clicked()
                {
                    config.output_monitor = MonitorChoice::Index(i);
                    edits.changed = true;
                }
            }
        });
    ui.small("Applies at the next start.");
    ui.horizontal(|ui| {
        ui.label("Present");
        for (mode, name) in [
            (PresentModeChoice::Fifo, "VSync"),
            (PresentModeChoice::Mailbox, "Mailbox"),
        ] {
            if ui.radio(config.present_mode == mode, name).clicked() && config.present_mode != mode
            {
                config.present_mode = mode;
                edits.push(OverlayAction::PresentMode(mode));
            }
        }
    });
    let mut scale = config.internal_scale;
    let range = onset_render::renderer::MIN_INTERNAL_SCALE..=1.0;
    if ui
        .add(
            egui::Slider::new(&mut scale, range)
                .text("Internal scale")
                .step_by(0.05),
        )
        .changed()
    {
        config.internal_scale = scale;
        edits.push(OverlayAction::InternalScale(scale));
    }
}

fn scene_section(
    ui: &mut egui::Ui,
    config: &mut Config,
    view: &OverlayView<'_>,
    edits: &mut Edits,
) {
    ui.horizontal_wrapped(|ui| {
        for scene in view.scenes {
            if ui
                .selectable_label(scene == view.active_scene, scene)
                .clicked()
            {
                config.scene.clone_from(scene);
                edits.push(OverlayAction::Scene(scene.clone()));
            }
        }
    });
    ui.horizontal(|ui| {
        if ui.checkbox(&mut config.show_hud, "HUD").changed() {
            edits.push(OverlayAction::ShowHud(config.show_hud));
        }
        if ui
            .checkbox(&mut config.show_card, "Now Playing card")
            .changed()
        {
            edits.push(OverlayAction::ShowCard(config.show_card));
        }
    });
}

fn audio_section(ui: &mut egui::Ui, config: &mut Config, endpoints: &[String], edits: &mut Edits) {
    let current = config
        .audio_device
        .clone()
        .unwrap_or_else(|| "System default".to_string());
    egui::ComboBox::from_label("Loopback device")
        .selected_text(current)
        .show_ui(ui, |ui| {
            if ui
                .selectable_label(config.audio_device.is_none(), "System default")
                .clicked()
            {
                config.audio_device = None;
                edits.changed = true;
            }
            for name in endpoints {
                let selected = config.audio_device.as_deref() == Some(name.as_str());
                if ui.selectable_label(selected, name).clicked() {
                    config.audio_device = Some(name.clone());
                    edits.changed = true;
                }
            }
        });
    ui.small("Applies at the next start.");
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entries() -> Vec<TrackEntry> {
        [
            "Adam Port — Move",
            "Coldplay — Adventure of a Lifetime",
            "CKay — love nwantiti",
        ]
        .iter()
        .map(|l| TrackEntry {
            title: l.split(" — ").nth(1).unwrap().to_string(),
            label: (*l).to_string(),
        })
        .collect()
    }

    #[test]
    fn filter_is_case_insensitive_and_capped() {
        let e = entries();
        assert_eq!(filter_tracks(&e, "ADVENTURE", 10).len(), 1);
        assert_eq!(filter_tracks(&e, "", 10).len(), 3);
        assert_eq!(filter_tracks(&e, "", 2).len(), 2);
        assert!(filter_tracks(&e, "nothing here", 10).is_empty());
    }

    #[test]
    fn monitor_labels_name_the_choice() {
        let m = vec!["DISPLAY1".to_string(), "DISPLAY2".to_string()];
        assert_eq!(monitor_label(&m, &MonitorChoice::Index(1)), "#1 DISPLAY2");
        assert_eq!(
            monitor_label(&m, &MonitorChoice::Index(5)),
            "#5 (not connected)"
        );
        assert_eq!(monitor_label(&m, &MonitorChoice::Primary), "Primary");
    }

    #[test]
    fn status_text_names_the_source_and_track() {
        let s = EngineStatus::Running {
            source: "sim".into(),
            track: "Adam Port - Move".into(),
        };
        assert_eq!(status_text(&s), "sim: Adam Port - Move");
        assert!(status_text(&EngineStatus::Idle).starts_with("idle"));
    }
}
