//! Where rekordbox keeps its data on this machine.
use std::path::{Path, PathBuf};

#[derive(Debug, thiserror::Error)]
pub enum PathsError {
    #[error("rekordbox data folder not found (looked in {0})")]
    NotFound(PathBuf),
    #[error("no master.db in {0}")]
    NoDatabase(PathBuf),
}

/// Resolved locations inside a rekordbox data folder (`%APPDATA%\Pioneer\rekordbox`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RekordboxPaths {
    pub app_dir: PathBuf,
    pub master_db: PathBuf,
    pub master_wal: PathBuf,
    pub share_dir: PathBuf,
}

impl RekordboxPaths {
    /// Looks in `%APPDATA%\Pioneer\rekordbox`.
    pub fn discover() -> Result<Self, PathsError> {
        let base = directories::BaseDirs::new()
            .map(|b| b.config_dir().join("Pioneer").join("rekordbox"))
            .ok_or_else(|| PathsError::NotFound(PathBuf::from("%APPDATA%")))?;
        Self::from_app_dir(&base)
    }

    pub fn from_app_dir(dir: &Path) -> Result<Self, PathsError> {
        if !dir.is_dir() {
            return Err(PathsError::NotFound(dir.to_path_buf()));
        }
        let master_db = dir.join("master.db");
        if !master_db.is_file() {
            return Err(PathsError::NoDatabase(dir.to_path_buf()));
        }
        Ok(Self {
            app_dir: dir.to_path_buf(),
            master_wal: dir.join("master.db-wal"),
            share_dir: dir.join("share"),
            master_db,
        })
    }

    /// Resolve a rekordbox-relative share path such as `/PIONEER/USBANLZ/…/ANLZ0000.DAT`.
    /// rekordbox mixes `/` and `\` separators; both are accepted.
    pub fn resolve_share(&self, rel: &str) -> PathBuf {
        let mut out = self.share_dir.clone();
        for part in rel.split(['/', '\\']).filter(|s| !s.is_empty()) {
            out.push(part);
        }
        out
    }
}
