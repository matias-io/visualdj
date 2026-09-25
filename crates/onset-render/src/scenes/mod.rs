//! The scenes that ship with Onset. Each is a WGSL file in `assets/shaders/` rendered by
//! [`FullscreenScene`]; the list order is the menu order.
use std::path::Path;

use crate::gpu::Gpu;
use crate::scene::{FullscreenScene, Scene, ShaderError};

/// File stems under `assets/shaders/`, in menu order.
pub const BUILTIN_SCENE_NAMES: &[&str] = &["tunnel", "synthwave", "ribbons", "kaleido", "lasers", "nebula", "zerog", "crystal", "liquid", "cover", "pulse", "ring", "warp", "voronoi"];

/// What a scene is, for the launcher and Auto mode.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SceneInfo {
    pub name: &'static str,
    pub title: &'static str,
    /// One line for the launcher: what it looks like and what drives it.
    pub blurb: &'static str,
    /// 0 calm .. 1 frantic.
    pub energy: f32,
    /// 0 bright .. 1 dark.
    pub darkness: f32,
    /// 1 light, 2 medium, 3 heavy (High quality and up only in Auto mode).
    pub cost: u8,
    /// In the Auto rotation unless the DJ changes it.
    pub auto: bool,
}

pub const SCENES: &[SceneInfo] = &[
    SceneInfo {
        name: "tunnel",
        title: "Neon Tunnel",
        blurb: "Fly through glowing diamond frames. Bass pushes the speed, mids twist the frames, highs spark.",
        energy: 0.85,
        darkness: 0.55,
        cost: 2,
        auto: true,
    },
    SceneInfo {
        name: "synthwave",
        title: "Outrun",
        blurb: "Retro sun over a neon grid. The mountains are the last two seconds of the spectrum.",
        energy: 0.6,
        darkness: 0.45,
        cost: 1,
        auto: true,
    },
    SceneInfo {
        name: "ribbons",
        title: "Silk",
        blurb: "Waves of light lines shaped by the spectrum, rippling on every kick.",
        energy: 0.45,
        darkness: 0.4,
        cost: 2,
        auto: true,
    },
    SceneInfo {
        name: "kaleido",
        title: "Mandala",
        blurb: "Psychedelic kaleidoscope swirl; segments multiply at the drop.",
        energy: 0.7,
        darkness: 0.25,
        cost: 1,
        auto: true,
    },
    SceneInfo {
        name: "lasers",
        title: "Laser Show",
        blurb: "Beams sweep through haze from a truss, one per frequency band; fans open at the drop.",
        energy: 0.95,
        darkness: 0.55,
        cost: 2,
        auto: true,
    },
    SceneInfo {
        name: "nebula",
        title: "Deep Space",
        blurb: "Slow flight through a volumetric nebula that breathes with the bass. For dark, slow tracks.",
        energy: 0.15,
        darkness: 0.9,
        cost: 3,
        auto: true,
    },
    SceneInfo {
        name: "zerog",
        title: "Zero Gravity",
        blurb: "Tumbling debris drifting in deep space. Bass breathes the field outward, the build-up draws it in, the drop throws it apart.",
        energy: 0.35,
        darkness: 0.85,
        cost: 2,
        auto: true,
    },
    SceneInfo {
        name: "crystal",
        title: "Crystal",
        blurb: "Flying through a lattice of glassy neon crystals, each lit by its own slice of the spectrum.",
        energy: 0.75,
        darkness: 0.5,
        cost: 3,
        auto: true,
    },
    SceneInfo {
        name: "liquid",
        title: "Liquid",
        blurb: "Chrome liquid with specular light; the surface flows with the bass and shimmers with the highs.",
        energy: 0.4,
        darkness: 0.35,
        cost: 2,
        auto: true,
    },
    SceneInfo {
        name: "cover",
        title: "Cover Art",
        blurb: "The track's own artwork, mirrored, warped by the spectrum and shattered at the drop.",
        energy: 0.55,
        darkness: 0.4,
        cost: 1,
        auto: true,
    },
    SceneInfo {
        name: "warp",
        title: "Warp (classic)",
        blurb: "The original flowing warp.",
        energy: 0.5,
        darkness: 0.4,
        cost: 1,
        auto: false,
    },
    SceneInfo {
        name: "voronoi",
        title: "Cells (classic)",
        blurb: "The original Voronoi cells.",
        energy: 0.5,
        darkness: 0.4,
        cost: 1,
        auto: false,
    },
    SceneInfo {
        name: "pulse",
        title: "Pulse (classic)",
        blurb: "Minimal beat pulse. Light on the GPU.",
        energy: 0.3,
        darkness: 0.5,
        cost: 1,
        auto: false,
    },
    SceneInfo {
        name: "ring",
        title: "Ring (classic)",
        blurb: "Minimal spectrum ring. Light on the GPU.",
        energy: 0.4,
        darkness: 0.5,
        cost: 1,
        auto: false,
    },
];

/// The registry entry for a scene, if it has one.
pub fn scene_info(name: &str) -> Option<&'static SceneInfo> {
    SCENES.iter().find(|s| s.name == name)
}

/// The shipped sources, compiled into the binary so a missing or broken file on disk never
/// takes a scene away from the output.
pub const EMBEDDED_SCENES: &[(&str, &str)] = &[
    (
        "tunnel",
        include_str!("../../../../assets/shaders/tunnel.wgsl"),
    ),
    (
        "synthwave",
        include_str!("../../../../assets/shaders/synthwave.wgsl"),
    ),
    (
        "ribbons",
        include_str!("../../../../assets/shaders/ribbons.wgsl"),
    ),
    (
        "kaleido",
        include_str!("../../../../assets/shaders/kaleido.wgsl"),
    ),
    (
        "lasers",
        include_str!("../../../../assets/shaders/lasers.wgsl"),
    ),
    (
        "nebula",
        include_str!("../../../../assets/shaders/nebula.wgsl"),
    ),
    (
        "zerog",
        include_str!("../../../../assets/shaders/zerog.wgsl"),
    ),
    (
        "crystal",
        include_str!("../../../../assets/shaders/crystal.wgsl"),
    ),
    (
        "liquid",
        include_str!("../../../../assets/shaders/liquid.wgsl"),
    ),
    (
        "cover",
        include_str!("../../../../assets/shaders/cover.wgsl"),
    ),
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
