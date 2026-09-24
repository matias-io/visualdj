//! Shared plumbing for the developer commands.
use std::path::PathBuf;

use onset_rekordbox::library::Library;
use onset_rekordbox::paths::RekordboxPaths;

pub fn resolve_paths(app_dir: Option<PathBuf>) -> anyhow::Result<RekordboxPaths> {
    Ok(match app_dir {
        Some(dir) => RekordboxPaths::from_app_dir(&dir)?,
        None => RekordboxPaths::discover()?,
    })
}

pub fn cache_dir() -> anyhow::Result<PathBuf> {
    directories::ProjectDirs::from("", "Onset", "Onset")
        .map(|d| d.cache_dir().to_path_buf())
        .ok_or_else(|| anyhow::anyhow!("no cache directory available"))
}

pub fn open_library(app_dir: Option<PathBuf>) -> anyhow::Result<(RekordboxPaths, Library)> {
    let paths = resolve_paths(app_dir)?;
    let lib = Library::open(&paths, &cache_dir()?)?;
    Ok((paths, lib))
}

/// A thread that sends once when the user types `q` and Enter.
pub fn quit_signal() -> crossbeam_channel::Receiver<()> {
    let (tx, rx) = crossbeam_channel::bounded(1);
    std::thread::spawn(move || {
        let mut line = String::new();
        while std::io::stdin().read_line(&mut line).is_ok() {
            if line.trim().eq_ignore_ascii_case("q") {
                let _ = tx.send(());
                break;
            }
            line.clear();
        }
    });
    rx
}

/// 24-character bar meter from band levels, auto-scaled to the loudest band seen.
pub struct Meter {
    peak: f32,
}

impl Meter {
    pub fn new() -> Self {
        Self { peak: 1e-6 }
    }

    pub fn render(&mut self, bands: &[f32]) -> String {
        const GLYPHS: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
        let max = bands.iter().copied().fold(0.0f32, f32::max);
        self.peak = (self.peak * 0.995).max(max).max(1e-6);
        bands
            .iter()
            .map(|b| {
                let level = (b / self.peak).clamp(0.0, 0.999);
                GLYPHS[(level * GLYPHS.len() as f32) as usize]
            })
            .collect()
    }
}
