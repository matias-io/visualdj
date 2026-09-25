//! Everything GPU: device setup, the per-frame uniforms mirrored into WGSL, scenes, text,
//! artwork quads, and a headless renderer for tests.
#![deny(unsafe_code)]

pub mod artwork;
pub mod assets;
pub mod card;
pub mod gpu;
pub mod headless;
pub mod hot_reload;
pub mod hud;
pub mod quad;
pub mod renderer;
pub mod scene;
pub mod scenes;
pub mod stage;
pub mod text;
pub mod uniforms;
