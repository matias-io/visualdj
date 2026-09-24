//! Per-version pointer chains: the file the calibrator writes and the reader loads.
//! One TOML file per rekordbox version under `offsets/`, e.g. `offsets/7.2.18.toml`.
use std::path::Path;

use serde::{Deserialize, Serialize};

use super::chain::Chain;

/// How the deck position is stored in rekordbox's memory.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PositionFormat {
    /// Signed 64-bit sample count.
    I64,
    /// Signed 32-bit sample count.
    I32,
    /// Double, in the unit given by `position_rate_hz` (1.0 for seconds).
    F64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DeckChains {
    /// Current (pitched) tempo, f32.
    pub bpm: Chain,
    /// Playback position in `position_format`.
    pub position: Chain,
    /// `Track Title: …\nArtist: …\nAlbum: …` text block.
    pub track_info: Option<Chain>,
    /// rekordbox-relative ANLZ `.DAT` path.
    pub anlz_path: Option<Chain>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Offsets {
    pub rekordbox_version: String,
    pub platform: String,
    pub position_format: PositionFormat,
    /// Position units per second of track time (44100 for a sample count).
    pub position_rate_hz: f64,
    /// Zero-based index of the master deck, u8.
    pub master_deck: Chain,
    pub decks: Vec<DeckChains>,
    /// Where these chains came from (calibrator run, imported file, hand-derived).
    pub provenance: Option<String>,
}

impl Offsets {
    pub fn to_toml(&self) -> anyhow::Result<String> {
        Ok(toml::to_string_pretty(self)?)
    }

    pub fn from_toml(text: &str) -> anyhow::Result<Self> {
        Ok(toml::from_str(text)?)
    }

    pub fn save(&self, path: &Path) -> anyhow::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(path, self.to_toml()?)?;
        Ok(())
    }

    pub fn load(path: &Path) -> anyhow::Result<Self> {
        Self::from_toml(&std::fs::read_to_string(path)?)
    }

    /// `<dir>/<version>.toml` when it exists and parses.
    pub fn for_version(dir: &Path, version: &str) -> Option<Self> {
        let path = dir.join(format!("{version}.toml"));
        match Self::load(&path) {
            Ok(o) => Some(o),
            Err(e) => {
                if path.exists() {
                    tracing::warn!(path = %path.display(), "offsets file unreadable: {e:#}");
                }
                None
            }
        }
    }

    /// Parses `rkbx_link`'s `data/offsets` text and returns the block for `version`, if any.
    /// Block layout: version line, master line, blank, then per deck four lines (bpm,
    /// position, track info, ANLZ path) separated by blank lines; two blanks end a block.
    pub fn from_rkbx_text(text: &str, version: &str) -> Option<Self> {
        let mut lines = text.lines().map(str::trim).peekable();
        while let Some(line) = lines.next() {
            if line != version {
                continue;
            }
            let master = Chain::from_rkbx_line(lines.next()?)?;
            let mut decks = Vec::new();
            let mut group: Vec<Chain> = Vec::new();
            let mut blanks = 0;
            for line in lines.by_ref() {
                if line.is_empty() {
                    blanks += 1;
                    if !group.is_empty() {
                        decks.push(std::mem::take(&mut group));
                    }
                    if blanks >= 2 {
                        break;
                    }
                    continue;
                }
                blanks = 0;
                match Chain::from_rkbx_line(line) {
                    Some(c) => group.push(c),
                    None => break,
                }
            }
            if !group.is_empty() {
                decks.push(group);
            }
            let decks: Vec<DeckChains> = decks
                .into_iter()
                .filter(|g| g.len() >= 2)
                .map(|g| DeckChains {
                    bpm: g[0].clone(),
                    position: g[1].clone(),
                    track_info: g.get(2).cloned(),
                    anlz_path: g.get(3).cloned(),
                })
                .collect();
            if decks.is_empty() {
                return None;
            }
            return Some(Self {
                rekordbox_version: version.to_string(),
                platform: "windows".to_string(),
                position_format: PositionFormat::I64,
                position_rate_hz: 44_100.0,
                master_deck: master,
                decks,
                provenance: Some("imported from rkbx_link offsets text".to_string()),
            });
        }
        None
    }
}
