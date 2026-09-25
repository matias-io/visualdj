//! The telemetry overlay: frame timing, the active scene, transport and structure state,
//! and the last shader error. Top left, small, toggled by `H`.
use std::collections::VecDeque;

use onset_core::music_state::MusicState;
use onset_core::phrase::PhraseKind;

use crate::overlay_options::HudOptions;
use crate::text::TextItem;

/// Frames kept for the percentile.
pub const WINDOW: usize = 120;

const LINE_PX: f32 = 18.0;
const MARGIN: f32 = 16.0;

/// What the HUD shows besides the `MusicState`.
#[derive(Debug, Clone, Default)]
pub struct HudInfo {
    pub scene: String,
    pub size: (u32, u32),
    pub gpu_ms: Option<f32>,
    pub last_error: Option<String>,
    /// The engine's one-line status ("waiting for rekordbox", "rekordbox: Artist - Title").
    pub source: String,
}

pub struct Hud {
    frame_ms: VecDeque<f32>,
    cpu_ms: f32,
}

impl Default for Hud {
    fn default() -> Self {
        Self::new()
    }
}

/// Short label for the phrase readout.
pub fn phrase_name(kind: Option<PhraseKind>) -> &'static str {
    match kind {
        None => "-",
        Some(PhraseKind::Intro) => "Intro",
        Some(PhraseKind::Verse) => "Verse",
        Some(PhraseKind::Bridge) => "Bridge",
        Some(PhraseKind::Chorus) => "Chorus",
        Some(PhraseKind::Outro) => "Outro",
        Some(PhraseKind::Up) => "Up",
        Some(PhraseKind::Down) => "Down",
    }
}

fn mmss(seconds: f64) -> String {
    let s = seconds.max(0.0);
    let m = (s / 60.0).floor();
    format!("{m:02.0}:{:04.1}", s - m * 60.0)
}

impl Hud {
    pub fn new() -> Self {
        Self {
            frame_ms: VecDeque::with_capacity(WINDOW + 1),
            cpu_ms: 0.0,
        }
    }

    /// Records one frame: `interval_ms` since the previous frame (`None` for the first),
    /// `cpu_ms` the previous frame's CPU cost.
    pub fn record(&mut self, interval_ms: Option<f32>, cpu_ms: f32) {
        if let Some(dt) = interval_ms
            && dt > 0.0
        {
            self.frame_ms.push_back(dt);
            if self.frame_ms.len() > WINDOW {
                self.frame_ms.pop_front();
            }
        }
        self.cpu_ms = cpu_ms;
    }

    pub fn last_frame_ms(&self) -> f32 {
        self.frame_ms.back().copied().unwrap_or(0.0)
    }

    /// 99th percentile of frame time over the window; 0 until frames exist.
    pub fn p99_ms(&self) -> f32 {
        if self.frame_ms.is_empty() {
            return 0.0;
        }
        let mut sorted: Vec<f32> = self.frame_ms.iter().copied().collect();
        sorted.sort_by(f32::total_cmp);
        let idx = ((sorted.len() as f32 * 0.99).ceil() as usize).clamp(1, sorted.len()) - 1;
        sorted[idx]
    }

    /// The lines to draw, in physical pixels; `scale` is the window's DPI factor. `o` picks
    /// which pieces appear; a line with nothing left in it is dropped.
    #[allow(clippy::too_many_lines)] // one readout, piece by piece
    pub fn items(
        &self,
        ms: &MusicState,
        info: &HudInfo,
        scale: f32,
        o: &HudOptions,
    ) -> Vec<TextItem> {
        let scale = scale * o.size.clamp(0.5, 2.5);
        let px = LINE_PX * scale;
        let gpu = info
            .gpu_ms
            .map_or_else(String::new, |g| format!("  ·  gpu {g:.2} ms"));
        let transport = if ms.track.is_none() {
            "idle".to_string()
        } else if ms.playing {
            "playing".to_string()
        } else {
            "paused".to_string()
        };
        let next = match (ms.next_phrase, ms.beats_to_next_phrase) {
            (Some(kind), Some(beats)) => format!("{} in {beats}", phrase_name(Some(kind))),
            _ => "-".to_string(),
        };
        let drop = ms
            .drop_countdown_beats
            .map_or_else(|| "-".to_string(), |b| format!("{b} beats"));

        let join = |parts: Vec<String>| {
            parts
                .into_iter()
                .filter(|p| !p.is_empty())
                .collect::<Vec<_>>()
                .join("  ·  ")
        };
        let on = |cond: bool, s: String| if cond { s } else { String::new() };
        let key = ms
            .track
            .as_ref()
            .and_then(|t| t.key.clone())
            .unwrap_or_else(|| "-".to_string());
        let cue = ms.next_cue.map_or_else(
            || "next cue -".to_string(),
            |c| {
                let name = if c.slot == 0 {
                    "memory cue".to_string()
                } else {
                    format!("cue {}", char::from(b'A' + (c.slot - 1).min(7)))
                };
                format!("{name} in {:.1} s", c.seconds)
            },
        );
        let a = &ms.analysis;
        let mut lines = vec![
            on(
                o.scene,
                format!("Onset  ·  {}  ·  {}x{}", info.scene, info.size.0, info.size.1),
            ),
            on(
                o.performance,
                format!(
                    "frame {:.1} ms  ·  p99 {:.1} ms  ·  cpu {:.2} ms{gpu}",
                    self.last_frame_ms(),
                    self.p99_ms(),
                    self.cpu_ms
                ),
            ),
            join(vec![
                on(
                    o.transport,
                    if info.source.is_empty() {
                        "-".to_string()
                    } else {
                        info.source.clone()
                    },
                ),
                on(o.transport, transport),
                on(o.tempo, format!("{:.1} BPM", ms.bpm)),
                on(o.key, key),
                on(o.transport, mmss(ms.playhead_s)),
            ]),
            join(vec![
                on(o.phrase, format!("phrase {}", phrase_name(ms.phrase))),
                on(o.phrase, format!("next {next}")),
                on(o.drop, format!("drop {drop}")),
                on(o.phrase, format!("intensity {:.2}", ms.intensity)),
            ]),
            on(o.next_cue, cue),
            on(
                o.analysis,
                format!(
                    "low {:.2}  ·  mid {:.2}  ·  high {:.2}  ·  vocal {:.2}  ·  kick {:.2}  ·  hats {:.2}",
                    a.low, a.mid, a.high, a.vocal, ms.audio.kick, ms.audio.hat
                ),
            ),
        ];
        lines.retain(|l| !l.is_empty());
        if let Some(err) = &info.last_error {
            lines.push(err.lines().next().unwrap_or(err).to_string());
        }

        let mut items = Vec::with_capacity(lines.len() * 2);
        for (i, line) in lines.iter().enumerate() {
            let y = MARGIN * scale + i as f32 * px * 1.35;
            let is_error = info.last_error.is_some() && i == lines.len() - 1;
            let color = if is_error {
                [255, 96, 96, 240]
            } else {
                [255, 255, 255, 230]
            };
            // A dark copy one pixel down and right keeps the text readable on bright scenes.
            items.push(
                TextItem::new(line.as_str(), px, (MARGIN * scale + scale, y + scale))
                    .weight(500)
                    .color([0, 0, 0, 200]),
            );
            items.push(
                TextItem::new(line.as_str(), px, (MARGIN * scale, y))
                    .weight(500)
                    .color(color),
            );
        }
        items
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn p99_tracks_the_slow_frames() {
        let mut hud = Hud::new();
        hud.record(None, 0.0);
        for i in 0..200 {
            hud.record(Some(if i % 50 == 25 { 40.0 } else { 8.0 }), 1.0);
        }
        assert!(hud.p99_ms() > 30.0, "p99 {}", hud.p99_ms());
        assert!(hud.last_frame_ms() < 10.0);
    }

    #[test]
    fn window_is_bounded() {
        let mut hud = Hud::new();
        for _ in 0..500 {
            hud.record(Some(16.0), 0.5);
        }
        assert_eq!(hud.frame_ms.len(), WINDOW);
    }

    #[test]
    fn switched_off_pieces_leave_the_readout() {
        let hud = Hud::new();
        let ms = MusicState::default();
        let all = hud.items(&ms, &HudInfo::default(), 1.0, &HudOptions::default());
        let few = HudOptions {
            performance: false,
            scene: false,
            ..HudOptions::default()
        };
        let less = hud.items(&ms, &HudInfo::default(), 1.0, &few);
        assert_eq!(less.len() + 4, all.len(), "two lines (each with a shadow) gone");
        assert!(less.iter().all(|i| !i.text.contains("frame")));
        let tempo_only = HudOptions {
            scene: false,
            performance: false,
            transport: false,
            phrase: false,
            drop: false,
            ..HudOptions::default()
        };
        let t = hud.items(&ms, &HudInfo::default(), 1.0, &tempo_only);
        assert_eq!(t.len(), 2);
        assert!(t[1].text.contains("BPM"), "{}", t[1].text);
    }

    #[test]
    fn items_include_the_error_line_when_present() {
        let hud = Hud::new();
        let ms = MusicState::default();
        let plain = hud.items(&ms, &HudInfo::default(), 1.0, &HudOptions::default());
        let info = HudInfo {
            last_error: Some("shader `ring` failed to compile: x\nmore".into()),
            ..HudInfo::default()
        };
        let with = hud.items(&ms, &info, 1.0, &HudOptions::default());
        assert_eq!(with.len(), plain.len() + 2);
        assert!(with.last().unwrap().text.starts_with("shader `ring`"));
        assert!(!with.last().unwrap().text.contains("more"));
    }

    #[test]
    fn mmss_formats_tenths() {
        assert_eq!(mmss(75.25), "01:15.2");
        assert_eq!(mmss(-3.0), "00:00.0");
    }
}
