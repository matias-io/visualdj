//! Watches `assets/shaders/` and reports which scene shaders changed, debounced so an editor's
//! save (often several writes) produces one reload.
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use notify::{RecursiveMode, Watcher};

/// Quiet period after the last write before a path is reported.
pub const DEBOUNCE: Duration = Duration::from_millis(150);

/// Collapses bursts of writes to the same path into one event after a quiet period.
pub struct Debouncer {
    quiet: Duration,
    pending: HashMap<PathBuf, Instant>,
}

impl Debouncer {
    pub fn new(quiet: Duration) -> Self {
        Self {
            quiet,
            pending: HashMap::new(),
        }
    }

    pub fn touch(&mut self, path: PathBuf, at: Instant) {
        self.pending.insert(path, at);
    }

    /// Paths whose last write is older than the quiet period, in a stable order.
    pub fn ready(&mut self, now: Instant) -> Vec<PathBuf> {
        let mut ready: Vec<PathBuf> = self
            .pending
            .iter()
            .filter(|(_, t)| now.saturating_duration_since(**t) >= self.quiet)
            .map(|(p, _)| p.clone())
            .collect();
        ready.sort();
        for p in &ready {
            self.pending.remove(p);
        }
        ready
    }
}

/// `…/ring.wgsl` → `ring`; anything that is not a `.wgsl` file (editor backups, notes) → `None`.
pub fn scene_stem(path: &Path) -> Option<String> {
    let ext = path.extension()?.to_str()?;
    if !ext.eq_ignore_ascii_case("wgsl") {
        return None;
    }
    path.file_stem()?.to_str().map(str::to_string)
}

/// Reports changed shader stems from `assets/shaders/`, debounced.
pub struct ShaderWatcher {
    _watcher: notify::RecommendedWatcher,
    events: crossbeam_channel::Receiver<PathBuf>,
    debouncer: Debouncer,
}

impl ShaderWatcher {
    pub fn new(dir: &Path) -> anyhow::Result<Self> {
        let (tx, rx) = crossbeam_channel::unbounded::<PathBuf>();
        let mut watcher =
            notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
                if let Ok(event) = res {
                    for path in event.paths {
                        let _ = tx.send(path);
                    }
                }
            })?;
        watcher.watch(dir, RecursiveMode::NonRecursive)?;
        tracing::info!(dir = %dir.display(), "watching shaders for changes");
        Ok(Self {
            _watcher: watcher,
            events: rx,
            debouncer: Debouncer::new(DEBOUNCE),
        })
    }

    /// Scene stems that changed and have been quiet for [`DEBOUNCE`]. `common` means all.
    pub fn poll(&mut self) -> Vec<String> {
        let now = Instant::now();
        while let Ok(path) = self.events.try_recv() {
            if scene_stem(&path).is_some() {
                self.debouncer.touch(path, now);
            }
        }
        self.debouncer
            .ready(now)
            .iter()
            .filter_map(|p| scene_stem(p))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::time::{Duration, Instant};

    #[test]
    fn debouncer_emits_once_after_quiet_period() {
        let t0 = Instant::now();
        let mut d = Debouncer::new(Duration::from_millis(150));
        d.touch(PathBuf::from("ring.wgsl"), t0);
        d.touch(PathBuf::from("ring.wgsl"), t0 + Duration::from_millis(40));
        d.touch(PathBuf::from("ring.wgsl"), t0 + Duration::from_millis(90));
        assert!(
            d.ready(t0 + Duration::from_millis(200)).is_empty(),
            "still within the quiet period"
        );
        let ready = d.ready(t0 + Duration::from_millis(250));
        assert_eq!(ready, vec![PathBuf::from("ring.wgsl")]);
        assert!(
            d.ready(t0 + Duration::from_millis(300)).is_empty(),
            "emitted once"
        );
    }

    #[test]
    fn debouncer_keeps_paths_separate() {
        let t0 = Instant::now();
        let mut d = Debouncer::new(Duration::from_millis(100));
        d.touch(PathBuf::from("a.wgsl"), t0);
        d.touch(PathBuf::from("b.wgsl"), t0 + Duration::from_millis(50));
        let first = d.ready(t0 + Duration::from_millis(120));
        assert_eq!(first, vec![PathBuf::from("a.wgsl")]);
        let second = d.ready(t0 + Duration::from_millis(170));
        assert_eq!(second, vec![PathBuf::from("b.wgsl")]);
    }

    #[test]
    fn scene_stem_extracts_name_for_shader_files_only() {
        assert_eq!(
            scene_stem(&PathBuf::from(r"C:\x\assets\shaders\ring.wgsl")),
            Some("ring".into())
        );
        assert_eq!(scene_stem(&PathBuf::from("assets/shaders/notes.txt")), None);
        assert_eq!(
            scene_stem(&PathBuf::from("assets/shaders/ring.wgsl~")),
            None
        );
    }
}
