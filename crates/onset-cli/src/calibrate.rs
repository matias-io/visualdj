//! `calibrate`: the guided calibration session on the terminal. The session itself lives in
//! `onset_transport::memory::calibrator`; this prints its progress lines.
#![cfg(windows)]

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use onset_transport::memory::calibrator::{self, Session};

pub fn calibrate(out_dir: &Path, step_seconds: u64) -> anyhow::Result<()> {
    let cancel = AtomicBool::new(false);
    let mut print = |line: &str| println!("{line}");
    let mut session = Session {
        step_limit: Duration::from_secs(step_seconds),
        report: &mut print,
        cancel: &cancel,
    };
    calibrator::calibrate(out_dir, &mut session)?;
    Ok(())
}

pub fn refine(dir: &Path) -> anyhow::Result<()> {
    let cancel = AtomicBool::new(false);
    let mut print = |line: &str| println!("{line}");
    let mut session = Session {
        step_limit: Duration::from_secs(60),
        report: &mut print,
        cancel: &cancel,
    };
    calibrator::refine_existing(dir, &mut session)?;
    Ok(())
}

pub fn default_out_dir() -> PathBuf {
    std::env::var_os("ONSET_OFFSETS").map_or_else(|| PathBuf::from("offsets"), PathBuf::from)
}
