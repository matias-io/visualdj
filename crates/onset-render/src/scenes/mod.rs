//! The scenes that ship with Onset. Each is a WGSL file in `assets/shaders/` rendered by
//! [`FullscreenScene`]; the list order is the menu order.
use std::path::Path;

use crate::gpu::Gpu;
use crate::scene::{FullscreenScene, Scene, ShaderError};

/// File stems under `assets/shaders/`, in menu order.
pub const BUILTIN_SCENE_NAMES: &[&str] = &["pulse", "ring", "warp", "voronoi"];

/// Loads every built-in scene from `shader_dir`. Fails on the first shader that does not
/// compile, naming it, so a broken shipped shader is caught by tests rather than at a gig.
pub fn builtin_scenes(
    gpu: &Gpu,
    format: wgpu::TextureFormat,
    layout: &wgpu::BindGroupLayout,
    shader_dir: &Path,
) -> Result<Vec<Box<dyn Scene>>, ShaderError> {
    let mut scenes: Vec<Box<dyn Scene>> = Vec::with_capacity(BUILTIN_SCENE_NAMES.len());
    for name in BUILTIN_SCENE_NAMES {
        let path = shader_dir.join(format!("{name}.wgsl"));
        let source = std::fs::read_to_string(&path).map_err(|e| ShaderError {
            name: (*name).to_string(),
            message: format!("cannot read {}: {e}", path.display()),
        })?;
        scenes.push(Box::new(FullscreenScene::new(
            gpu, name, &source, format, layout,
        )?));
    }
    Ok(scenes)
}
