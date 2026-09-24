//! The scenes that ship with Onset. Each is a WGSL file in `assets/shaders/` rendered by
//! [`FullscreenScene`]; the list order is the menu order.
use std::path::Path;

use crate::gpu::Gpu;
use crate::scene::{FullscreenScene, Scene, ShaderError};

/// File stems under `assets/shaders/`, in menu order.
pub const BUILTIN_SCENE_NAMES: &[&str] = &["pulse", "ring", "warp", "voronoi"];

/// The shipped sources, compiled into the binary so a missing or broken file on disk never
/// takes a scene away from the output.
pub const EMBEDDED_SCENES: &[(&str, &str)] = &[
    (
        "pulse",
        include_str!("../../../../assets/shaders/pulse.wgsl"),
    ),
    ("ring", include_str!("../../../../assets/shaders/ring.wgsl")),
    ("warp", include_str!("../../../../assets/shaders/warp.wgsl")),
    (
        "voronoi",
        include_str!("../../../../assets/shaders/voronoi.wgsl"),
    ),
];

const _: () = assert!(EMBEDDED_SCENES.len() == BUILTIN_SCENE_NAMES.len());

/// What [`builtin_scenes`] produced: the scenes that exist and the file errors it met.
pub struct LoadedScenes {
    pub scenes: Vec<Box<dyn Scene>>,
    /// One entry per scene whose on-disk file failed to compile (the embedded copy was used).
    pub errors: Vec<ShaderError>,
}

fn embedded(name: &str) -> Option<&'static str> {
    EMBEDDED_SCENES
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, src)| *src)
}

/// Loads every built-in scene, preferring the file in `shader_dir` (so edits count) and
/// falling back to the embedded copy when the file is missing or does not compile. A broken
/// file is reported in `errors` for the HUD; nothing here is fatal.
pub fn builtin_scenes(
    gpu: &Gpu,
    format: wgpu::TextureFormat,
    layout: &wgpu::BindGroupLayout,
    shader_dir: &Path,
) -> LoadedScenes {
    let mut scenes: Vec<Box<dyn Scene>> = Vec::with_capacity(BUILTIN_SCENE_NAMES.len());
    let mut errors = Vec::new();
    for name in BUILTIN_SCENE_NAMES {
        let path = shader_dir.join(format!("{name}.wgsl"));
        let from_disk = match std::fs::read_to_string(&path) {
            Ok(src) => match FullscreenScene::new(gpu, name, &src, format, layout) {
                Ok(scene) => Some(scene),
                Err(e) => {
                    tracing::error!("{e}; using the embedded copy");
                    errors.push(e);
                    None
                }
            },
            Err(e) => {
                tracing::warn!(path = %path.display(), "shader file unavailable ({e}); using the embedded copy");
                None
            }
        };
        let scene = from_disk.or_else(|| {
            let src = embedded(name)?;
            match FullscreenScene::new(gpu, name, src, format, layout) {
                Ok(scene) => Some(scene),
                Err(e) => {
                    tracing::error!("embedded {e}");
                    errors.push(e);
                    None
                }
            }
        });
        if let Some(scene) = scene {
            scenes.push(Box::new(scene));
        }
    }
    LoadedScenes { scenes, errors }
}
