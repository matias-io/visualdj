//! The launcher page: what the DJ sees before the show. It says whether rekordbox is ready,
//! lets them pick the output monitor and the look, runs the one-time calibration for a new
//! rekordbox version, lists the show's shortcuts, and starts the show.
use std::path::Path;

use crate::config::Config;
use crate::engine::EngineStatus;
use crate::overlay::{Edits, OverlayAction, OverlayView, audio_section, output_section, scene_section};

/// Window size the launcher opens at, in logical pixels (scaled by the monitor's DPI).
pub const LAUNCHER_SIZE: (f64, f64) = (980.0, 820.0);

/// Readiness of the rekordbox link, as a traffic light.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Light {
    Red,
    Amber,
    Green,
}

/// The launcher's verdict on the rekordbox link: the light, one line saying where things
/// stand, and what to do about it (empty when nothing is needed).
pub fn readiness(status: &EngineStatus) -> (Light, String, String) {
    match status {
        EngineStatus::Starting => (Light::Amber, "starting".into(), String::new()),
        EngineStatus::Waiting(why) if why.contains("no offsets") => (
            Light::Amber,
            "rekordbox is running, but this version is not calibrated yet".into(),
            "Run the calibration below once. Onset then connects by itself.".into(),
        ),
        EngineStatus::Waiting(why) if why.contains("waiting for rekordbox") => (
            Light::Red,
            "rekordbox is not running".into(),
            "Start rekordbox in Performance mode. Onset connects by itself, before or during the show.".into(),
        ),
        EngineStatus::Waiting(why) | EngineStatus::Error(why) => {
            (Light::Red, why.clone(), String::new())
        }
        EngineStatus::Idle => (
            Light::Green,
            "connected; load a track on a deck and press play".into(),
            String::new(),
        ),
        EngineStatus::Unidentified { .. } => (
            Light::Green,
            "connected; following the playing deck (track not identified, audio only)".into(),
            String::new(),
        ),
        EngineStatus::Running { track, .. } => {
            (Light::Green, format!("connected: {track}"), String::new())
        }
    }
}

/// rekordbox versions with an offsets file in `dir`, sorted.
pub fn calibrated_versions(dir: &Path) -> Vec<String> {
    let mut out: Vec<String> = std::fs::read_dir(dir)
        .map(|entries| {
            entries
                .filter_map(Result::ok)
                .map(|e| e.path())
                .filter(|p| p.extension().is_some_and(|x| x == "toml"))
                .filter_map(|p| p.file_stem().map(|s| s.to_string_lossy().into_owned()))
                .collect()
        })
        .unwrap_or_default();
    out.sort();
    out
}

/// Versions calibrated in any folder Onset loads offsets from.
pub fn all_calibrated_versions(primary: &Path) -> Vec<String> {
    let mut out: Vec<String> = onset_transport::memory::offsets::Offsets::search_dirs(primary)
        .iter()
        .flat_map(|d| calibrated_versions(d))
        .collect();
    out.sort();
    out.dedup();
    out
}

/// What the launcher shows about calibration.
pub struct CalibrationView<'a> {
    pub running: bool,
    /// Progress lines of the current or last run, oldest first.
    pub lines: &'a [String],
    pub versions: &'a [String],
}

/// Shortcuts that work during the show, with what they do.
pub const SHORTCUTS: &[(&str, &str)] = &[
    ("F", "fullscreen on / off"),
    ("Esc", "back to this launcher (closes the settings panel first)"),
    ("Tab", "settings panel over the show"),
    ("H", "HUD (frame time, rekordbox link, phrase, drop countdown)"),
    ("C", "now-playing card"),
    ("B", "blackout"),
    ("Left / Right", "previous / next scene"),
];

#[cfg(not(windows))]
const STEPS_FALLBACK: &[&str] = &["Follow the steps as the calibrator announces them."];

fn steps() -> &'static [&'static str] {
    #[cfg(windows)]
    {
        onset_transport::memory::calibrator::STEPS
    }
    #[cfg(not(windows))]
    {
        STEPS_FALLBACK
    }
}

fn light_colour(light: Light) -> egui::Color32 {
    match light {
        Light::Red => egui::Color32::from_rgb(230, 70, 70),
        Light::Amber => egui::Color32::from_rgb(240, 180, 40),
        Light::Green => egui::Color32::from_rgb(70, 200, 110),
    }
}

/// Draws the launcher. Edits to the config are recorded in `edits`; starting the show,
/// calibrating and quitting are actions the app carries out.
pub fn launcher_page(
    ctx: &egui::Context,
    config: &mut Config,
    view: &OverlayView<'_>,
    cal: &CalibrationView<'_>,
    endpoints: &[String],
    edits: &mut Edits,
) {
    let frame = egui::Frame::default()
        .fill(egui::Color32::from_rgba_unmultiplied(12, 12, 16, 228))
        .inner_margin(18.0);
    let screen = ctx.content_rect();
    // A title-less window pinned to the whole viewport: the page fills the window and
    // scrolls when the window is short.
    egui::Window::new("launcher")
        .title_bar(false)
        .frame(frame)
        .fixed_pos(screen.min)
        .fixed_size(screen.size())
        .show(ctx, |ui| {
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .max_height(screen.height() - 40.0)
                .show(ui, |ui| {
                    ui.heading(egui::RichText::new("Onset").size(30.0));
                    ui.label("Real-time, structure-aware visuals for rekordbox. The scene showing through is the live preview.");
                    ui.separator();
                    rekordbox_section(ui, view);
                    ui.separator();
                    ui.heading("Output");
                    output_section(ui, config, view, edits);
                    if ui
                        .checkbox(&mut config.fullscreen, "Fullscreen on that monitor (F toggles during the show)")
                        .changed()
                    {
                        edits.changed = true;
                    }
                    ui.separator();
                    ui.heading("Look");
                    scene_section(ui, config, view, edits);
                    ui.separator();
                    ui.heading("Audio");
                    audio_section(ui, config, endpoints, edits);
                    ui.separator();
                    calibration_section(ui, cal, edits);
                    ui.separator();
                    shortcuts_section(ui, view.simulator);
                    ui.separator();
                    ui.horizontal(|ui| {
                        let start = egui::Button::new(egui::RichText::new("Start show").size(22.0))
                            .min_size(egui::vec2(220.0, 44.0));
                        if ui.add(start).clicked() {
                            edits.actions.push(OverlayAction::StartShow);
                        }
                        if ui.button("Quit").clicked() {
                            edits.actions.push(OverlayAction::Quit);
                        }
                    });
                    ui.small("The show starts idle when rekordbox is not ready and picks up as soon as a deck plays.");
                });
        });
}

fn rekordbox_section(ui: &mut egui::Ui, view: &OverlayView<'_>) {
    ui.heading("rekordbox");
    let (light, verdict, hint) = readiness(view.status);
    ui.horizontal(|ui| {
        let (rect, _) = ui.allocate_exact_size(egui::vec2(16.0, 16.0), egui::Sense::hover());
        ui.painter()
            .circle_filled(rect.center(), 6.5, light_colour(light));
        ui.label(verdict);
    });
    if !hint.is_empty() {
        ui.small(hint);
    }
    if view.simulator {
        ui.small("Developer simulator: rekordbox is not read in this run.");
    }
}

fn calibration_section(ui: &mut egui::Ui, cal: &CalibrationView<'_>, edits: &mut Edits) {
    ui.heading("Calibration");
    ui.label(
        "Once per rekordbox version, about two minutes. Onset reads the deck positions from rekordbox's memory and needs to learn what a deck looks like in each release.",
    );
    if cal.versions.is_empty() {
        ui.label("Calibrated versions: none yet.");
    } else {
        ui.label(format!("Calibrated versions: {}", cal.versions.join(", ")));
    }
    if cal.running {
        ui.label("Calibrating. Do each step as it appears; the calibrator sees it and moves on:");
    } else {
        ui.horizontal(|ui| {
            if ui.button("Calibrate this rekordbox (about two minutes)").clicked() {
                edits.actions.push(OverlayAction::Calibrate);
            }
        });
        ui.collapsing("The steps, for reference", |ui| {
            for (i, step) in steps().iter().enumerate() {
                ui.label(format!("{}. {step}", i + 1));
            }
            ui.small("Open rekordbox in Performance mode. Pausing and playing again is how the calibrator tells the playhead from clocks that keep running; it then describes the deck so every deck is found at runtime.");
        });
    }
    if !cal.lines.is_empty() {
        egui::ScrollArea::vertical()
            .id_salt("calibration-log")
            .max_height(150.0)
            .stick_to_bottom(true)
            .show(ui, |ui| {
                for line in cal.lines {
                    ui.monospace(line);
                }
            });
    }
    if cal.running && ui.button("Cancel calibration").clicked() {
        edits.actions.push(OverlayAction::CancelCalibration);
    }
}

fn shortcuts_section(ui: &mut egui::Ui, simulator: bool) {
    ui.heading("Shortcuts during the show");
    egui::Grid::new("shortcuts").num_columns(2).spacing([18.0, 4.0]).show(ui, |ui| {
        for (key, what) in SHORTCUTS {
            ui.monospace(*key);
            ui.label(*what);
            ui.end_row();
        }
        if simulator {
            for (key, what) in [
                ("Space", "play / pause the simulator"),
                ("Home", "restart the simulated track"),
                ("[ / ]", "simulator rate down / up"),
                ("R", "reload the simulated track"),
            ] {
                ui.monospace(key);
                ui.label(what);
                ui.end_row();
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn readiness_maps_each_engine_state_to_a_light() {
        let (light, _, hint) = readiness(&EngineStatus::Waiting("waiting for rekordbox".into()));
        assert_eq!(light, Light::Red);
        assert!(hint.contains("Start rekordbox"));
        let (light, _, hint) =
            readiness(&EngineStatus::Waiting("no offsets for rekordbox 7.2.19".into()));
        assert_eq!(light, Light::Amber);
        assert!(hint.contains("calibration"));
        assert_eq!(readiness(&EngineStatus::Idle).0, Light::Green);
        let (light, verdict, _) = readiness(&EngineStatus::Running {
            source: "rekordbox".into(),
            track: "Artist - Title".into(),
        });
        assert_eq!(light, Light::Green);
        assert!(verdict.contains("Artist - Title"));
    }

    #[test]
    fn calibrated_versions_lists_toml_stems_sorted() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("7.2.18.toml"), "").unwrap();
        std::fs::write(dir.path().join("7.1.0.toml"), "").unwrap();
        std::fs::write(dir.path().join("notes.txt"), "").unwrap();
        assert_eq!(calibrated_versions(dir.path()), vec!["7.1.0", "7.2.18"]);
        assert!(calibrated_versions(Path::new("does/not/exist")).is_empty());
    }
}
