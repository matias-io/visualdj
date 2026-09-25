//! Orchestrates a frame: the show director and autopilot react to the music, the active
//! scene (two during a transition) renders into the stage's HDR targets, the stage adds
//! bloom and the effects and writes the output, then the overlays (Now Playing card, HUD)
//! go on top.
use std::path::{Path, PathBuf};
use std::time::Instant;

use onset_core::music_state::{DEFAULT_THEME, MusicState};
use onset_core::show::{
    AutoChange, AutoPilot, Fx, FxSettings, SceneProfile, ShowDirector, ShowEvent, Transition, Vibe,
    vibe,
};
use serde::{Deserialize, Serialize};

use crate::artwork::{ArtworkLoader, palette};
use crate::card::{Card, CardFrame};
use crate::gpu::Gpu;
use crate::hot_reload::ShaderWatcher;
use crate::hud::{Hud, HudInfo};
use crate::scene::{FrameBindings, HISTORY_COLS, Scene, ShaderError};
use crate::scenes::scene_info;
use crate::stage::{HDR_FORMAT, PostUniforms, Stage};
use crate::text::{TextItem, TextLayer};
use crate::uniforms::{FrameExtras, FrameUniforms};

#[derive(Debug, Clone, Copy, Default)]
pub struct FrameStats {
    /// CPU time spent recording and submitting the frame.
    pub cpu_ms: f32,
}

/// Lowest internal scale offered; below this the upscale looks like a mistake.
pub const MIN_INTERNAL_SCALE: f32 = 0.5;

/// Rendering detail. Higher levels raise the internal resolution, the ray-march steps
/// scenes take, the bloom depth, and allow the heaviest scenes into Auto mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Quality {
    Low,
    Medium,
    #[default]
    High,
    Ultra,
}

impl Quality {
    pub const ALL: [Self; 4] = [Self::Low, Self::Medium, Self::High, Self::Ultra];

    /// The value scenes read as `quality()`.
    pub fn code(self) -> f32 {
        match self {
            Self::Low => 0.0,
            Self::Medium => 1.0,
            Self::High => 2.0,
            Self::Ultra => 3.0,
        }
    }

    pub fn internal_scale(self) -> f32 {
        match self {
            Self::Low => 0.5,
            Self::Medium => 0.75,
            Self::High | Self::Ultra => 1.0,
        }
    }

    pub fn bloom_levels(self) -> usize {
        match self {
            Self::Low => 3,
            Self::Medium => 4,
            Self::High | Self::Ultra => 5,
        }
    }

    /// Most expensive scene (by `SceneInfo::cost`) Auto mode may pick.
    pub fn max_cost(self) -> u8 {
        match self {
            Self::Low => 1,
            Self::Medium => 2,
            Self::High | Self::Ultra => 3,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Low => "Low",
            Self::Medium => "Medium",
            Self::High => "High",
            Self::Ultra => "Ultra",
        }
    }
}

/// Everything about the look the DJ controls.
#[derive(Debug, Clone, PartialEq)]
pub struct RenderSettings {
    pub fx: FxSettings,
    pub quality: Quality,
    /// Auto mode picks and changes scenes; otherwise the chosen scene stays on.
    pub auto: bool,
    pub auto_change: AutoChange,
    /// Scenes in the Auto rotation; `None` uses each scene's default.
    pub rotation: Option<Vec<String>>,
    /// Per-scene speed, intensity and colour shift, by scene name.
    pub tweaks: std::collections::BTreeMap<String, onset_core::show::SceneTweak>,
}

impl Default for RenderSettings {
    fn default() -> Self {
        Self {
            fx: FxSettings::default(),
            quality: Quality::default(),
            auto: true,
            auto_change: AutoChange::default(),
            rotation: None,
            tweaks: std::collections::BTreeMap::new(),
        }
    }
}

/// Length of the launcher's build-up preview.
const BUILD_PREVIEW_S: f32 = 4.0;

struct ActiveTransition {
    from: usize,
    to: usize,
    started: f32,
    seconds: f32,
    kind: Transition,
}

fn transition_code(t: Transition) -> f32 {
    match t {
        Transition::Crossfade => 0.0,
        Transition::Flash => 1.0,
        Transition::ZoomBlur => 2.0,
        Transition::Glitch => 3.0,
        Transition::Wipe => 4.0,
    }
}

#[allow(clippy::struct_excessive_bools)] // independent switches: card, HUD, blackout, passthrough
pub struct Renderer {
    format: wgpu::TextureFormat,
    bindings: FrameBindings,
    stage: Stage,
    scenes: Vec<Box<dyn Scene>>,
    active: usize,
    size: (u32, u32),
    /// The most recent hot-reload failure, shown by the HUD until a good reload clears it.
    last_error: Option<String>,
    /// The engine's status line for the HUD, set by the app each frame.
    source_line: String,
    text: TextLayer,
    card: Card,
    hud: Hud,
    show_card: bool,
    show_hud: bool,
    /// One line about the current track's lyrics, for the launcher.
    lyrics_status: String,
    lyrics: crate::lyrics::LyricsLayer,
    lyrics_options: crate::overlay_options::LyricsOptions,
    /// When a launcher build-up preview started (renderer clock), while it runs.
    build_preview: Option<f32>,
    hud_options: crate::overlay_options::HudOptions,
    card_options: crate::overlay_options::CardOptions,
    /// Window DPI factor; the HUD scales with it, the card scales with the frame height.
    scale: f32,
    last_cpu_ms: f32,
    /// Scenes render at `internal_scale` of the output; the composite upscales.
    internal_scale: f32,
    /// Output nothing but black: the DJ's panic button and the end-of-set fade target.
    blackout: bool,
    last_frame: Option<Instant>,
    /// Seconds since start as of the current frame (transition timing).
    frame_clock: f32,
    /// Skip tonemapping and effects (tests that check exact colours).
    passthrough: bool,

    settings: RenderSettings,
    show: ShowDirector,
    autopilot: AutoPilot,
    transition: Option<ActiveTransition>,
    frame_index: u64,
    vibe: Vibe,
    last_events: Vec<ShowEvent>,
    art_loader: ArtworkLoader,
    art_path: Option<PathBuf>,
    palette_now: [[f32; 3]; 5],
    palette_target: [[f32; 3]; 5],
}

impl Renderer {
    pub fn new(gpu: &Gpu, size: (u32, u32), format: wgpu::TextureFormat) -> Self {
        let settings = RenderSettings::default();
        let stage = Stage::new(gpu, format, size, settings.quality.bloom_levels());
        let mut bindings = FrameBindings::new(gpu);
        bindings.set_prev_views(gpu, stage.mix_views());
        Self {
            format,
            bindings,
            stage,
            scenes: Vec::new(),
            active: 0,
            size,
            last_error: None,
            source_line: String::new(),
            text: TextLayer::new(gpu, format),
            card: Card::new(gpu, format),
            hud: Hud::new(),
            show_card: true,
            build_preview: None,
            lyrics_status: String::new(),
            lyrics: crate::lyrics::LyricsLayer::new(),
            lyrics_options: crate::overlay_options::LyricsOptions::default(),
            hud_options: crate::overlay_options::HudOptions::default(),
            card_options: crate::overlay_options::CardOptions::default(),
            show_hud: false,
            scale: 1.0,
            last_cpu_ms: 0.0,
            internal_scale: 1.0,
            blackout: false,
            last_frame: None,
            frame_clock: 0.0,
            passthrough: false,
            settings,
            show: ShowDirector::new(),
            autopilot: AutoPilot::new(),
            transition: None,
            frame_index: 0,
            vibe: Vibe {
                energy: 0.5,
                darkness: 0.5,
            },
            last_events: Vec::new(),
            art_loader: ArtworkLoader::new(),
            art_path: None,
            palette_now: DEFAULT_THEME,
            palette_target: DEFAULT_THEME,
        }
    }

    /// The format scene pipelines must target.
    pub fn scene_format(&self) -> wgpu::TextureFormat {
        HDR_FORMAT
    }

    pub fn output_format(&self) -> wgpu::TextureFormat {
        self.format
    }

    pub fn set_passthrough(&mut self, on: bool) {
        self.passthrough = on;
    }

    pub fn set_settings(&mut self, gpu: &Gpu, settings: RenderSettings) {
        let quality_changed = settings.quality != self.settings.quality;
        self.settings = settings;
        if quality_changed {
            self.rebuild_targets(gpu);
        }
    }

    pub fn settings(&self) -> &RenderSettings {
        &self.settings
    }

    /// The effects on screen this frame (for the launcher's meters).
    pub fn fx(&self) -> Fx {
        self.show.fx()
    }

    pub fn vibe(&self) -> Vibe {
        self.vibe
    }

    /// Plays an event through the show director (and Auto mode) on the next frame.
    pub fn preview_event(&mut self, event: ShowEvent) {
        self.show.inject(event);
    }

    /// One line about the current track's lyrics.
    pub fn lyrics_status(&self) -> &str {
        &self.lyrics_status
    }

    /// The lyrics for `track` (none for an instrumental or a miss) and the launcher's line
    /// about them.
    pub fn set_lyrics(
        &mut self,
        track: onset_core::track::TrackId,
        lyrics: Option<std::sync::Arc<onset_core::lyrics::Lyrics>>,
        status: String,
    ) {
        self.lyrics.set(track, lyrics);
        self.lyrics_status = status;
    }

    pub fn set_lyrics_options(&mut self, options: crate::overlay_options::LyricsOptions) {
        self.lyrics_options = options;
    }

    /// Plays a four-second build-up on the preview, ending in a drop.
    pub fn preview_build(&mut self) {
        self.build_preview = Some(self.frame_clock);
    }

    pub fn last_events(&self) -> &[ShowEvent] {
        &self.last_events
    }

    pub fn palette(&self) -> [[f32; 3]; 5] {
        self.palette_now
    }

    pub fn set_blackout(&mut self, on: bool) {
        self.blackout = on;
    }

    pub fn blackout(&self) -> bool {
        self.blackout
    }

    /// Fraction of the output resolution scenes render at, clamped to
    /// [`MIN_INTERNAL_SCALE`]..=1.0. The quality level's own scale multiplies it.
    pub fn set_internal_scale(&mut self, gpu: &Gpu, scale: f32) {
        self.internal_scale = if scale.is_finite() {
            scale.clamp(MIN_INTERNAL_SCALE, 1.0)
        } else {
            1.0
        };
        self.rebuild_targets(gpu);
    }

    pub fn internal_scale(&self) -> f32 {
        self.internal_scale
    }

    /// The size scenes actually render at.
    pub fn internal_size(&self) -> (u32, u32) {
        let k = (self.internal_scale * self.settings.quality.internal_scale()).max(0.25);
        if k >= 0.999 {
            self.size
        } else {
            (
                ((self.size.0 as f32 * k).round() as u32).max(1),
                ((self.size.1 as f32 * k).round() as u32).max(1),
            )
        }
    }

    fn rebuild_targets(&mut self, gpu: &Gpu) {
        let internal = self.internal_size();
        self.stage
            .resize(gpu, internal, self.settings.quality.bloom_levels());
        self.bindings.set_prev_views(gpu, self.stage.mix_views());
        for s in &mut self.scenes {
            s.resize(gpu, internal);
        }
    }

    pub fn set_source_line(&mut self, line: String) {
        self.source_line = line;
    }

    pub fn set_scale(&mut self, scale: f32) {
        self.scale = scale.max(0.1);
    }

    /// What the HUD and the Now Playing card show, and how big.
    pub fn set_overlay_options(
        &mut self,
        hud: crate::overlay_options::HudOptions,
        card: crate::overlay_options::CardOptions,
    ) {
        self.hud_options = hud;
        self.card_options = card;
    }

    pub fn set_show_card(&mut self, show: bool) {
        self.show_card = show;
    }

    pub fn show_card(&self) -> bool {
        self.show_card
    }

    pub fn set_show_hud(&mut self, show: bool) {
        self.show_hud = show;
    }

    pub fn show_hud(&self) -> bool {
        self.show_hud
    }

    pub fn hud(&self) -> &Hud {
        &self.hud
    }

    pub fn last_error(&self) -> Option<&str> {
        self.last_error.as_deref()
    }

    /// Shows a message in the HUD until the next successful reload clears it.
    pub fn set_last_error(&mut self, message: String) {
        self.last_error = Some(message);
    }

    /// Recompiles a fullscreen scene from new WGSL. On failure the old pipeline stays in use
    /// and the message is kept for the HUD; on success the message is cleared.
    pub fn reload_scene(&mut self, gpu: &Gpu, name: &str, source: &str) -> Result<(), ShaderError> {
        self.reload_scene_with_common(gpu, name, crate::scene::COMMON_WGSL, source)
    }

    /// [`Self::reload_scene`] with the on-disk `common.wgsl` as the prelude.
    pub fn reload_scene_with_common(
        &mut self,
        gpu: &Gpu,
        name: &str,
        common: &str,
        source: &str,
    ) -> Result<(), ShaderError> {
        let scene = self
            .scenes
            .iter_mut()
            .find(|s| s.name() == name)
            .and_then(|s| s.as_fullscreen_mut())
            .ok_or_else(|| ShaderError {
                name: name.to_string(),
                message: "no reloadable scene with that name".to_string(),
            })?;
        match scene.replace_shader_with_common(gpu, common, source) {
            Ok(()) => {
                self.last_error = None;
                tracing::info!(scene = name, "shader reloaded");
                Ok(())
            }
            Err(e) => {
                tracing::error!("{e}");
                self.last_error = Some(e.to_string());
                Err(e)
            }
        }
    }

    /// Applies any shader files the watcher reports changed. `common.wgsl` reloads every scene.
    pub fn poll_hot_reload(&mut self, gpu: &Gpu, watcher: &mut ShaderWatcher, shader_dir: &Path) {
        let changed = watcher.poll();
        if changed.is_empty() {
            return;
        }
        let common = std::fs::read_to_string(shader_dir.join("common.wgsl"))
            .unwrap_or_else(|_| crate::scene::COMMON_WGSL.to_string());
        for stem in changed {
            let targets: Vec<String> = if stem == "common" {
                self.scene_names()
            } else {
                vec![stem]
            };
            for name in targets {
                let path = shader_dir.join(format!("{name}.wgsl"));
                match std::fs::read_to_string(&path) {
                    Ok(src) => {
                        let _ = self.reload_scene_with_common(gpu, &name, &common, &src);
                    }
                    Err(e) => tracing::warn!(path = %path.display(), "cannot read shader: {e}"),
                }
            }
        }
    }

    pub fn bindings(&self) -> &FrameBindings {
        &self.bindings
    }

    pub fn add_scene(&mut self, scene: Box<dyn Scene>) {
        self.scenes.push(scene);
    }

    pub fn scene_names(&self) -> Vec<String> {
        self.scenes.iter().map(|s| s.name().to_string()).collect()
    }

    pub fn active_scene(&self) -> Option<&str> {
        let i = self.transition.as_ref().map_or(self.active, |t| t.to);
        self.scenes.get(i).map(|s| s.name())
    }

    fn start_transition(&mut self, to: usize, kind: Transition, seconds: f32) {
        if to >= self.scenes.len() {
            return;
        }
        let from = self.transition.as_ref().map_or(self.active, |t| t.to);
        if from == to && self.transition.is_none() {
            return;
        }
        let now = self.clock();
        self.transition = Some(ActiveTransition {
            from,
            to,
            started: now,
            seconds: seconds.max(0.05),
            kind,
        });
        self.autopilot.set_current(to);
    }

    fn clock(&self) -> f32 {
        self.frame_clock
    }

    /// Selects a scene by name with a short crossfade; unknown names are ignored.
    pub fn set_scene(&mut self, name: &str) -> bool {
        match self.scenes.iter().position(|s| s.name() == name) {
            Some(i) => {
                if self.frame_index == 0 {
                    // Before the first frame: no transition, just start there.
                    self.active = i;
                    self.autopilot.set_current(i);
                } else {
                    self.start_transition(i, Transition::Crossfade, 0.8);
                }
                true
            }
            None => false,
        }
    }

    pub fn next_scene(&mut self) {
        if !self.scenes.is_empty() {
            let cur = self.transition.as_ref().map_or(self.active, |t| t.to);
            self.start_transition((cur + 1) % self.scenes.len(), Transition::Crossfade, 0.8);
        }
    }

    pub fn prev_scene(&mut self) {
        if !self.scenes.is_empty() {
            let cur = self.transition.as_ref().map_or(self.active, |t| t.to);
            let n = self.scenes.len();
            self.start_transition((cur + n - 1) % n, Transition::Crossfade, 0.8);
        }
    }

    pub fn resize(&mut self, gpu: &Gpu, size: (u32, u32)) {
        self.size = size;
        self.rebuild_targets(gpu);
    }

    /// Scene profiles for Auto mode, in scene order, and which are allowed right now.
    fn rotation(&self) -> (Vec<SceneProfile>, Vec<bool>) {
        let max_cost = self.settings.quality.max_cost();
        self.scenes
            .iter()
            .map(|s| {
                let info = scene_info(s.name());
                let profile = SceneProfile {
                    name: s.name().to_string(),
                    energy: info.map_or(0.5, |i| i.energy),
                    darkness: info.map_or(0.5, |i| i.darkness),
                    cost: info.map_or(1, |i| i.cost),
                };
                let wanted = match &self.settings.rotation {
                    Some(list) => list.iter().any(|n| n == s.name()),
                    None => info.is_some_and(|i| i.auto),
                };
                let allowed = wanted && profile.cost <= max_cost;
                (profile, allowed)
            })
            .unzip()
    }

    /// Cover art for the scenes and the palette, decoded off-thread.
    fn follow_artwork(&mut self, gpu: &Gpu, ms: &MusicState, dt: f32) {
        let wanted = ms.track.as_ref().and_then(|t| t.artwork_path.clone());
        if wanted != self.art_path {
            self.art_path.clone_from(&wanted);
            match &wanted {
                Some(p) => self.art_loader.request(p.clone()),
                None => self.palette_target = DEFAULT_THEME,
            }
        }
        while let Some((path, image)) = self.art_loader.try_take() {
            if self.art_path.as_deref() == Some(path.as_path()) {
                self.bindings.set_artwork(gpu, &image);
                self.palette_target = palette(&image, &DEFAULT_THEME);
                self.vibe = vibe(ms.track.as_ref(), ms.mood, &self.palette_target);
            }
        }
        let k = 1.0 - (-dt / 1.2).exp();
        for (now, target) in self.palette_now.iter_mut().zip(&self.palette_target) {
            for (a, b) in now.iter_mut().zip(target) {
                *a += (b - *a) * k;
            }
        }
    }

    fn post_uniforms(
        &self,
        ms: &MusicState,
        time_s: f32,
        fx: &Fx,
        t: Option<(f32, f32)>,
    ) -> PostUniforms {
        let s = &self.settings.fx;
        // Shake: two slow, incommensurate wobbles. Fast ones read as jitter.
        let amp = fx.shake * 0.008;
        let shake = [
            amp * ((time_s * 7.3).sin() * 0.6 + (time_s * 4.1).sin() * 0.4),
            amp * ((time_s * 6.1).cos() * 0.6 + (time_s * 3.7).sin() * 0.4),
        ];
        let pal = self.palette_now;
        let (progress, kind) = t.unwrap_or((0.0, 0.0));
        let intensity = ms.intensity.clamp(0.0, 1.0);
        PostUniforms {
            res_texel: [
                self.size.0 as f32,
                self.size.1 as f32,
                1.0 / self.size.0.max(1) as f32,
                1.0 / self.size.1.max(1) as f32,
            ],
            a: [time_s, fx.flash, fx.invert, fx.hue],
            flash_col: [
                fx.flash_color[0],
                fx.flash_color[1],
                fx.flash_color[2],
                fx.strobe,
            ],
            motion: [shake[0], shake[1], fx.zoom, fx.glitch],
            look: [
                s.bloom * (0.55 + 0.45 * intensity),
                s.grain,
                0.35,
                0.0015 + 0.004 * fx.drop_hit,
            ],
            tone: [1.0, fx.seed, 0.9, if self.blackout { 1.0 } else { 0.0 }],
            trans: [progress, kind, 0.0, fx.tension],
            build: [fx.build, fx.beat_count, fx.phrase_move, fx.move_angle],
            style: [s.buildup, fx.vocal, 0.0, 0.0],
            accent: [pal[2][0], pal[2][1], pal[2][2], 1.0],
            accent2: [pal[3][0], pal[3][1], pal[3][2], 1.0],
        }
    }

    /// Updates the show, renders the scene(s) through the stage into `target`, then the
    /// card and HUD when enabled.
    #[allow(clippy::too_many_lines)] // one frame, read top to bottom
    pub fn render(
        &mut self,
        gpu: &Gpu,
        encoder: &mut wgpu::CommandEncoder,
        target: &wgpu::TextureView,
        ms: &MusicState,
        time_s: f32,
    ) -> FrameStats {
        let started = Instant::now();
        let interval_ms = self
            .last_frame
            .map(|t| started.duration_since(t).as_secs_f32() * 1000.0);
        self.last_frame = Some(started);
        self.hud.record(interval_ms, self.last_cpu_ms);
        // Effects advance with the clock the caller gives (the app's wall time, a test's
        // synthetic time), not with how fast frames happen to be rendered.
        let dt = if self.frame_index == 0 {
            1.0 / 60.0
        } else {
            (time_s - self.frame_clock).clamp(0.0, 0.1)
        };
        self.frame_clock = time_s;

        // The show: events, effects, and in Auto mode the next scene.
        self.follow_artwork(gpu, ms, dt);
        let events = self.show.update(ms, dt, &self.settings.fx);
        if events.contains(&ShowEvent::Track) {
            self.vibe = vibe(ms.track.as_ref(), ms.mood, &self.palette_target);
        }
        if self.settings.auto && !self.scenes.is_empty() {
            let (profiles, allowed) = self.rotation();
            if let Some(change) = self.autopilot.on_events(
                &events,
                self.vibe,
                &profiles,
                &allowed,
                self.settings.auto_change,
                self.settings.fx.chaos,
            ) {
                tracing::info!(scene = %profiles[change.index].name, ?change.transition, "auto scene");
                self.start_transition(change.index, change.transition, change.seconds);
            }
        }
        if !events.is_empty() {
            self.last_events = events;
        }
        let mut fx = self.show.fx();
        if let Some(start) = self.build_preview {
            let p = (time_s - start) / BUILD_PREVIEW_S;
            if p >= 1.0 {
                self.build_preview = None;
                self.show.inject(ShowEvent::Drop);
            } else {
                fx.build = p.clamp(0.0, 1.0);
                fx.tension = fx.build;
            }
        }

        // Transition bookkeeping.
        let mut transition = None;
        if let Some(t) = &self.transition {
            let p = (time_s - t.started) / t.seconds;
            if !(0.0..1.0).contains(&p) || t.from >= self.scenes.len() {
                self.active = t.to;
                self.transition = None;
            } else {
                transition = Some((t.from, t.to, p, transition_code(t.kind)));
            }
        }

        // Spectrum history and the frame uniforms.
        let mut row = [0.0f32; HISTORY_COLS as usize];
        row[..24].copy_from_slice(&ms.audio.levels);
        row[24..30].copy_from_slice(&ms.audio.groups);
        row[30] = ms.audio.kick;
        row[31] = ms.audio.snare;
        self.bindings.push_history(gpu, &row);
        let internal = self.stage.size();
        let extras = FrameExtras {
            fx,
            vibe: self.vibe,
            reactivity: self.settings.fx.reactivity,
            trails: self.settings.fx.trails,
            quality: self.settings.quality.code(),
            history_row: self.bindings.history_row(),
            theme: Some(self.palette_now),
            tweak: self
                .active_scene()
                .and_then(|n| self.settings.tweaks.get(n))
                .copied()
                .unwrap_or_default(),
        };
        self.bindings.write(
            gpu,
            &FrameUniforms::from_state_with(ms, internal, time_s, &extras),
        );

        if self.blackout || self.scenes.is_empty() {
            clear_to_black(encoder, target);
        }
        if self.blackout {
            let cpu_ms = started.elapsed().as_secs_f32() * 1000.0;
            self.last_cpu_ms = cpu_ms;
            return FrameStats { cpu_ms };
        }

        if !self.scenes.is_empty() {
            let cur = (self.frame_index % 2) as usize;
            self.bindings.select(1 - cur);
            let mut post =
                self.post_uniforms(ms, time_s, &fx, transition.map(|(_, _, p, k)| (p, k)));
            if self.passthrough {
                post.trans[2] = 1.0;
            }
            self.stage.write(gpu, &post);
            if let Some((from, to, _, _)) = transition {
                self.scenes[from].render(gpu, encoder, self.stage.scene_view(0), &self.bindings);
                self.scenes[to].render(gpu, encoder, self.stage.scene_view(1), &self.bindings);
                self.stage.run_transition(gpu, encoder, cur);
            } else {
                let active = self.active.min(self.scenes.len() - 1);
                self.scenes[active].render(gpu, encoder, self.stage.mix_view(cur), &self.bindings);
            }
            self.stage.finish(gpu, encoder, cur, target);
        }
        self.frame_index += 1;

        // Overlays grow a little on emphasis (track change, drop).
        let grow = 1.0 + 0.12 * fx.emphasis;
        let mut items: Vec<TextItem> = self.lyrics.items(
            &mut self.text,
            &crate::lyrics::LyricsFrame {
                size: self.size,
                ms,
                fx: &fx,
                theme: &self.palette_now,
                options: &self.lyrics_options,
                time_s,
            },
        );
        self.card
            .update(gpu, ms.track.as_ref().filter(|_| self.show_card), time_s);
        if self.show_card {
            items.extend(self.card.draw(
                gpu,
                encoder,
                target,
                &mut self.text,
                CardFrame {
                    size: self.size,
                    theme: &self.palette_now,
                    live_bpm: ms.bpm,
                    time_s,
                    emphasis: fx.emphasis,
                    options: &self.card_options,
                },
            ));
        }
        if self.show_hud {
            let info = HudInfo {
                scene: self.active_scene().unwrap_or("-").to_string(),
                size: self.size,
                gpu_ms: None,
                last_error: self.last_error.clone(),
                source: self.source_line.clone(),
            };
            items.extend(
                self.hud
                    .items(ms, &info, self.scale * grow, &self.hud_options),
            );
        }
        if !items.is_empty() {
            match self.text.prepare(gpu, self.size, &items) {
                Ok(()) => {
                    let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                        label: Some("text"),
                        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                            view: target,
                            depth_slice: None,
                            resolve_target: None,
                            ops: wgpu::Operations {
                                load: wgpu::LoadOp::Load,
                                store: wgpu::StoreOp::Store,
                            },
                        })],
                        ..Default::default()
                    });
                    self.text.render(&mut pass);
                }
                Err(e) => tracing::warn!("text prepare: {e}"),
            }
        }
        self.text.trim();

        let cpu_ms = started.elapsed().as_secs_f32() * 1000.0;
        self.last_cpu_ms = cpu_ms;
        FrameStats { cpu_ms }
    }
}

fn clear_to_black(encoder: &mut wgpu::CommandEncoder, target: &wgpu::TextureView) {
    encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
        label: Some("clear"),
        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
            view: target,
            depth_slice: None,
            resolve_target: None,
            ops: wgpu::Operations {
                load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                store: wgpu::StoreOp::Store,
            },
        })],
        ..Default::default()
    });
}
