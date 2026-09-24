//! `onset --bench N` runs the simulator for N seconds and prints frame statistics. It needs
//! a display and the private fixtures, so it only runs when `ONSET_DISPLAY_TESTS=1`.
use std::path::PathBuf;
use std::process::Command;

fn fixtures() -> Option<PathBuf> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/private");
    root.join("master.db").exists().then_some(root)
}

#[test]
fn bench_mode_prints_mean_p99_and_dropped_frames() {
    if std::env::var_os("ONSET_DISPLAY_TESTS").is_none() {
        eprintln!("skipping: set ONSET_DISPLAY_TESTS=1 to run the on-screen bench");
        return;
    }
    let Some(app_dir) = fixtures() else {
        eprintln!("skipping: fixtures/private missing");
        return;
    };
    let repo = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let out = Command::new(env!("CARGO_BIN_EXE_onset"))
        .current_dir(&repo)
        .args([
            "--bench", "3", "--sim", "Move", "--gain", "0.0", "--scene", "ring",
        ])
        .arg("--app-dir")
        .arg(&app_dir)
        .output()
        .expect("onset runs");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "exit {:?}\n{stderr}", out.status);
    let line = stdout
        .lines()
        .find(|l| l.starts_with("bench "))
        .unwrap_or_else(|| panic!("no bench line in:\n{stdout}\n{stderr}"));
    for key in ["scene=", "frames=", "mean_ms=", "p99_ms=", "dropped="] {
        assert!(line.contains(key), "{key} missing from {line}");
    }
}
