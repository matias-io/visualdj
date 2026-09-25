//! The show director: what a lighting operator would do with rekordbox's analysis. It
//! watches the music state for events (a new beat, bar or phrase, the drop, a cue passing
//! under the playhead, a new track) and answers with effect envelopes the renderer applies
//! over any scene: flashes, inversions, colour shifts, camera shake, zoom punches, glitches,
//! strobes and a pulse for the overlays. Every effect is scaled by the DJ's settings, and a
//! little seeded randomness keeps two drops from looking identical.
//!
//! The same module picks scenes in Auto mode: each track gets a vibe from its tempo, key,
//! genre, rekordbox mood and cover colours, and the autopilot chooses scenes that match,
//! switching to something bigger at the drop and calmer in the breakdown.
use serde::{Deserialize, Serialize};

use crate::music_state::MusicState;
use crate::phrase::{Mood, PhraseKind};
use crate::track::{TrackId, TrackMeta};

/// Photosensitivity guideline (WCAG 2.3.1): no more than three flashes in any second.
pub const MAX_STROBE_HZ: f32 = 3.0;

/// How strongly each effect responds, 0 = off. Reactivity scales the audio response of the
/// scenes themselves (0..2); the rest scale the director's effects (0..1).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct FxSettings {
    pub reactivity: f32,
    pub flashes: f32,
    pub shake: f32,
    pub colour: f32,
    pub inversions: f32,
    pub glitch: f32,
    pub bloom: f32,
    pub trails: f32,
    pub grain: f32,
    pub chaos: f32,
    /// How much the Now Playing card and HUD grow on a track change or drop.
    pub emphasis: f32,
    /// Full-screen strobe on drops. Off by default: it can trigger photosensitive seizures.
    pub strobe: bool,
}

impl FxSettings {
    pub const CHILL: Self = Self {
        reactivity: 0.7,
        flashes: 0.2,
        shake: 0.0,
        colour: 0.4,
        inversions: 0.0,
        glitch: 0.1,
        bloom: 0.6,
        trails: 0.5,
        grain: 0.25,
        chaos: 0.2,
        emphasis: 0.5,
        strobe: false,
    };
    pub const CLUB: Self = Self {
        reactivity: 1.0,
        flashes: 0.55,
        shake: 0.35,
        colour: 0.65,
        inversions: 0.35,
        glitch: 0.35,
        bloom: 0.8,
        trails: 0.45,
        grain: 0.2,
        chaos: 0.45,
        emphasis: 0.7,
        strobe: false,
    };
    pub const FESTIVAL: Self = Self {
        reactivity: 1.4,
        flashes: 0.9,
        shake: 0.7,
        colour: 0.9,
        inversions: 0.7,
        glitch: 0.6,
        bloom: 1.0,
        trails: 0.4,
        grain: 0.15,
        chaos: 0.75,
        emphasis: 0.9,
        strobe: false,
    };
}

impl Default for FxSettings {
    fn default() -> Self {
        Self::CLUB
    }
}

/// The effect envelopes for one frame. Everything is 0..1 unless noted.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Fx {
    /// Full-screen flash strength and its colour (linear RGB).
    pub flash: f32,
    pub flash_color: [f32; 3],
    /// 1 = colours inverted.
    pub invert: f32,
    /// Accumulated hue rotation in turns (grows without bound; the shader takes `fract`).
    pub hue: f32,
    pub shake: f32,
    pub zoom: f32,
    pub glitch: f32,
    /// 0 or 1 while strobing, never faster than `MAX_STROBE_HZ`.
    pub strobe: f32,
    /// Overlay pulse (card and HUD scale).
    pub emphasis: f32,
    /// Decaying markers of the last drop, phrase change, cue and track change.
    pub drop_hit: f32,
    pub phrase_hit: f32,
    pub cue_hit: f32,
    pub track_hit: f32,
    /// Rises over the 16 beats before an announced drop.
    pub tension: f32,
    /// Beats and bars since the start of the track (fractional).
    pub beat_count: f32,
    pub bar_count: f32,
    /// Per-track random seed in 0..1, so scenes can vary between tracks.
    pub seed: f32,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ShowEvent {
    Beat,
    Bar,
    /// A new phrase that is not a drop or breakdown.
    Phrase(Option<PhraseKind>),
    /// The drop: the phrase turned into a Chorus.
    Drop,
    /// Into a breakdown, bridge or outro.
    Breakdown,
    /// The playhead passed a memory cue (slot 0) or hot cue (1..=8).
    Cue {
        slot: u8,
        color: Option<[u8; 3]>,
    },
    Track,
}

/// Small deterministic generator (xorshift64*), seeded per track.
#[derive(Debug, Clone)]
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Self(seed.max(1))
    }

    pub fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// Uniform in 0..1.
    pub fn unit(&mut self) -> f32 {
        (self.next_u64() >> 40) as f32 / (1u64 << 24) as f32
    }

    /// True with probability `p`.
    pub fn chance(&mut self, p: f32) -> bool {
        self.unit() < p
    }
}

fn decay(value: f32, dt: f32, tau: f32) -> f32 {
    value * (-dt / tau.max(1e-3)).exp()
}

fn approach(value: f32, target: f32, dt: f32, tau: f32) -> f32 {
    value + (target - value) * (1.0 - (-dt / tau.max(1e-3)).exp())
}

fn is_breakdown(kind: Option<PhraseKind>) -> bool {
    matches!(
        kind,
        Some(PhraseKind::Down | PhraseKind::Bridge | PhraseKind::Outro)
    )
}

#[derive(Debug, Clone)]
pub struct ShowDirector {
    rng: Rng,
    fx: Fx,
    last_beat: Option<u32>,
    last_phrase: Option<PhraseKind>,
    last_track: Option<TrackId>,
    last_cue: Option<(u8, f32)>,
    /// Colour of the cue that was ahead last frame.
    cue_color: Option<[u8; 3]>,
    hue_target: f32,
    invert_target: f32,
    invert_hold_s: f32,
    shake_burst: f32,
    zoom_burst: f32,
    strobe_clock: f32,
}

impl Default for ShowDirector {
    fn default() -> Self {
        Self::new()
    }
}

impl ShowDirector {
    pub fn new() -> Self {
        Self {
            rng: Rng::new(0x9E37_79B9_7F4A_7C15),
            fx: Fx {
                flash_color: [1.0, 1.0, 1.0],
                ..Fx::default()
            },
            last_beat: None,
            last_phrase: None,
            last_track: None,
            last_cue: None,
            cue_color: None,
            hue_target: 0.0,
            invert_target: 0.0,
            invert_hold_s: 0.0,
            shake_burst: 0.0,
            zoom_burst: 0.0,
            strobe_clock: 0.0,
        }
    }

    pub fn fx(&self) -> Fx {
        self.fx
    }

    fn beat_s(ms: &MusicState) -> f32 {
        if ms.bpm > 1.0 { 60.0 / ms.bpm } else { 0.5 }
    }

    /// Detects this frame's events from the state (compared with the last frame's).
    fn events(&mut self, ms: &MusicState) -> Vec<ShowEvent> {
        let mut out = Vec::new();
        let track = ms.track.as_ref().map(|t| t.id);
        if track.is_some() && track != self.last_track {
            out.push(ShowEvent::Track);
            self.last_phrase = ms.phrase;
            self.last_beat = ms.beat_index;
            self.last_cue = None;
        }
        self.last_track = track;
        if !ms.playing {
            self.last_cue = ms.next_cue.map(|c| (c.slot, c.seconds));
            return out;
        }
        if let Some(b) = ms.beat_index
            && self.last_beat != Some(b)
        {
            out.push(ShowEvent::Beat);
            if b % 4 == 0 {
                out.push(ShowEvent::Bar);
            }
        }
        self.last_beat = ms.beat_index;

        if ms.phrase != self.last_phrase && !out.contains(&ShowEvent::Track) {
            let into_chorus = ms.phrase == Some(PhraseKind::Chorus);
            if into_chorus && self.last_phrase.is_some() {
                out.push(ShowEvent::Drop);
            } else if is_breakdown(ms.phrase) {
                out.push(ShowEvent::Breakdown);
            } else if ms.phrase.is_some() {
                out.push(ShowEvent::Phrase(ms.phrase));
            }
        }
        self.last_phrase = ms.phrase;

        // A cue has passed when the one ahead changes after the old one was about to arrive.
        let now = ms.next_cue.map(|c| (c.slot, c.seconds));
        if let Some((slot, secs)) = self.last_cue {
            let passed = (0.0..=0.35).contains(&secs)
                && now.is_none_or(|(s, t)| s != slot || t > secs + 0.5);
            if passed {
                out.push(ShowEvent::Cue {
                    slot,
                    color: self.cue_color,
                });
            }
        }
        self.last_cue = now;
        self.cue_color = ms.next_cue.and_then(|c| c.color);
        out
    }

    /// Advances by `dt` seconds and returns the events seen this frame.
    #[allow(clippy::too_many_lines)] // one table of reactions, read top to bottom
    pub fn update(&mut self, ms: &MusicState, dt: f32, s: &FxSettings) -> Vec<ShowEvent> {
        let dt = dt.clamp(0.0, 0.1);
        let events = self.events(ms);
        let beat_s = Self::beat_s(ms);
        let fx = &mut self.fx;

        // Decays first, so an event this frame lands at full strength.
        fx.flash = decay(fx.flash, dt, 0.22);
        fx.phrase_hit = decay(fx.phrase_hit, dt, 1.0);
        fx.cue_hit = decay(fx.cue_hit, dt, 0.6);
        fx.track_hit = decay(fx.track_hit, dt, 2.0);
        fx.drop_hit = decay(fx.drop_hit, dt, beat_s * 4.0);
        fx.emphasis = decay(fx.emphasis, dt, 0.9);
        fx.glitch = decay(fx.glitch, dt, 0.35);
        self.shake_burst = decay(self.shake_burst, dt, 0.4);
        self.zoom_burst = decay(self.zoom_burst, dt, beat_s * 1.5);

        for e in &events {
            match *e {
                ShowEvent::Track => {
                    let id = ms.track.as_ref().map_or(1, |t| t.id.0);
                    self.rng = Rng::new(id.wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ 0xD1B5);
                    fx.seed = self.rng.unit();
                    fx.track_hit = 1.0;
                    fx.emphasis = fx.emphasis.max(s.emphasis);
                    fx.glitch = fx.glitch.max(0.8 * s.glitch);
                    self.hue_target += (self.rng.unit() - 0.5) * s.colour;
                    self.invert_target = 0.0;
                }
                ShowEvent::Drop => {
                    fx.flash = fx.flash.max(s.flashes);
                    fx.flash_color = [1.0, 1.0, 1.0];
                    fx.drop_hit = 1.0;
                    fx.emphasis = fx.emphasis.max(0.6 * s.emphasis);
                    fx.glitch = fx.glitch.max(0.3 * s.glitch);
                    // A clear but tasteful swing; the palette stays the track's own.
                    let sign = if self.rng.chance(0.5) { 1.0 } else { -1.0 };
                    self.hue_target += sign * (0.12 + 0.1 * self.rng.unit()) * s.colour;
                    self.shake_burst = self.shake_burst.max(s.shake);
                    self.zoom_burst = self.zoom_burst.max(0.12 * s.reactivity);
                    if self.rng.chance(s.inversions) {
                        self.invert_target = 1.0;
                        self.invert_hold_s = beat_s;
                    }
                }
                ShowEvent::Breakdown => {
                    fx.phrase_hit = 1.0;
                    fx.flash = fx.flash.max(0.2 * s.flashes);
                    self.hue_target -= (0.1 + 0.15 * self.rng.unit()) * s.colour;
                    self.invert_target = 0.0;
                }
                ShowEvent::Phrase(_) => {
                    fx.phrase_hit = 1.0;
                    fx.flash = fx.flash.max(0.35 * s.flashes);
                    let sign = if self.rng.chance(0.5) { 1.0 } else { -1.0 };
                    self.hue_target += sign * (0.08 + 0.17 * self.rng.unit()) * s.colour;
                    if self.rng.chance(0.5 * s.chaos * s.inversions) {
                        self.invert_target = 1.0;
                        self.invert_hold_s = beat_s;
                    }
                }
                ShowEvent::Cue { color, .. } => {
                    fx.cue_hit = 1.0;
                    fx.flash = fx.flash.max(0.45 * s.flashes);
                    fx.flash_color = color.map_or([1.0, 1.0, 1.0], |c| {
                        [
                            f32::from(c[0]) / 255.0,
                            f32::from(c[1]) / 255.0,
                            f32::from(c[2]) / 255.0,
                        ]
                    });
                    fx.glitch = fx.glitch.max(0.4 * s.glitch);
                }
                ShowEvent::Beat => {
                    if ms.phrase == Some(PhraseKind::Chorus) {
                        fx.flash = fx.flash.max(0.12 * s.flashes * ms.intensity);
                    }
                }
                ShowEvent::Bar => {
                    if self.rng.chance(0.08 * s.chaos) {
                        self.hue_target += (self.rng.unit() - 0.5) * 0.3 * s.colour;
                    }
                    if ms.phrase == Some(PhraseKind::Chorus)
                        && self.rng.chance(0.06 * s.chaos * s.inversions)
                    {
                        self.invert_target = 1.0;
                        self.invert_hold_s = beat_s * 0.5;
                    }
                    if self.rng.chance(0.05 * s.chaos) {
                        fx.glitch = fx.glitch.max(0.5 * s.glitch);
                    }
                }
            }
        }

        // Inversions hold for a while, then fade back.
        if self.invert_hold_s > 0.0 {
            self.invert_hold_s -= dt;
            if self.invert_hold_s <= 0.0 {
                self.invert_target = 0.0;
            }
        }
        fx.invert = approach(
            fx.invert,
            self.invert_target * s.inversions.min(1.0).ceil(),
            dt,
            0.06,
        );

        // Colour: eased towards the target, with a slow drift that follows the energy.
        self.hue_target += s.colour * 0.004 * ms.intensity * dt;
        fx.hue = approach(fx.hue, self.hue_target, dt, 0.7);

        let kick = ms.audio.kick * s.reactivity;
        fx.shake = (kick * s.shake * ms.intensity * 0.6 + self.shake_burst).min(1.0);
        fx.zoom = kick * 0.03 + self.zoom_burst;

        fx.tension = match ms.drop_countdown_beats {
            Some(n) if n <= 16 && ms.phrase != Some(PhraseKind::Chorus) => {
                ((16.0 - n as f32 + ms.beat_phase) / 16.0).clamp(0.0, 1.0)
            }
            _ => 0.0,
        };

        // Strobe: only when enabled, only right after a drop, at most three flashes a second.
        fx.strobe = if s.strobe && fx.drop_hit > 0.3 && ms.playing {
            let mut hz = 1.0 / beat_s;
            while hz > MAX_STROBE_HZ {
                hz *= 0.5;
            }
            self.strobe_clock = (self.strobe_clock + dt * hz).fract();
            if self.strobe_clock < 0.25 { 1.0 } else { 0.0 }
        } else {
            self.strobe_clock = 0.0;
            0.0
        };

        let counted = ms.beat_count();
        fx.beat_count = counted;
        fx.bar_count = counted / 4.0;
        events
    }
}

// ------------------------------------------------------------------------------------------
// Auto mode
// ------------------------------------------------------------------------------------------

/// What a scene is like, for Auto mode: how energetic, how dark, how costly to render.
#[derive(Debug, Clone, PartialEq)]
pub struct SceneProfile {
    pub name: String,
    pub energy: f32,
    pub darkness: f32,
    /// 1 light, 2 medium, 3 heavy (only offered on High and Ultra quality).
    pub cost: u8,
}

/// A track's character for scene choice: 0..1 energy and darkness.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Vibe {
    pub energy: f32,
    pub darkness: f32,
}

const HIGH_ENERGY_GENRES: &[&str] = &[
    "techno",
    "hard",
    "drum",
    "dnb",
    "d&b",
    "dubstep",
    "trance",
    "edm",
    "electro",
    "bass",
    "house",
    "jungle",
    "trap",
    "rave",
    "big room",
    "hardstyle",
    "garage",
    "breaks",
];
const LOW_ENERGY_GENRES: &[&str] = &[
    "ambient",
    "chill",
    "lofi",
    "lo-fi",
    "acoustic",
    "ballad",
    "downtempo",
    "soul",
    "jazz",
    "classical",
    "folk",
    "r&b",
    "rnb",
    "piano",
    "sleep",
];
const DARK_GENRES: &[&str] = &[
    "techno",
    "dark",
    "deep",
    "industrial",
    "dubstep",
    "minimal",
    "drum",
    "dnb",
    "trap",
    "goth",
];
const BRIGHT_GENRES: &[&str] = &[
    "pop",
    "disco",
    "funk",
    "nu-disco",
    "french",
    "k-pop",
    "dance",
    "latin",
    "reggaeton",
    "tropical",
    "afro",
];

fn genre_score(genre: &str, words: &[&str]) -> bool {
    let g = genre.to_lowercase();
    words.iter().any(|w| g.contains(w))
}

fn luminance(c: [f32; 3]) -> f32 {
    0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2]
}

/// The vibe of a track from everything rekordbox knows: tempo, key (minor darkens),
/// genre, the mood of its phrase analysis, and how bright its cover is.
pub fn vibe(track: Option<&TrackMeta>, mood: Option<Mood>, theme: &[[f32; 3]; 5]) -> Vibe {
    let Some(t) = track else {
        return Vibe {
            energy: 0.5,
            darkness: 0.5,
        };
    };
    let bpm = t.bpm.unwrap_or(120.0);
    let tempo = ((bpm - 80.0) / 70.0).clamp(0.0, 1.0);
    let mood_e = match mood {
        Some(Mood::High) => 0.9,
        Some(Mood::Mid) => 0.55,
        Some(Mood::Low) => 0.2,
        None => tempo,
    };
    let genre = t.genre.as_deref().unwrap_or("");
    let genre_e = if genre_score(genre, HIGH_ENERGY_GENRES) {
        0.85
    } else if genre_score(genre, LOW_ENERGY_GENRES) {
        0.15
    } else {
        0.5
    };
    let energy = (0.35 * tempo + 0.35 * mood_e + 0.3 * genre_e).clamp(0.0, 1.0);

    let minor = t
        .key
        .as_deref()
        .is_some_and(|k| k.trim().ends_with('A') || k.to_lowercase().contains('m'));
    let cover = theme[2..].iter().map(|c| luminance(*c)).sum::<f32>() / 3.0;
    let mut darkness = 0.3 + if minor { 0.25 } else { -0.05 };
    darkness += (0.45 - cover) * 0.6;
    if genre_score(genre, DARK_GENRES) {
        darkness += 0.2;
    }
    if genre_score(genre, BRIGHT_GENRES) {
        darkness -= 0.2;
    }
    if minor && energy < 0.4 {
        darkness += 0.15;
    }
    Vibe {
        energy,
        darkness: darkness.clamp(0.0, 1.0),
    }
}

/// When Auto mode changes scenes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AutoChange {
    /// Keep the scene that is on.
    Off,
    /// A new scene for each track.
    Tracks,
    /// Per track, plus a bigger scene at the drop and a calmer one in breakdowns.
    Drops,
    /// All of the above, plus a fresh scene every few phrases.
    #[default]
    Phrases,
}

/// How a scene change looks. The renderer maps each to a transition shader.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transition {
    Crossfade,
    Flash,
    ZoomBlur,
    Glitch,
    Wipe,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SceneChange {
    pub index: usize,
    pub transition: Transition,
    pub seconds: f32,
}

/// Bars a scene stays on before the autopilot may rotate it (drops and tracks excepted).
const MIN_BARS: u32 = 16;

#[derive(Debug, Clone)]
pub struct AutoPilot {
    rng: Rng,
    base: Option<usize>,
    drop: Option<usize>,
    calm: Option<usize>,
    current: Option<usize>,
    bars: u32,
}

impl Default for AutoPilot {
    fn default() -> Self {
        Self::new()
    }
}

impl AutoPilot {
    pub fn new() -> Self {
        Self {
            rng: Rng::new(0xA076_1D64_78BD_642F),
            base: None,
            drop: None,
            calm: None,
            current: None,
            bars: 0,
        }
    }

    pub fn current(&self) -> Option<usize> {
        self.current
    }

    /// Tells the autopilot which scene is on (after a manual change).
    pub fn set_current(&mut self, index: usize) {
        self.current = Some(index);
        self.bars = 0;
    }

    /// Scenes ranked by closeness to `target` (best first), among `allowed`.
    fn ranked(profiles: &[SceneProfile], allowed: &[bool], target: Vibe) -> Vec<usize> {
        let mut idx: Vec<usize> = (0..profiles.len())
            .filter(|i| allowed.get(*i).copied().unwrap_or(false))
            .collect();
        idx.sort_by(|a, b| {
            let d = |i: usize| {
                let p = &profiles[i];
                (p.energy - target.energy).abs() + 0.8 * (p.darkness - target.darkness).abs()
            };
            d(*a).total_cmp(&d(*b))
        });
        idx
    }

    /// One of the best few, weighted to the best, avoiding `not` when possible.
    fn pick(&mut self, ranked: &[usize], not: Option<usize>, chaos: f32) -> Option<usize> {
        let pool: Vec<usize> = ranked
            .iter()
            .copied()
            .filter(|i| Some(*i) != not)
            .take(2 + (chaos * 2.0) as usize)
            .collect();
        let pool = if pool.is_empty() {
            ranked.to_vec()
        } else {
            pool
        };
        if pool.is_empty() {
            return None;
        }
        let r = self.rng.unit().powf(1.5 - chaos.min(1.0));
        pool.get(((r * pool.len() as f32) as usize).min(pool.len() - 1))
            .copied()
    }

    fn change(
        &mut self,
        index: Option<usize>,
        transition: Transition,
        seconds: f32,
    ) -> Option<SceneChange> {
        let index = index?;
        if Some(index) == self.current {
            return None;
        }
        self.current = Some(index);
        self.bars = 0;
        Some(SceneChange {
            index,
            transition,
            seconds,
        })
    }

    /// Reacts to this frame's events. `allowed[i]` says whether scene `i` is in the rotation
    /// (enabled by the user and affordable at the current quality).
    pub fn on_events(
        &mut self,
        events: &[ShowEvent],
        vibe_now: Vibe,
        profiles: &[SceneProfile],
        allowed: &[bool],
        mode: AutoChange,
        chaos: f32,
    ) -> Option<SceneChange> {
        if mode == AutoChange::Off || profiles.is_empty() {
            return None;
        }
        for e in events {
            match *e {
                ShowEvent::Track => {
                    let ranked = Self::ranked(profiles, allowed, vibe_now);
                    self.base = self.pick(&ranked, self.current, chaos);
                    let hot = Vibe {
                        energy: (vibe_now.energy + 0.35).min(1.0),
                        ..vibe_now
                    };
                    let calm = Vibe {
                        energy: (vibe_now.energy - 0.3).max(0.0),
                        darkness: (vibe_now.darkness + 0.15).min(1.0),
                    };
                    let hot_ranked = Self::ranked(profiles, allowed, hot);
                    self.drop = self.pick(&hot_ranked, self.base, chaos);
                    let calm_ranked = Self::ranked(profiles, allowed, calm);
                    self.calm = self.pick(&calm_ranked, self.base, chaos);
                    let t = if self.rng.chance(0.5) {
                        Transition::Crossfade
                    } else {
                        Transition::Glitch
                    };
                    return self.change(self.base, t, 2.5);
                }
                ShowEvent::Drop if mode != AutoChange::Tracks => {
                    let t = if self.rng.chance(0.5) {
                        Transition::Flash
                    } else {
                        Transition::ZoomBlur
                    };
                    return self.change(self.drop.or(self.base), t, 0.6);
                }
                ShowEvent::Breakdown if mode != AutoChange::Tracks => {
                    return self.change(self.calm.or(self.base), Transition::Crossfade, 4.0);
                }
                ShowEvent::Phrase(_) if mode == AutoChange::Phrases && self.bars >= MIN_BARS => {
                    // Back to the base after a drop section, or a fresh take on the vibe.
                    let target = if self.current == self.base {
                        let ranked = Self::ranked(profiles, allowed, vibe_now);
                        self.pick(&ranked, self.current, chaos)
                    } else {
                        self.base
                    };
                    let t = if self.rng.chance(0.5) {
                        Transition::Wipe
                    } else {
                        Transition::Crossfade
                    };
                    return self.change(target, t, 2.0);
                }
                ShowEvent::Phrase(_) if mode == AutoChange::Drops && self.current != self.base => {
                    return self.change(self.base, Transition::Crossfade, 2.0);
                }
                ShowEvent::Bar => self.bars += 1,
                _ => {}
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::music_state::{CueAhead, MusicState};

    fn track(id: u64, bpm: f32, key: &str, genre: Option<&str>) -> TrackMeta {
        TrackMeta {
            id: TrackId(id),
            title: "t".into(),
            artist: "a".into(),
            album: String::new(),
            year: None,
            key: Some(key.into()),
            bpm: Some(bpm),
            duration_s: Some(200.0),
            sample_rate: None,
            file_path: None,
            artwork_path: None,
            analysis_path: None,
            isrc: None,
            genre: genre.map(Into::into),
        }
    }

    fn playing(phrase: Option<PhraseKind>, beat: u32) -> MusicState {
        MusicState {
            playing: true,
            bpm: 128.0,
            phrase,
            beat_index: Some(beat),
            track: Some(track(7, 128.0, "8B", Some("House"))),
            intensity: 1.0,
            ..MusicState::default()
        }
    }

    #[test]
    fn the_drop_flashes_and_is_scaled_by_the_setting() {
        let mut d = ShowDirector::new();
        let s = FxSettings::FESTIVAL;
        d.update(&playing(Some(PhraseKind::Up), 60), 0.016, &s);
        let events = d.update(&playing(Some(PhraseKind::Chorus), 64), 0.016, &s);
        assert!(events.contains(&ShowEvent::Drop), "{events:?}");
        assert!(d.fx().flash > 0.8);
        assert!((d.fx().drop_hit - 1.0).abs() < 0.01);

        let mut quiet = ShowDirector::new();
        let off = FxSettings {
            flashes: 0.0,
            ..FxSettings::FESTIVAL
        };
        quiet.update(&playing(Some(PhraseKind::Up), 60), 0.016, &off);
        quiet.update(&playing(Some(PhraseKind::Chorus), 64), 0.016, &off);
        assert!(quiet.fx().flash < 1e-3, "flashes off means no flash");
    }

    #[test]
    fn a_new_track_is_an_event_not_a_drop() {
        let mut d = ShowDirector::new();
        let s = FxSettings::CLUB;
        let events = d.update(&playing(Some(PhraseKind::Chorus), 10), 0.016, &s);
        assert!(events.contains(&ShowEvent::Track));
        assert!(!events.contains(&ShowEvent::Drop));
        assert!(d.fx().track_hit > 0.99);
    }

    #[test]
    fn passing_a_cue_is_detected() {
        let mut d = ShowDirector::new();
        let s = FxSettings::CLUB;
        let mut ms = playing(Some(PhraseKind::Verse), 10);
        d.update(&ms, 0.016, &s);
        ms.next_cue = Some(CueAhead {
            slot: 2,
            seconds: 0.1,
            color: Some([255, 0, 0]),
        });
        d.update(&ms, 0.016, &s);
        ms.next_cue = Some(CueAhead {
            slot: 3,
            seconds: 20.0,
            color: None,
        });
        let events = d.update(&ms, 0.016, &s);
        assert!(
            events
                .iter()
                .any(|e| matches!(e, ShowEvent::Cue { slot: 2, .. })),
            "{events:?}"
        );
        assert!(d.fx().flash_color[1] < 0.1, "the cue's red tints the flash");
    }

    #[test]
    fn strobe_never_exceeds_three_hertz() {
        let mut d = ShowDirector::new();
        let s = FxSettings {
            strobe: true,
            ..FxSettings::FESTIVAL
        };
        let mut ms = playing(Some(PhraseKind::Up), 60);
        ms.bpm = 180.0;
        d.update(&ms, 0.01, &s);
        ms.phrase = Some(PhraseKind::Chorus);
        let mut rising = 0;
        let mut last = 0.0;
        for i in 0..100 {
            ms.beat_index = Some(64 + i / 33);
            d.update(&ms, 0.01, &s);
            if d.fx().strobe > 0.5 && last < 0.5 {
                rising += 1;
            }
            last = d.fx().strobe;
        }
        assert!(rising <= 3, "{rising} flashes in one second");
        assert!(rising >= 1, "strobe runs after the drop when enabled");
    }

    #[test]
    fn strobe_is_off_unless_enabled() {
        let mut d = ShowDirector::new();
        let s = FxSettings::FESTIVAL;
        d.update(&playing(Some(PhraseKind::Up), 60), 0.016, &s);
        for i in 0..60 {
            d.update(&playing(Some(PhraseKind::Chorus), 64 + i / 30), 0.016, &s);
            assert!(d.fx().strobe < 0.5);
        }
    }

    #[test]
    fn tension_builds_before_the_drop() {
        let mut d = ShowDirector::new();
        let s = FxSettings::CLUB;
        let mut ms = playing(Some(PhraseKind::Up), 50);
        ms.drop_countdown_beats = Some(14);
        d.update(&ms, 0.016, &s);
        let early = d.fx().tension;
        ms.drop_countdown_beats = Some(2);
        d.update(&ms, 0.016, &s);
        assert!(d.fx().tension > early + 0.5);
    }

    fn profiles() -> Vec<SceneProfile> {
        [
            ("nebula", 0.15, 0.9),
            ("ribbons", 0.45, 0.4),
            ("tunnel", 0.85, 0.6),
            ("lasers", 0.95, 0.5),
            ("kaleido", 0.7, 0.2),
        ]
        .iter()
        .map(|(n, e, d)| SceneProfile {
            name: (*n).into(),
            energy: *e,
            darkness: *d,
            cost: 2,
        })
        .collect()
    }

    #[test]
    fn a_slow_sad_track_gets_a_dark_calm_vibe() {
        let theme = crate::music_state::DEFAULT_THEME;
        let sad = vibe(
            Some(&track(1, 78.0, "4A", Some("Ambient"))),
            Some(Mood::Low),
            &theme,
        );
        let party = vibe(
            Some(&track(2, 128.0, "8B", Some("House"))),
            Some(Mood::High),
            &theme,
        );
        assert!(sad.energy < 0.3, "{sad:?}");
        assert!(sad.darkness > party.darkness, "{sad:?} {party:?}");
        assert!(party.energy > 0.7, "{party:?}");
    }

    #[test]
    fn autopilot_matches_the_vibe_and_goes_big_at_the_drop() {
        let mut ap = AutoPilot::new();
        let p = profiles();
        let allowed = vec![true; p.len()];
        let calm = Vibe {
            energy: 0.1,
            darkness: 0.9,
        };
        let first = ap
            .on_events(
                &[ShowEvent::Track],
                calm,
                &p,
                &allowed,
                AutoChange::Drops,
                0.0,
            )
            .expect("a scene for the new track");
        assert!(p[first.index].energy < 0.5, "{}", p[first.index].name);
        let drop = ap
            .on_events(
                &[ShowEvent::Drop],
                calm,
                &p,
                &allowed,
                AutoChange::Drops,
                0.0,
            )
            .expect("a bigger scene at the drop");
        assert!(p[drop.index].energy > p[first.index].energy);
        let back = ap
            .on_events(
                &[ShowEvent::Phrase(Some(PhraseKind::Verse))],
                calm,
                &p,
                &allowed,
                AutoChange::Drops,
                0.0,
            )
            .expect("back to the base after the drop");
        assert_eq!(back.index, first.index);
    }

    #[test]
    fn autopilot_respects_disabled_scenes_and_off() {
        let mut ap = AutoPilot::new();
        let p = profiles();
        let mut allowed = vec![false; p.len()];
        allowed[1] = true;
        let v = Vibe {
            energy: 0.9,
            darkness: 0.5,
        };
        let c = ap
            .on_events(
                &[ShowEvent::Track],
                v,
                &p,
                &allowed,
                AutoChange::Phrases,
                0.5,
            )
            .unwrap();
        assert_eq!(c.index, 1);
        assert!(
            AutoPilot::new()
                .on_events(&[ShowEvent::Track], v, &p, &allowed, AutoChange::Off, 0.5)
                .is_none()
        );
    }
}
