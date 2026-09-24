//! The telemetry overlay: frame timing, the active scene, transport and structure state,
//! and the last shader error. Top left, small, toggled by `H`.
use std::collections::VecDeque;

use onset_core::music_state::MusicState;
use onset_core::phrase::PhraseKind;

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
}

pub struct Hud {
    frame_ms: VecDeque<f32>,
    last_time: Option<f32>,
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
            last_time: None,
            cpu_ms: 0.0,
        }
    }

    /// Records one frame: `time_s` is when it started, `cpu_ms` the previous frame's CPU cost.
    pub fn record(&mut self, time_s: f32, cpu_ms: f32) {
        if let Some(last) = self.last_time {
            let dt = (time_s - last) * 1000.0;
            if dt > 0.0 {
                self.frame_ms.push_back(dt);
                if self.frame_ms.len() > WINDOW {
                    self.frame_ms.pop_front();
                }
            }
        }
        self.last_time = Some(time_s);
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

    /// The lines to draw, in physical pixels; `scale` is the window's DPI factor.
    pub fn items(&self, ms: &MusicState, info: &HudInfo, scale: f32) -> Vec<TextItem> {
        let px = LINE_PX * scale;
        let gpu = info
            .gpu_ms
            .map_or_else(|| "n/a".to_string(), |g| format!("{g:.2} ms"));
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

        let mut lines = vec![
            format!(
                "Onset  ·  {}  ·  {}x{}",
                info.scene, info.size.0, info.size.1
            ),
            format!(
                "frame {:.1} ms  ·  p99 {:.1} ms  ·  cpu {:.2} ms  ·  gpu {gpu}",
                self.last_frame_ms(),
                self.p99_ms(),
                self.cpu_ms
            ),
            format!(
                "{transport}  ·  {:.1} BPM  ·  {}",
                ms.bpm,
                mmss(ms.playhead_s)
            ),
            format!(
                "phrase {}  ·  next {next}  ·  drop {drop}  ·  intensity {:.2}",
                phrase_name(ms.phrase),
                ms.intensity
            ),
        ];
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
        let mut t = 0.0f32;
        hud.record(t, 0.0);
        for i in 0..200 {
            t += if i % 50 == 25 { 0.040 } else { 0.008 };
            hud.record(t, 1.0);
        }
        assert!(hud.p99_ms() > 30.0, "p99 {}", hud.p99_ms());
        assert!(hud.last_frame_ms() < 10.0);
    }

    #[test]
    fn window_is_bounded() {
        let mut hud = Hud::new();
        for i in 0..500 {
            hud.record(i as f32 * 0.016, 0.5);
        }
        assert_eq!(hud.frame_ms.len(), WINDOW);
    }

    #[test]
    fn items_include_the_error_line_when_present() {
        let hud = Hud::new();
        let ms = MusicState::default();
        let plain = hud.items(&ms, &HudInfo::default(), 1.0);
        let info = HudInfo {
            last_error: Some("shader `ring` failed to compile: x\nmore".into()),
            ..HudInfo::default()
        };
        let with = hud.items(&ms, &info, 1.0);
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
