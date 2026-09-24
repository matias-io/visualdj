//! User configuration, persisted as TOML under `%APPDATA%\Onset\config.toml`.
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum MonitorChoice {
    Primary,
    /// Zero-based index in the OS monitor list.
    Index(usize),
    /// Case-insensitive substring of the monitor name.
    NameContains(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PresentModeChoice {
    /// VSync, one frame of latency, never tears. The default.
    Fifo,
    /// VSync with the newest frame replacing a queued one; lower latency where supported.
    Mailbox,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub output_monitor: MonitorChoice,
    pub present_mode: PresentModeChoice,
    /// Scenes render at this fraction of the output resolution and are upscaled.
    pub internal_scale: f32,
    pub scene: String,
    pub show_hud: bool,
    pub show_card: bool,
    /// Track title the simulator loads at startup when no live transport is configured.
    pub sim_track: Option<String>,
    /// Substring of the loopback endpoint name; default output device when `None`.
    pub audio_device: Option<String>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            output_monitor: MonitorChoice::Primary,
            present_mode: PresentModeChoice::Fifo,
            internal_scale: 1.0,
            scene: "ring".to_string(),
            show_hud: true,
            show_card: true,
            sim_track: None,
            audio_device: None,
        }
    }
}

impl Config {
    /// `%APPDATA%\Onset\config.toml`, or a relative fallback when no profile dir exists.
    pub fn path() -> PathBuf {
        directories::ProjectDirs::from("", "Onset", "Onset").map_or_else(
            || PathBuf::from("onset-config.toml"),
            |d| d.config_dir().join("config.toml"),
        )
    }

    pub fn load() -> Self {
        Self::load_from(&Self::path())
    }

    /// Missing or unreadable files give the defaults; a corrupt file is logged, never fatal.
    pub fn load_from(path: &Path) -> Self {
        match std::fs::read_to_string(path) {
            Ok(text) => toml::from_str(&text).unwrap_or_else(|e| {
                tracing::warn!(path = %path.display(), "config unreadable, using defaults: {e}");
                Self::default()
            }),
            Err(_) => Self::default(),
        }
    }

    pub fn save(&self) -> anyhow::Result<()> {
        self.save_to(&Self::path())
    }

    pub fn save_to(&self, path: &Path) -> anyhow::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(path, toml::to_string_pretty(self)?)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_file_gives_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = Config::load_from(&dir.path().join("nope.toml"));
        assert_eq!(cfg, Config::default());
        assert_eq!(cfg.output_monitor, MonitorChoice::Primary);
        assert_eq!(cfg.present_mode, PresentModeChoice::Fifo);
        assert!((cfg.internal_scale - 1.0).abs() < f32::EPSILON);
        assert!(cfg.show_hud && cfg.show_card);
    }

    #[test]
    fn round_trips_through_toml() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let cfg = Config {
            output_monitor: MonitorChoice::Index(1),
            present_mode: PresentModeChoice::Mailbox,
            internal_scale: 0.75,
            scene: "warp".into(),
            show_hud: false,
            show_card: true,
            sim_track: Some("Move".into()),
            audio_device: Some("NVIDIA".into()),
        };
        cfg.save_to(&path).unwrap();
        assert_eq!(Config::load_from(&path), cfg);
    }

    #[test]
    fn corrupt_file_gives_defaults_not_a_crash() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "this is = not [ toml").unwrap();
        assert_eq!(Config::load_from(&path), Config::default());
    }

    #[test]
    fn monitor_choice_serialises_readably() {
        let s = toml::to_string(&Config {
            output_monitor: MonitorChoice::NameContains("DISPLAY2".into()),
            ..Config::default()
        })
        .unwrap();
        assert!(s.contains("DISPLAY2"), "{s}");
    }
}
