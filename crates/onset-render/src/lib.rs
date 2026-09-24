//! Everything GPU: device setup, the per-frame uniforms mirrored into WGSL, scenes, text,
//! artwork quads, and a headless renderer for tests.
#![deny(unsafe_code)]

pub mod assets;
pub mod gpu;
pub mod headless;
pub mod renderer;
pub mod scene;
pub mod uniforms;
