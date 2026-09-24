//! Owns the frame bindings and the scene list; renders the active scene into a target.
use std::time::Instant;

use onset_core::music_state::MusicState;

use crate::gpu::Gpu;
use crate::scene::{FrameBindings, Scene};
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
}

impl Renderer {
    pub fn new(gpu: &Gpu, size: (u32, u32)) -> Self {
        Self {
            bindings: FrameBindings::new(gpu),
            scenes: Vec::new(),
            active: 0,
            size,
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
