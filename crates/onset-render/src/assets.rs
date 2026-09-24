//! Where shader and font files live at runtime.
use std::path::PathBuf;

/// `ONSET_ASSETS` if set, else `assets/` beside the executable, else `assets/` in the
/// working directory (the development layout).
pub fn assets_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("ONSET_ASSETS") {
        return PathBuf::from(dir);
    }
    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
    {
        let beside = dir.join("assets");
        if beside.is_dir() {
            return beside;
        }
    }
    PathBuf::from("assets")
}

pub fn shader_dir() -> PathBuf {
    assets_dir().join("shaders")
}
