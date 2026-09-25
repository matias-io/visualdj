//! The launcher page: what the DJ sees before the show. It says whether rekordbox is ready,
//! lets them pick the output monitor and the look, runs the one-time calibration for a new
//! rekordbox version, lists the show's shortcuts, and starts the show.
use std::path::Path;

use onset_core::show::{AutoChange, FxSettings, SceneTweak, ShowEvent};
use onset_render::overlay_options::{Corner, LyricPlace, LyricStyle};
use onset_render::renderer::Quality;
use onset_render::scenes::scene_info;

use crate::config::{Config, MonitorChoice, Preset, PresentModeChoice};
use crate::engine::{EngineCommand, EngineStatus};
use crate::overlay::{Edits, OverlayAction, OverlayView, monitor_detail, monitor_label};

/// Window size the launcher opens at, in logical pixels (scaled by the monitor's DPI).
pub const LAUNCHER_SIZE: (f64, f64) = (1280.0, 820.0);

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
        EngineStatus::Waiting(why) if why.contains("finding its decks") => (
            Light::Amber,
            "rekordbox found, finding its decks".into(),
            "This takes up to 20 seconds after Onset or rekordbox starts.".into(),
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
        EngineStatus::Running { .. } => (
            Light::Green,
            "connected to rekordbox, following the playing deck".into(),
            String::new(),
        ),
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
    (
        "Esc",
        "back to this launcher (closes the settings panel first)",
    ),
    ("Tab", "settings panel over the show"),
    (
        "H",
        "HUD (frame time, rekordbox link, phrase, drop countdown)",
    ),
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

/// The launcher's tabs. Show has everything a set needs; the rest are the advanced ones.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Tab {
    #[default]
    Show,
    Scenes,
    Effects,
    Screen,
    Lyrics,
    Output,
    Audio,
    Setup,
    Help,
}

impl Tab {
    const BASIC: [Self; 2] = [Self::Show, Self::Help];
    const ADVANCED: [Self; 7] = [
        Self::Scenes,
        Self::Effects,
        Self::Screen,
        Self::Lyrics,
        Self::Output,
        Self::Audio,
        Self::Setup,
    ];

    fn label(self) -> &'static str {
        match self {
            Self::Show => "Show",
            Self::Scenes => "Scenes",
            Self::Effects => "Effects",
            Self::Screen => "On screen",
            Self::Lyrics => "Lyrics",
            Self::Output => "Output",
            Self::Audio => "Audio",
            Self::Setup => "Setup",
            Self::Help => "Help",
        }
    }
}

/// Width of the launcher's panel, in logical pixels; the rest of the window is the preview.
pub const PANEL_WIDTH: f32 = 540.0;

fn rgb(c: [f32; 3]) -> egui::Color32 {
    let to8 = |v: f32| (v.clamp(0.0, 1.0).powf(1.0 / 2.2) * 255.0).round() as u8;
    egui::Color32::from_rgb(to8(c[0]), to8(c[1]), to8(c[2]))
}

/// The accent the launcher uses: the track's own colour, lifted towards white until it is
/// light enough to read on the dark panel and to carry black text on a button.
fn accent(view: &OverlayView<'_>) -> egui::Color32 {
    let c = view.palette[2];
    let m = c[0].max(c[1]).max(c[2]).max(0.05);
    let mut c = [c[0] / m, c[1] / m, c[2] / m];
    let luma = |c: &[f32; 3]| 0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2];
    for _ in 0..8 {
        if luma(&c) >= 0.45 {
            break;
        }
        for v in &mut c {
            *v += (1.0 - *v) * 0.2;
        }
    }
    rgb(c)
}

/// An on/off switch that makes its state obvious (the egui demo's toggle).
fn toggle(ui: &mut egui::Ui, on: &mut bool, colour: egui::Color32) -> egui::Response {
    let size = ui.spacing().interact_size.y * egui::vec2(2.0, 1.0);
    let (rect, mut response) = ui.allocate_exact_size(size, egui::Sense::click());
    if response.clicked() {
        *on = !*on;
        response.mark_changed();
    }
    let t = ui.ctx().animate_bool_responsive(response.id, *on);
    let radius = 0.5 * rect.height();
    let bg = if *on {
        colour
    } else {
        egui::Color32::from_gray(70)
    };
    ui.painter().rect_filled(rect, radius, bg);
    let x = egui::lerp((rect.left() + radius)..=(rect.right() - radius), t);
    ui.painter()
        .circle_filled(egui::pos2(x, rect.center().y), radius * 0.8, egui::Color32::WHITE);
    response
}

/// A labelled switch with a line of explanation and a clear ON/OFF word.
fn switch_row(
    ui: &mut egui::Ui,
    on: &mut bool,
    title: &str,
    detail: &str,
    colour: egui::Color32,
) -> bool {
    let mut changed = false;
    ui.horizontal(|ui| {
        changed = toggle(ui, on, colour).changed();
        ui.label(egui::RichText::new(title).strong());
        let word = if *on { "ON" } else { "OFF" };
        let tint = if *on { colour } else { egui::Color32::GRAY };
        ui.label(egui::RichText::new(word).color(tint).small().strong());
    });
    ui.label(egui::RichText::new(detail).small().weak());
    changed
}

/// A thin bar showing an effect's current level, so the DJ sees what a slider does.
fn meter(ui: &mut egui::Ui, level: f32, colour: egui::Color32) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 4.0), egui::Sense::hover());
    ui.painter().rect_filled(rect, 2.0, egui::Color32::from_gray(35));
    let mut fill = rect;
    fill.set_width(rect.width() * level.clamp(0.0, 1.0));
    ui.painter().rect_filled(fill, 2.0, colour);
}

fn section(ui: &mut egui::Ui, title: &str) {
    ui.add_space(10.0);
    ui.label(egui::RichText::new(title.to_uppercase()).small().strong().weak());
    ui.add_space(2.0);
}

/// A section heading with a "?" that explains it on hover.
fn section_help(ui: &mut egui::Ui, title: &str, help: &str) {
    ui.add_space(10.0);
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new(title.to_uppercase()).small().strong().weak());
        ui.label(egui::RichText::new(" ? ").small().strong().background_color(egui::Color32::from_gray(50)))
            .on_hover_text(help);
    });
    ui.add_space(2.0);
}

/// One switch in a grid of what to show.
fn tick(ui: &mut egui::Ui, on: &mut bool, label: &str) -> bool {
    ui.checkbox(on, label).changed()
}

/// Draws the launcher: a panel of settings on the right; the live preview (scene, card and
/// HUD exactly as the audience will see them) fills the rest of the window.
pub fn launcher_page(
    ctx: &egui::Context,
    config: &mut Config,
    view: &OverlayView<'_>,
    endpoints: &[String],
    tab: &mut Tab,
    edits: &mut Edits,
) {
    let screen = ctx.content_rect();
    let width = PANEL_WIDTH.min(screen.width() * 0.6);
    let frame = egui::Frame::default()
        .fill(egui::Color32::from_rgba_unmultiplied(10, 10, 14, 238))
        .inner_margin(egui::Margin::symmetric(18, 14))
        .stroke(egui::Stroke::new(1.0, egui::Color32::from_gray(40)));
    let colour = accent(view);
    egui::Window::new("launcher")
        .title_bar(false)
        .frame(frame)
        .fixed_pos(egui::pos2(screen.right() - width, screen.top()))
        .fixed_size(egui::vec2(width - 36.0, screen.height() - 28.0))
        .show(ctx, |ui| {
            header(ui, view, colour);
            ui.add_space(6.0);
            let mut tab_button = |ui: &mut egui::Ui, t: Tab| {
                let selected = *tab == t;
                let text = egui::RichText::new(t.label()).color(if selected {
                    egui::Color32::WHITE
                } else {
                    egui::Color32::from_gray(190)
                });
                if ui.selectable_label(selected, text).clicked() {
                    *tab = t;
                }
            };
            ui.horizontal_wrapped(|ui| {
                for t in Tab::BASIC {
                    tab_button(ui, t);
                }
                ui.separator();
                ui.label(egui::RichText::new("Advanced").small().weak());
                for t in Tab::ADVANCED {
                    tab_button(ui, t);
                }
            });
            ui.separator();
            let footer = 70.0;
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .max_height((ui.available_height() - footer).max(100.0))
                .show(ui, |ui| match *tab {
                    Tab::Show => show_tab(ui, config, view, colour, edits),
                    Tab::Scenes => scenes_tab(ui, config, view, colour, edits),
                    Tab::Effects => effects_tab(ui, config, view, colour, edits),
                    Tab::Screen => screen_tab(ui, config, colour, edits),
                    Tab::Lyrics => lyrics_tab(ui, config, view, colour, edits),
                    Tab::Output => output_tab(ui, config, view, edits),
                    Tab::Audio => audio_tab(ui, config, view, endpoints, colour, edits),
                    Tab::Setup => calibration_section(ui, &view.calibration, edits),
                    Tab::Help => help_tab(ui, view.simulator),
                });
            ui.separator();
            ui.horizontal(|ui| {
                let start = egui::Button::new(
                    egui::RichText::new("Start show").size(20.0).strong().color(egui::Color32::BLACK),
                )
                .fill(colour)
                .min_size(egui::vec2(200.0, 40.0));
                if ui.add(start).clicked() {
                    edits.actions.push(OverlayAction::StartShow);
                }
                if ui.button("Quit").clicked() {
                    edits.actions.push(OverlayAction::Quit);
                }
            });
            ui.label(egui::RichText::new("Esc during the show comes back here.").small().weak());
        });
}

fn header(ui: &mut egui::Ui, view: &OverlayView<'_>, colour: egui::Color32) {
    // The title breathes with the kick, so the launcher shows the audio is live.
    // The row keeps a fixed height so the panel below does not jump on every kick.
    let pulse = view.audio.kick.clamp(0.0, 1.0);
    let (row, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 44.0), egui::Sense::hover());
    let painter = ui.painter_at(row);
    // Measured at rest, so the subtitle stays still while the title pulses.
    let title_w = painter
        .layout_no_wrap("ONSET".into(), egui::FontId::proportional(30.0), colour)
        .size()
        .x;
    painter.text(
        row.left_center(),
        egui::Align2::LEFT_CENTER,
        "ONSET",
        egui::FontId::proportional(30.0 + 4.0 * pulse),
        colour,
    );
    painter.text(
        egui::pos2(row.left() + title_w + 14.0, row.center().y + 3.0),
        egui::Align2::LEFT_CENTER,
        "visuals for rekordbox",
        egui::FontId::proportional(14.0),
        ui.visuals().weak_text_color(),
    );
    let (light, verdict, hint) = readiness(view.status);
    ui.horizontal(|ui| {
        let (rect, _) = ui.allocate_exact_size(egui::vec2(14.0, 14.0), egui::Sense::hover());
        ui.painter().circle_filled(rect.center(), 6.0, light_colour(light));
        ui.label(verdict);
    });
    if !hint.is_empty() {
        ui.label(egui::RichText::new(hint).small().weak());
    }
    if let Some(t) = view.track {
        let mut tags = vec![format!("energy {:.0}%", view.vibe.energy * 100.0)];
        tags.push(if view.vibe.darkness > 0.6 {
            "dark".to_string()
        } else if view.vibe.darkness < 0.35 {
            "bright".to_string()
        } else {
            "balanced".to_string()
        });
        if let Some(g) = &t.genre {
            tags.push(g.clone());
        }
        ui.label(egui::RichText::new(format!("Now: {} - {}", t.artist, t.title)).strong());
        ui.label(egui::RichText::new(tags.join("  ·  ")).small().weak());
    } else {
        // Same two lines while nothing plays, so the tabs stay put when a track arrives.
        ui.label(egui::RichText::new("Now: nothing playing").strong().weak());
        ui.label(egui::RichText::new(" ").small());
    }
    if view.simulator {
        ui.label(egui::RichText::new("Developer simulator: rekordbox is not read in this run.").small());
    }
}

/// The effects a style turns up, as (label, level 0..1) for the mode cards.
fn style_levels(fx: &FxSettings) -> [(&'static str, f32); 6] {
    [
        ("Movement", fx.reactivity / 1.5),
        ("Flashes", fx.flashes),
        ("Shake", fx.shake),
        ("Colour", fx.colour),
        ("Surprises", f32::midpoint(fx.inversions, fx.glitch)),
        ("Build-ups", fx.buildup),
    ]
}

/// What each style looks like, in the words a DJ would use.
fn style_story(p: Preset) -> &'static [&'static str] {
    match p {
        Preset::Chill => &[
            "Scenes drift and breathe with the music; nothing jolts.",
            "Colours turn slowly at new phrases.",
            "A soft glow of light bars before a drop, a gentle lift when it lands.",
            "No shake, no inversions, no strobe.",
        ],
        Preset::Club => &[
            "Scenes move with the bass, mids and drums.",
            "Light bars sweep across the screen as a drop builds; a soft flash when it lands.",
            "Every new phrase slides the picture with a band of light, and the colours swing.",
            "A little camera sway on kicks, the odd inversion or glitch.",
        ],
        Preset::Festival => &[
            "Everything bigger: stronger movement, bolder colour swings.",
            "Full build-ups: faster sweeps, a push in and a colour cycle towards the drop.",
            "Drops land with a flash, a zoom, camera sway and sometimes an inversion.",
            "Still no strobe unless you turn it on, and nothing flashes more than three times a second.",
        ],
        Preset::Custom => &["Your own mix of the sliders on the Effects tab."],
    }
}

/// Three cards, one per style, each showing what it turns up; click one to use it.
fn mode_cards(ui: &mut egui::Ui, config: &mut Config, colour: egui::Color32, edits: &mut Edits) {
    ui.columns(3, |cols| {
        for (col, p) in cols.iter_mut().zip(Preset::NAMED) {
            let selected = config.preset == p;
            let fx = p.fx().unwrap_or_default();
            let frame = egui::Frame::default()
                .fill(if selected { egui::Color32::from_gray(38) } else { egui::Color32::from_gray(24) })
                .stroke(egui::Stroke::new(if selected { 2.0 } else { 1.0 }, if selected { colour } else { egui::Color32::from_gray(55) }))
                .corner_radius(6.0)
                .inner_margin(egui::Margin::same(8));
            let response = frame
                .show(col, |ui| {
                    let title = egui::RichText::new(p.label()).strong().size(16.0);
                    ui.label(if selected { title.color(colour) } else { title });
                    for (name, level) in style_levels(&fx) {
                        ui.label(egui::RichText::new(name).small().weak());
                        meter(ui, level, if selected { colour } else { egui::Color32::from_gray(120) });
                    }
                })
                .response
                .interact(egui::Sense::click())
                .on_hover_text(p.blurb());
            if response.clicked() && !selected {
                config.preset = p;
                config.fx = fx;
                edits.push(OverlayAction::Look);
            }
        }
    });
    ui.add_space(4.0);
    for line in style_story(config.preset) {
        ui.label(egui::RichText::new(format!("•  {line}")).small());
    }
}

fn preset_buttons(ui: &mut egui::Ui, config: &mut Config, colour: egui::Color32, edits: &mut Edits) {
    ui.horizontal(|ui| {
        for p in Preset::NAMED {
            let selected = config.preset == p;
            let text = egui::RichText::new(p.label()).strong();
            let b = egui::Button::new(if selected { text.color(egui::Color32::BLACK) } else { text })
                .fill(if selected { colour } else { egui::Color32::from_gray(45) })
                .min_size(egui::vec2(96.0, 30.0));
            if ui.add(b).clicked() {
                config.preset = p;
                if let Some(fx) = p.fx() {
                    config.fx = fx;
                }
                edits.push(OverlayAction::Look);
            }
        }
    });
    ui.label(egui::RichText::new(config.preset.blurb()).small().weak());
}

fn show_tab(
    ui: &mut egui::Ui,
    config: &mut Config,
    view: &OverlayView<'_>,
    colour: egui::Color32,
    edits: &mut Edits,
) {
    ui.label("Everything needed for a show is on this page; the other tabs fine-tune it. The preview on the left is what the audience sees.");
    section(ui, "Where the show goes");
    monitor_picker(ui, config, view, edits);
    if switch_row(
        ui,
        &mut config.fullscreen,
        "Fullscreen",
        "Borderless fullscreen on that screen. F toggles it during the show.",
        colour,
    ) {
        edits.changed = true;
    }

    section_help(ui, "Style", "How big the show goes. Each card shows what it turns up; click one to use it. Fine-tune every effect on the Effects tab.");
    mode_cards(ui, config, colour, edits);

    section(ui, "Scenes");
    if switch_row(
        ui,
        &mut config.auto,
        "Auto",
        "Onset picks scenes that suit each track (tempo, key, genre, rekordbox's mood, cover colours) and changes them at drops and breakdowns.",
        colour,
    ) {
        edits.push(OverlayAction::Look);
    }
    if !config.auto {
        scene_combo(ui, config, view, edits);
    }

    section(ui, "On screen");
    if switch_row(
        ui,
        &mut config.show_card,
        "Now Playing card",
        "Cover, title and artist, bottom left. C toggles it during the show.",
        colour,
    ) {
        edits.push(OverlayAction::ShowCard(config.show_card));
    }
    if switch_row(
        ui,
        &mut config.show_hud,
        "HUD",
        "Technical readout top left: frame time, rekordbox link, phrase, drop countdown. For you, not the crowd. H toggles it.",
        colour,
    ) {
        edits.push(OverlayAction::ShowHud(config.show_hud));
    }

    section(ui, "Try it");
    preview_buttons(ui, edits);
    ui.label(egui::RichText::new("Plays the effects on the preview without music.").small().weak());
}

/// Buttons that play show moments on the preview without music.
fn preview_buttons(ui: &mut egui::Ui, edits: &mut Edits) {
    ui.horizontal_wrapped(|ui| {
        if ui.button("Build-up and drop").clicked() {
            edits.actions.push(OverlayAction::PreviewBuild);
        }
        if ui.button("Drop").clicked() {
            edits.actions.push(OverlayAction::Preview(ShowEvent::Drop));
        }
        if ui.button("New phrase").clicked() {
            edits.actions.push(OverlayAction::Preview(ShowEvent::Phrase(None)));
        }
        if ui.button("Track change").clicked() {
            edits.actions.push(OverlayAction::Preview(ShowEvent::Track));
        }
        if ui.button("Next scene").clicked() {
            edits.actions.push(OverlayAction::NextScene);
        }
    });
}

fn monitor_picker(ui: &mut egui::Ui, config: &mut Config, view: &OverlayView<'_>, edits: &mut Edits) {
    egui::ComboBox::from_id_salt("monitor")
        .width(ui.available_width() - 10.0)
        .selected_text(monitor_label(view.monitors, &config.output_monitor))
        .show_ui(ui, |ui| {
            if ui
                .selectable_label(config.output_monitor == MonitorChoice::Primary, "Primary screen")
                .clicked()
            {
                config.output_monitor = MonitorChoice::Primary;
                edits.changed = true;
            }
            for (i, m) in view.monitors.iter().enumerate() {
                let selected = config.output_monitor == MonitorChoice::Index(i);
                if ui
                    .selectable_label(selected, format!("#{i} {}", monitor_detail(m)))
                    .clicked()
                {
                    config.output_monitor = MonitorChoice::Index(i);
                    edits.changed = true;
                }
            }
        });
}

fn scene_title(name: &str) -> String {
    scene_info(name).map_or_else(|| name.to_string(), |i| i.title.to_string())
}

fn scene_combo(ui: &mut egui::Ui, config: &mut Config, view: &OverlayView<'_>, edits: &mut Edits) {
    egui::ComboBox::from_id_salt("scene")
        .width(ui.available_width() - 10.0)
        .selected_text(scene_title(view.active_scene))
        .show_ui(ui, |ui| {
            for name in view.scenes {
                if ui
                    .selectable_label(name == view.active_scene, scene_title(name))
                    .clicked()
                {
                    config.scene.clone_from(name);
                    edits.push(OverlayAction::Scene(name.clone()));
                }
            }
        });
}

fn scenes_tab(
    ui: &mut egui::Ui,
    config: &mut Config,
    view: &OverlayView<'_>,
    colour: egui::Color32,
    edits: &mut Edits,
) {
    if switch_row(
        ui,
        &mut config.auto,
        "Auto",
        "Scenes follow the music. Off keeps the scene you pick; Left and Right change it during the show.",
        colour,
    ) {
        edits.push(OverlayAction::Look);
    }
    if config.auto {
        ui.horizontal(|ui| {
            ui.label("Change scenes");
            for (mode, name) in [
                (AutoChange::Tracks, "per track"),
                (AutoChange::Drops, "+ at drops"),
                (AutoChange::Phrases, "+ every few phrases"),
            ] {
                if ui.radio(config.auto_change == mode, name).clicked() && config.auto_change != mode {
                    config.auto_change = mode;
                    edits.push(OverlayAction::Look);
                }
            }
        });
    }
    section_help(ui, "The scenes", "Click a picture to put that scene on the preview and adjust it below. Ticked scenes are the ones Auto chooses from.");
    let max_cost = config.quality.max_cost();
    let mut rotation: Vec<String> = config.rotation.clone().unwrap_or_else(|| {
        view.scenes
            .iter()
            .filter(|n| scene_info(n).is_some_and(|i| i.auto))
            .cloned()
            .collect()
    });
    let mut rotation_changed = false;
    let thumbs = crate::thumbs::textures(ui.ctx());
    let card_w = (ui.available_width() - 12.0) / 2.0;
    let img = egui::vec2(card_w - 4.0, (card_w - 4.0) * 9.0 / 16.0);
    for pair in view.scenes.chunks(2) {
        ui.horizontal(|ui| {
            for name in pair {
                ui.vertical(|ui| {
                    ui.set_width(card_w);
                    let info = scene_info(name);
                    let active = name == view.active_scene;
                    let picture = if let Some(t) = thumbs.get(name.as_str()) {
                        ui.add(egui::Image::new((t.id(), img)).sense(egui::Sense::click()))
                    } else {
                        ui.add_sized(img, egui::Button::new(scene_title(name)))
                    };
                    if active {
                        ui.painter().rect_stroke(picture.rect, 3.0, egui::Stroke::new(2.0, colour), egui::StrokeKind::Outside);
                    }
                    let picture = match info {
                        Some(i) => picture.on_hover_text(i.blurb),
                        None => picture,
                    };
                    if picture.clicked() {
                        edits.push(OverlayAction::Scene(name.clone()));
                    }
                    ui.horizontal(|ui| {
                        let mut on = rotation.contains(name);
                        if ui.checkbox(&mut on, "").on_hover_text("In the Auto rotation").changed() {
                            if on {
                                rotation.push(name.clone());
                            } else {
                                rotation.retain(|n| n != name);
                            }
                            rotation_changed = true;
                        }
                        let title = egui::RichText::new(scene_title(name)).strong();
                        ui.label(if active { title.color(colour) } else { title });
                        if let Some(i) = info {
                            let tag = match i.cost {
                                3 => "heavy",
                                2 => "medium",
                                _ => "light",
                            };
                            let t = egui::RichText::new(tag).small();
                            ui.label(if i.cost > max_cost { t.color(egui::Color32::from_rgb(230, 140, 60)) } else { t.weak() })
                                .on_hover_text(if i.cost > max_cost { "Auto skips it at this quality; raise Quality on the Output tab." } else { "How hard it works the graphics card." });
                        }
                    });
                });
            }
        });
        ui.add_space(6.0);
    }
    tweak_editor(ui, config, view, edits);
    if rotation_changed {
        config.rotation = Some(rotation);
        edits.push(OverlayAction::Look);
    }
    ui.add_space(8.0);
    if ui.button("Reset the rotation").clicked() {
        config.rotation = None;
        edits.push(OverlayAction::Look);
    }
}

/// Speed, intensity and colour for the scene on the preview.
fn tweak_editor(ui: &mut egui::Ui, config: &mut Config, view: &OverlayView<'_>, edits: &mut Edits) {
    let name = view.active_scene.to_string();
    section_help(ui, &format!("Adjust {}", scene_title(&name)), "These apply to this scene only, wherever it appears: in Auto, or picked by hand.");
    if let Some(i) = scene_info(&name) {
        ui.label(egui::RichText::new(i.blurb).small().weak());
    }
    let mut t = config.tweaks.get(&name).copied().unwrap_or_default();
    let mut changed = false;
    changed |= ui.add(egui::Slider::new(&mut t.speed, 0.25..=2.0).text("Speed").step_by(0.05)).changed();
    changed |= ui.add(egui::Slider::new(&mut t.intensity, 0.0..=2.0).text("Reacts to the music").step_by(0.05)).changed();
    changed |= ui.add(egui::Slider::new(&mut t.hue, 0.0..=1.0).text("Colour shift").step_by(0.01)).changed();
    ui.horizontal(|ui| {
        if ui.button("Reset this scene").clicked() {
            t = SceneTweak::default();
            changed = true;
        }
    });
    if changed {
        if t == SceneTweak::default() {
            config.tweaks.remove(&name);
        } else {
            config.tweaks.insert(name, t);
        }
        edits.push(OverlayAction::Look);
    }
}

/// One effect slider with its explanation and a live meter of what it is doing.
fn effect_slider(
    ui: &mut egui::Ui,
    value: &mut f32,
    range: std::ops::RangeInclusive<f32>,
    title: &str,
    detail: &str,
    live: Option<f32>,
    colour: egui::Color32,
) -> bool {
    ui.add_space(6.0);
    let changed = ui
        .add(egui::Slider::new(value, range).text(title).show_value(false))
        .changed();
    ui.label(egui::RichText::new(detail).small().weak());
    if let Some(level) = live {
        meter(ui, level, colour);
    }
    changed
}

fn effects_tab(
    ui: &mut egui::Ui,
    config: &mut Config,
    view: &OverlayView<'_>,
    colour: egui::Color32,
    edits: &mut Edits,
) {
    ui.label("How strongly the show reacts. Start from a style, then adjust; the bars under the sliders show each effect as it happens.");
    preset_buttons(ui, config, colour, edits);
    if ui.button("Preview a drop").clicked() {
        edits.actions.push(OverlayAction::Preview(ShowEvent::Drop));
    }
    let mut fx = config.preset.fx().unwrap_or(config.fx);
    let live = view.fx;
    let mut changed = false;
    section(ui, "Movement");
    changed |= effect_slider(ui, &mut fx.reactivity, 0.0..=2.0, "Audio reactivity",
        "How far the scenes move with the bass, mids, treble and drums.", Some(view.audio.loudness), colour);
    changed |= effect_slider(ui, &mut fx.shake, 0.0..=1.0, "Camera shake",
        "The picture jolts on kicks during drops.", Some(live.shake), colour);
    changed |= effect_slider(ui, &mut fx.emphasis, 0.0..=1.0, "Overlay pulse",
        "The card and HUD grow for a moment on a new track or a drop.", Some(live.emphasis), colour);
    section_help(ui, "Build-ups and phrase changes", "rekordbox's phrase analysis says when a drop is coming and when a phrase ends; these animate the approach and the change.");
    changed |= effect_slider(ui, &mut fx.buildup, 0.0..=1.0, "Build-ups",
        "Light bars sweep across the screen before a drop, faster as it nears, with a slow push in and a colour cycle.", Some(live.build), colour);
    changed |= effect_slider(ui, &mut fx.moves, 0.0..=1.0, "Phrase slides",
        "On every new phrase the picture slides and a band of light crosses the screen.", Some(live.phrase_move), colour);
    ui.horizontal(|ui| {
        if ui.button("Preview a build-up").clicked() {
            edits.actions.push(OverlayAction::PreviewBuild);
        }
        if ui.button("Preview a phrase change").clicked() {
            edits.actions.push(OverlayAction::Preview(ShowEvent::Phrase(None)));
        }
    });
    section(ui, "Light");
    changed |= effect_slider(ui, &mut fx.flashes, 0.0..=1.0, "Flashes",
        "White flash at drops, tinted flashes when the playhead passes a cue.", Some(live.flash), colour);
    changed |= effect_slider(ui, &mut fx.bloom, 0.0..=1.5, "Glow",
        "Bloom around bright light.", None, colour);
    changed |= effect_slider(ui, &mut fx.trails, 0.0..=1.0, "Trails",
        "Scenes that use feedback leave light trails behind.", None, colour);
    section(ui, "Colour and surprises");
    changed |= effect_slider(ui, &mut fx.colour, 0.0..=1.0, "Colour changes",
        "Hue swings at drops and new phrases; the palette always starts from the cover.", Some(live.phrase_hit), colour);
    changed |= effect_slider(ui, &mut fx.inversions, 0.0..=1.0, "Inversions",
        "Light and dark swap for a beat at some drops.", Some(live.invert), colour);
    changed |= effect_slider(ui, &mut fx.glitch, 0.0..=1.0, "Glitches",
        "Slices of the picture tear on track changes and cues.", Some(live.glitch), colour);
    changed |= effect_slider(ui, &mut fx.chaos, 0.0..=1.0, "Randomness",
        "How often Onset surprises: unexpected colour turns, glitches, bolder scene picks.", None, colour);
    changed |= effect_slider(ui, &mut fx.grain, 0.0..=1.0, "Film grain",
        "A little texture over the whole picture.", None, colour);
    section(ui, "Strobe");
    let before = fx.strobe;
    ui.checkbox(&mut fx.strobe, "Strobe after drops");
    ui.label(egui::RichText::new("Can trigger seizures in people with photosensitive epilepsy. Onset never flashes faster than three times a second, but check with the venue before turning it on.").small().color(egui::Color32::from_rgb(230, 140, 60)));
    changed |= before != fx.strobe;
    if changed {
        config.fx = fx;
        config.preset = Preset::Custom;
        edits.push(OverlayAction::Look);
    }
}

fn screen_tab(ui: &mut egui::Ui, config: &mut Config, colour: egui::Color32, edits: &mut Edits) {
    ui.label("What sits on top of the visuals: the Now Playing card for the crowd, the HUD for you.");
    section_help(ui, "Now Playing card", "The track's cover, title and artist, with the details you tick below. It swaps with a crossfade when the MASTER deck changes.");
    if switch_row(ui, &mut config.show_card, "Show the card", "C toggles it during the show.", colour) {
        edits.push(OverlayAction::ShowCard(config.show_card));
    }
    let mut changed = false;
    let c = &mut config.card;
    egui::ComboBox::from_label("Corner")
        .selected_text(c.corner.label())
        .show_ui(ui, |ui| {
            for corner in Corner::ALL {
                changed |= ui.selectable_value(&mut c.corner, corner, corner.label()).changed();
            }
        });
    changed |= ui.add(egui::Slider::new(&mut c.size, 0.6..=1.6).text("Size").step_by(0.05)).changed();
    egui::Grid::new("card-fields").num_columns(3).spacing([16.0, 2.0]).show(ui, |ui| {
        changed |= tick(ui, &mut c.artwork, "Cover art");
        changed |= tick(ui, &mut c.album, "Album");
        changed |= tick(ui, &mut c.year, "Year");
        ui.end_row();
        changed |= tick(ui, &mut c.key, "Key");
        changed |= tick(ui, &mut c.bpm, "BPM");
        changed |= tick(ui, &mut c.genre, "Genre");
        ui.end_row();
        changed |= tick(ui, &mut c.label, "Label");
        changed |= tick(ui, &mut c.rating, "Rating");
        changed |= tick(ui, &mut c.tags, "My Tags");
        ui.end_row();
        changed |= tick(ui, &mut c.comment, "Comment");
        changed |= tick(ui, &mut c.play_count, "Play count");
        ui.end_row();
    });
    ui.label(egui::RichText::new("Label, rating, My Tags, comment and play count come from your rekordbox collection.").small().weak());

    section_help(ui, "HUD", "A technical readout for you, not the crowd: frame time, the rekordbox link, the phrase and the drop countdown.");
    if switch_row(ui, &mut config.show_hud, "Show the HUD", "H toggles it during the show.", colour) {
        edits.push(OverlayAction::ShowHud(config.show_hud));
    }
    let h = &mut config.hud;
    changed |= ui.add(egui::Slider::new(&mut h.size, 0.6..=2.0).text("Text size").step_by(0.05)).changed();
    egui::Grid::new("hud-fields").num_columns(3).spacing([16.0, 2.0]).show(ui, |ui| {
        changed |= tick(ui, &mut h.scene, "Scene");
        changed |= tick(ui, &mut h.performance, "Frame time");
        changed |= tick(ui, &mut h.transport, "Link and playhead");
        ui.end_row();
        changed |= tick(ui, &mut h.tempo, "BPM");
        changed |= tick(ui, &mut h.key, "Key");
        changed |= tick(ui, &mut h.phrase, "Phrase");
        ui.end_row();
        changed |= tick(ui, &mut h.drop, "Drop countdown");
        changed |= tick(ui, &mut h.next_cue, "Next cue");
        changed |= tick(ui, &mut h.analysis, "Bands and vocals");
        ui.end_row();
    });
    if changed {
        edits.push(OverlayAction::Look);
    }
}

fn lyrics_tab(
    ui: &mut egui::Ui,
    config: &mut Config,
    view: &OverlayView<'_>,
    colour: egui::Color32,
    edits: &mut Edits,
) {
    ui.label("Synced lyrics, line by line, timed to the playhead of the MASTER deck.");
    if !view.lyrics_status.is_empty() {
        ui.label(egui::RichText::new(view.lyrics_status).strong());
    }
    let l = &mut config.lyrics;
    let mut changed = switch_row(ui, &mut l.enabled, "Show lyrics", "Only while a track with lyrics is playing; nothing appears for instrumentals.", colour);
    changed |= switch_row(ui, &mut l.online, "Find lyrics online", "Looks each track up on LRCLIB, a free lyrics database, the first time it plays, and keeps a copy. Only the title, artist, album and length are sent.", colour);
    section_help(ui, "Style", "How the lyrics look. Try each with the preview buttons below.");
    for style in LyricStyle::ALL {
        ui.horizontal(|ui| {
            changed |= ui.radio_value(&mut l.style, style, style.label()).changed();
            ui.label(egui::RichText::new(style.blurb()).small().weak());
        });
    }
    ui.horizontal(|ui| {
        ui.label("Place");
        for place in LyricPlace::ALL {
            changed |= ui.radio_value(&mut l.place, place, place.label()).changed();
        }
    });
    changed |= ui.add(egui::Slider::new(&mut l.size, 0.6..=2.0).text("Size").step_by(0.05)).changed();
    section_help(ui, "At drops", "How much the lyrics move when the music does: a colour split, a rainbow sweep and a bounce on the kick at drops and build-ups. Nothing flashes.");
    changed |= effect_slider(ui, &mut l.drop_fx, 0.0..=1.0, "Drop animation",
        "0 keeps the lyrics still; 1 lets them dance with the drop.", Some(view.fx.drop_hit), colour);
    changed |= ui.add(egui::Slider::new(&mut l.offset_s, -2.0..=2.0).text("Timing (s)").step_by(0.05))
        .on_hover_text("Move the lyrics earlier (negative) or later (positive) if a track's lyrics are off.")
        .changed();
    ui.horizontal(|ui| {
        if ui.button("Preview a drop").clicked() {
            edits.actions.push(OverlayAction::Preview(ShowEvent::Drop));
        }
        if ui.button("Preview a build-up").clicked() {
            edits.actions.push(OverlayAction::PreviewBuild);
        }
    });
    if changed {
        edits.push(OverlayAction::Look);
    }
}

fn output_tab(ui: &mut egui::Ui, config: &mut Config, view: &OverlayView<'_>, edits: &mut Edits) {
    section(ui, "Screen");
    monitor_picker(ui, config, view, edits);
    ui.label(egui::RichText::new("Applies when the show starts. Each screen is listed with its resolution and Windows scaling.").small().weak());

    section(ui, "Graphics card");
    ui.label(format!("Rendering on: {}", view.adapter));
    let current = config.gpu.clone().unwrap_or_else(|| "Automatic (fastest)".to_string());
    egui::ComboBox::from_id_salt("gpu")
        .width(ui.available_width() - 10.0)
        .selected_text(current)
        .show_ui(ui, |ui| {
            if ui.selectable_label(config.gpu.is_none(), "Automatic (fastest)").clicked() {
                config.gpu = None;
                edits.changed = true;
            }
            for name in view.adapters {
                if ui.selectable_label(config.gpu.as_deref() == Some(name.as_str()), name).clicked() {
                    config.gpu = Some(name.clone());
                    edits.changed = true;
                }
            }
        });
    ui.label(egui::RichText::new("Applies at the next start. Laptops with two GPUs should use the dedicated one (NVIDIA or AMD).").small().weak());

    section(ui, "Quality");
    for q in Quality::ALL {
        let detail = match q {
            Quality::Low => "Half resolution, simple scenes. For integrated graphics.",
            Quality::Medium => "Three-quarter resolution; heavy scenes stay out of Auto.",
            Quality::High => "Full resolution, every scene. Good for a gaming laptop.",
            Quality::Ultra => "Full resolution with extra ray-march detail.",
        };
        if ui.radio(config.quality == q, format!("{}  -  {detail}", q.label())).clicked() && config.quality != q {
            config.quality = q;
            edits.push(OverlayAction::Look);
        }
    }
    ui.label(format!("Frame time: {:.1} ms", view.frame_ms));

    section(ui, "Timing");
    ui.horizontal(|ui| {
        for (mode, name) in [
            (PresentModeChoice::Fifo, "VSync (smooth, never tears)"),
            (PresentModeChoice::Mailbox, "Low latency"),
        ] {
            if ui.radio(config.present_mode == mode, name).clicked() && config.present_mode != mode {
                config.present_mode = mode;
                edits.push(OverlayAction::PresentMode(mode));
            }
        }
    });
    let mut scale = config.internal_scale;
    if ui
        .add(egui::Slider::new(&mut scale, onset_render::renderer::MIN_INTERNAL_SCALE..=1.0).text("Extra resolution scale").step_by(0.05))
        .changed()
    {
        config.internal_scale = scale;
        edits.push(OverlayAction::InternalScale(scale));
    }
}

fn audio_tab(
    ui: &mut egui::Ui,
    config: &mut Config,
    view: &OverlayView<'_>,
    endpoints: &[String],
    colour: egui::Color32,
    edits: &mut Edits,
) {
    ui.label("Onset listens to the mix to follow the drums and the spectrum.");
    section_help(ui, "Listening to", "Automatic picks a DJ controller's recording input when one is plugged in (for a DDJ-FLX10 on ASIO that is 'Microphone (DDJ-FLX10)', which carries the master mix) and otherwise listens to what the speakers play. Unplug the controller and Onset moves to the speakers; plug it back in and it moves back.");
    ui.label(format!("In use: {}", view.audio_device));
    ui.horizontal(|ui| {
        let current = config.audio_device.clone().unwrap_or_else(|| "Automatic".to_string());
        egui::ComboBox::from_id_salt("audio")
            .width(ui.available_width() - 90.0)
            .selected_text(current)
            .show_ui(ui, |ui| {
                if ui.selectable_label(config.audio_device.is_none(), "Automatic").clicked() {
                    config.audio_device = None;
                    edits.push(OverlayAction::Engine(EngineCommand::SetAudioDevice(None)));
                }
                for name in endpoints {
                    if ui.selectable_label(config.audio_device.as_deref() == Some(name.as_str()), name).clicked() {
                        config.audio_device = Some(name.clone());
                        edits.push(OverlayAction::Engine(EngineCommand::SetAudioDevice(Some(name.clone()))));
                    }
                }
            });
        if ui.button("Rescan").on_hover_text("List the devices again after plugging something in.").clicked() {
            edits.rescan_audio = true;
        }
    });
    ui.label(egui::RichText::new("Changes apply immediately.").small().weak());

    section(ui, "What Onset hears");
    let audio = view.audio;
    let (rect, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 70.0), egui::Sense::hover());
    ui.painter().rect_filled(rect, 4.0, egui::Color32::from_gray(20));
    let bands = audio.levels.len() as f32;
    let bar_w = rect.width() / bands;
    for (i, level) in audio.levels.iter().enumerate() {
        let h = rect.height() * level.clamp(0.0, 1.0);
        let x = rect.left() + i as f32 * bar_w;
        let bar = egui::Rect::from_min_max(egui::pos2(x + 1.0, rect.bottom() - h), egui::pos2(x + bar_w - 1.0, rect.bottom()));
        ui.painter().rect_filled(bar, 1.0, colour);
    }
    ui.horizontal(|ui| {
        for (name, v) in [("kick", audio.kick), ("snare", audio.snare), ("hats", audio.hat)] {
            let (r, _) = ui.allocate_exact_size(egui::vec2(12.0, 12.0), egui::Sense::hover());
            let c = egui::Color32::from_gray(50).lerp_to_gamma(colour, v.clamp(0.0, 1.0));
            ui.painter().circle_filled(r.center(), 5.0, c);
            ui.label(name);
        }
    });
    if audio.silent {
        ui.label(egui::RichText::new("Silence. If music is playing, choose the device that carries it above.").color(egui::Color32::from_rgb(230, 140, 60)));
    } else {
        ui.label(egui::RichText::new("Bass on the left, treble on the right, each band scaled to its own level.").small().weak());
    }

    section_help(ui, "What rekordbox's analysis says", "rekordbox analyses every track for its low, mid and high energy and where the vocals are. Onset reads that at the playhead, so scenes know what the track is doing with no delay: vocals light things up, the bands move different parts.");
    let a = view.analysis;
    for (name, level) in [("Low", a.low), ("Mid", a.mid), ("High", a.high), ("Vocals", a.vocal)] {
        ui.horizontal(|ui| {
            ui.add_sized(egui::vec2(52.0, 14.0), egui::Label::new(egui::RichText::new(name).small()));
            meter(ui, level, colour);
        });
    }
}

fn help_tab(ui: &mut egui::Ui, simulator: bool) {
    section(ui, "How Onset works");
    ui.label("Onset reads rekordbox as it plays: which tracks are loaded, which deck is MASTER, where each playhead is, and the analysis rekordbox already has (beat grid, phrases like intro, chorus and breakdown, hot cues, key, genre, the 3-band waveform and where the vocals are). It listens to the mix for the drums and the spectrum. Scenes use both: bass, mids, treble and vocals move different parts, build-ups sweep light across the screen, drops land with a flash and a new scene, cues tint the light.");
    section(ui, "Getting started");
    for (i, step) in [
        "Start rekordbox in Performance mode and load a track.",
        "Pick the screen for the show and a style on the Show tab.",
        "Press Start show. Esc comes back here; nothing you change here stops the music.",
        "After a rekordbox update, run the calibration once on the Setup tab.",
    ]
    .iter()
    .enumerate()
    {
        ui.label(format!("{}. {step}", i + 1));
    }
    shortcuts_section(ui, simulator);
    section(ui, "Tips");
    ui.label("Start the show before rekordbox if you like; it connects by itself. After a rekordbox update, run the calibration once on the Setup tab.");
}

fn calibration_section(ui: &mut egui::Ui, cal: &CalibrationView<'_>, edits: &mut Edits) {
    section(ui, "rekordbox calibration");
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
            if ui
                .button("Calibrate this rekordbox (about two minutes)")
                .clicked()
            {
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
    section(ui, "Shortcuts during the show");
    egui::Grid::new("shortcuts")
        .num_columns(2)
        .spacing([18.0, 4.0])
        .show(ui, |ui| {
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
        let (light, _, hint) = readiness(&EngineStatus::Waiting(
            "no offsets for rekordbox 7.2.19".into(),
        ));
        assert_eq!(light, Light::Amber);
        assert!(hint.contains("calibration"));
        assert_eq!(readiness(&EngineStatus::Idle).0, Light::Green);
        let (light, verdict, _) = readiness(&EngineStatus::Running {
            source: "rekordbox".into(),
            track: "Artist - Title".into(),
        });
        assert_eq!(light, Light::Green);
        // The header names the track on its own line; the verdict is only the link.
        assert!(!verdict.contains("Artist - Title"), "{verdict}");
        assert!(verdict.contains("following the playing deck"), "{verdict}");
        let (light, _, hint) = readiness(&EngineStatus::Waiting(
            "rekordbox found, finding its decks".into(),
        ));
        assert_eq!(light, Light::Amber);
        assert!(hint.contains("20 seconds"));
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
