//! Owns the frame bindings and the scene list; renders the active scene into a target, then
//! the overlays (Now Playing card, HUD) on top.
use std::path::Path;
use std::time::Instant;

use onset_core::music_state::MusicState;

use crate::card::{Card, CardFrame};
use crate::gpu::Gpu;
use crate::hot_reload::ShaderWatcher;
use crate::hud::{Hud, HudInfo};
use crate::quad::{QuadPipeline, QuadTexture};
use crate::scene::{FrameBindings, Scene, ShaderError};
use crate::text::{TextItem, TextLayer};
use crate::uniforms::FrameUniforms;

#[derive(Debug, Clone, Copy, Default)]
pub struct FrameStats {
    /// CPU time spent recording and submitting the frame.
    pub cpu_ms: f32,
}

/// Lowest internal scale the overlay offers; below this the upscale looks like a mistake.
pub const MIN_INTERNAL_SCALE: f32 = 0.5;

pub struct Renderer {
    format: wgpu::TextureFormat,
    bindings: FrameBindings,
    scenes: Vec<Box<dyn Scene>>,
    active: usize,
    size: (u32, u32),
    /// The most recent hot-reload failure, shown by the HUD until a good reload clears it.
    last_error: Option<String>,
    text: TextLayer,
    card: Card,
    hud: Hud,
    show_card: bool,
    show_hud: bool,
    /// Window DPI factor; the HUD scales with it, the card scales with the frame height.
    scale: f32,
    last_cpu_ms: f32,
    /// Scenes render at `internal_scale` of the output and are blitted up; 1.0 draws direct.
    internal_scale: f32,
    blit: QuadPipeline,
    offscreen: Option<QuadTexture>,
    /// Output nothing but black: the DJ's panic button and the end-of-set fade target.
    blackout: bool,
    last_frame: Option<Instant>,
}

impl Renderer {
    pub fn new(gpu: &Gpu, size: (u32, u32), format: wgpu::TextureFormat) -> Self {
        Self {
            format,
            bindings: FrameBindings::new(gpu),
            scenes: Vec::new(),
            active: 0,
            size,
            last_error: None,
            text: TextLayer::new(gpu, format),
            card: Card::new(gpu, format),
            hud: Hud::new(),
            show_card: true,
            show_hud: false,
            scale: 1.0,
            last_cpu_ms: 0.0,
            internal_scale: 1.0,
            blit: QuadPipeline::new(gpu, format),
            offscreen: None,
            blackout: false,
            last_frame: None,
        }
    }

    pub fn set_blackout(&mut self, on: bool) {
        self.blackout = on;
    }

    pub fn blackout(&self) -> bool {
        self.blackout
    }

    /// Fraction of the output resolution scenes render at, clamped to
    /// [`MIN_INTERNAL_SCALE`]..=1.0.
    pub fn set_internal_scale(&mut self, gpu: &Gpu, scale: f32) {
        self.internal_scale = if scale.is_finite() {
            scale.clamp(MIN_INTERNAL_SCALE, 1.0)
        } else {
            1.0
        };
        self.rebuild_offscreen(gpu);
    }

    pub fn internal_scale(&self) -> f32 {
        self.internal_scale
    }

    /// The size scenes actually render at.
    pub fn internal_size(&self) -> (u32, u32) {
        if self.internal_scale >= 1.0 {
            self.size
        } else {
            (
                ((self.size.0 as f32 * self.internal_scale).round() as u32).max(1),
                ((self.size.1 as f32 * self.internal_scale).round() as u32).max(1),
            )
        }
    }

    fn rebuild_offscreen(&mut self, gpu: &Gpu) {
        let internal = self.internal_size();
        self.offscreen = (self.internal_scale < 1.0)
            .then(|| self.blit.offscreen_target(gpu, internal, self.format));
        for s in &mut self.scenes {
            s.resize(gpu, internal);
        }
    }

    pub fn set_scale(&mut self, scale: f32) {
        self.scale = scale.max(0.1);
    }

    pub fn set_show_card(&mut self, show: bool) {
        self.show_card = show;
    }

    pub fn set_show_hud(&mut self, show: bool) {
        self.show_hud = show;
    }

    pub fn show_hud(&self) -> bool {
        self.show_hud
    }

    pub fn hud(&self) -> &Hud {
        &self.hud
    }

    pub fn last_error(&self) -> Option<&str> {
        self.last_error.as_deref()
    }

    /// Shows a message in the HUD until the next successful reload clears it.
    pub fn set_last_error(&mut self, message: String) {
        self.last_error = Some(message);
    }

    /// Recompiles a fullscreen scene from new WGSL. On failure the old pipeline stays in use
    /// and the message is kept for the HUD; on success the message is cleared.
    pub fn reload_scene(&mut self, gpu: &Gpu, name: &str, source: &str) -> Result<(), ShaderError> {
        self.reload_scene_with_common(gpu, name, crate::scene::COMMON_WGSL, source)
    }

    /// [`Self::reload_scene`] with the on-disk `common.wgsl` as the prelude.
    pub fn reload_scene_with_common(
        &mut self,
        gpu: &Gpu,
        name: &str,
        common: &str,
        source: &str,
    ) -> Result<(), ShaderError> {
        let scene = self
            .scenes
            .iter_mut()
            .find(|s| s.name() == name)
            .and_then(|s| s.as_fullscreen_mut())
            .ok_or_else(|| ShaderError {
                name: name.to_string(),
                message: "no reloadable scene with that name".to_string(),
            })?;
        match scene.replace_shader_with_common(gpu, common, source) {
            Ok(()) => {
                self.last_error = None;
                tracing::info!(scene = name, "shader reloaded");
                Ok(())
            }
            Err(e) => {
                tracing::error!("{e}");
                self.last_error = Some(e.to_string());
                Err(e)
            }
        }
    }

    /// Applies any shader files the watcher reports changed. `common.wgsl` reloads every scene.
    pub fn poll_hot_reload(&mut self, gpu: &Gpu, watcher: &mut ShaderWatcher, shader_dir: &Path) {
        let changed = watcher.poll();
        if changed.is_empty() {
            return;
        }
        let common = std::fs::read_to_string(shader_dir.join("common.wgsl"))
            .unwrap_or_else(|_| crate::scene::COMMON_WGSL.to_string());
        for stem in changed {
            let targets: Vec<String> = if stem == "common" {
                self.scene_names()
            } else {
                vec![stem]
            };
            for name in targets {
                let path = shader_dir.join(format!("{name}.wgsl"));
                match std::fs::read_to_string(&path) {
                    Ok(src) => {
                        let _ = self.reload_scene_with_common(gpu, &name, &common, &src);
                    }
                    Err(e) => tracing::warn!(path = %path.display(), "cannot read shader: {e}"),
                }
            }
        }
    }

    pub fn bindings(&self) -> &FrameBindings {
        &self.bindings
    }

    pub fn add_scene(&mut self, scene: Box<dyn Scene>) {
        self.scenes.push(scene);
    }

    pub fn scene_names(&self) -> Vec<String> {
        self.scenes.iter().map(|s| s.name().to_string()).collect()
    }

    pub fn active_scene(&self) -> Option<&str> {
        self.scenes.get(self.active).map(|s| s.name())
    }

    /// Selects a scene by name; unknown names are ignored and reported.
    pub fn set_scene(&mut self, name: &str) -> bool {
        match self.scenes.iter().position(|s| s.name() == name) {
            Some(i) => {
                self.active = i;
                true
            }
            None => false,
        }
    }

    pub fn next_scene(&mut self) {
        if !self.scenes.is_empty() {
            self.active = (self.active + 1) % self.scenes.len();
        }
    }

    pub fn prev_scene(&mut self) {
        if !self.scenes.is_empty() {
            self.active = (self.active + self.scenes.len() - 1) % self.scenes.len();
        }
    }

    pub fn resize(&mut self, gpu: &Gpu, size: (u32, u32)) {
        self.size = size;
        self.rebuild_offscreen(gpu);
    }

    /// Writes the uniforms for `ms`, renders the active scene into `target`, then the card
    /// and HUD when enabled.
    pub fn render(
        &mut self,
        gpu: &Gpu,
        encoder: &mut wgpu::CommandEncoder,
        target: &wgpu::TextureView,
        ms: &MusicState,
        time_s: f32,
    ) -> FrameStats {
        let started = Instant::now();
        let interval_ms = self
            .last_frame
            .map(|t| started.duration_since(t).as_secs_f32() * 1000.0);
        self.last_frame = Some(started);
        self.hud.record(interval_ms, self.last_cpu_ms);
        if self.blackout || self.scenes.is_empty() {
            clear_to_black(encoder, target);
        }
        if self.blackout {
            let cpu_ms = started.elapsed().as_secs_f32() * 1000.0;
            self.last_cpu_ms = cpu_ms;
            return FrameStats { cpu_ms };
        }
        let internal = self.internal_size();
        self.bindings
            .write(gpu, &FrameUniforms::from_state(ms, internal, time_s));
        match (&self.offscreen, self.scenes.get_mut(self.active)) {
            (Some(off), Some(scene)) => {
                scene.render(gpu, encoder, &off.view, &self.bindings);
                off.place(
                    gpu,
                    [0.0, 0.0, self.size.0 as f32, self.size.1 as f32],
                    self.size,
                    1.0,
                );
                let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("upscale"),
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
                self.blit.draw(&mut pass, off);
            }
            (None, Some(scene)) => scene.render(gpu, encoder, target, &self.bindings),
            (_, None) => {}
        }

        let mut items: Vec<TextItem> = Vec::new();
        self.card
            .update(gpu, ms.track.as_ref().filter(|_| self.show_card), time_s);
        if self.show_card {
            items.extend(self.card.draw(
                gpu,
                encoder,
                target,
                &mut self.text,
                CardFrame {
                    size: self.size,
                    theme: &ms.theme,
                    live_bpm: ms.bpm,
                    time_s,
                },
            ));
        }
        if self.show_hud {
            let info = HudInfo {
                scene: self.active_scene().unwrap_or("-").to_string(),
                size: self.size,
                gpu_ms: None,
                last_error: self.last_error.clone(),
            };
            items.extend(self.hud.items(ms, &info, self.scale));
        }
        if !items.is_empty() {
            match self.text.prepare(gpu, self.size, &items) {
                Ok(()) => {
                    let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                        label: Some("text"),
                        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                            view: target,
                            depth_slice: None,
                            resolve_target: None,
                            ops: wgpu::Operations {
                                load: wgpu::LoadOp::Load,
                                store: wgpu::StoreOp::Store,
                            },
                        })],
                        ..Default::default()
                    });
                    self.text.render(&mut pass);
                }
                Err(e) => tracing::warn!("text prepare: {e}"),
            }
        }
        self.text.trim();

        let cpu_ms = started.elapsed().as_secs_f32() * 1000.0;
        self.last_cpu_ms = cpu_ms;
        FrameStats { cpu_ms }
    }
}

fn clear_to_black(encoder: &mut wgpu::CommandEncoder, target: &wgpu::TextureView) {
    encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
        label: Some("clear"),
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
}
