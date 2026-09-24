//! Owns the frame bindings and the scene list; renders the active scene into a target.
use std::time::Instant;

use onset_core::music_state::MusicState;

use std::path::Path;

use crate::gpu::Gpu;
use crate::hot_reload::ShaderWatcher;
use crate::scene::{FrameBindings, Scene, ShaderError};
use crate::uniforms::FrameUniforms;

#[derive(Debug, Clone, Copy, Default)]
pub struct FrameStats {
    /// CPU time spent recording and submitting the frame.
    pub cpu_ms: f32,
}

pub struct Renderer {
    bindings: FrameBindings,
    scenes: Vec<Box<dyn Scene>>,
    active: usize,
    size: (u32, u32),
    /// The most recent hot-reload failure, shown by the HUD until a good reload clears it.
    last_error: Option<String>,
}

impl Renderer {
    pub fn new(gpu: &Gpu, size: (u32, u32)) -> Self {
        Self {
            bindings: FrameBindings::new(gpu),
            scenes: Vec::new(),
            active: 0,
            size,
            last_error: None,
        }
    }

    pub fn last_error(&self) -> Option<&str> {
        self.last_error.as_deref()
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
        for s in &mut self.scenes {
            s.resize(gpu, size);
        }
    }

    /// Writes the uniforms for `ms` and renders the active scene into `target`.
    pub fn render(
        &mut self,
        gpu: &Gpu,
        encoder: &mut wgpu::CommandEncoder,
        target: &wgpu::TextureView,
        ms: &MusicState,
        time_s: f32,
    ) -> FrameStats {
        let started = Instant::now();
        self.bindings
            .write(gpu, &FrameUniforms::from_state(ms, self.size, time_s));
        if let Some(scene) = self.scenes.get_mut(self.active) {
            scene.render(gpu, encoder, target, &self.bindings);
        }
        FrameStats {
            cpu_ms: started.elapsed().as_secs_f32() * 1000.0,
        }
    }
}
