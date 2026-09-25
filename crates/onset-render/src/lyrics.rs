//! Synced lyrics over the show. The line being sung is laid out word by word so each word
//! can take its own colour and motion: Karaoke lights words as they are sung, and at drops
//! and build-ups the words split into colour, sweep through a rainbow and ride a gentle wave.
//! Nothing blinks: opacity only ever eases in and out, so it stays safe for photosensitive
//! viewers.
use std::sync::Arc;

use onset_core::lyrics::Lyrics;
use onset_core::music_state::MusicState;
use onset_core::show::Fx;
use onset_core::track::TrackId;

use crate::overlay_options::{LyricPlace, LyricStyle, LyricsOptions};
use crate::text::{TextItem, TextLayer, linear_to_srgb8};

/// Design size of the sung line at 1080 lines.
const LINE_PX: f32 = 64.0;
/// How long a new line takes to slide in, in seconds.
const APPEAR_S: f32 = 0.28;

/// Per-frame inputs to [`LyricsLayer::items`].
pub struct LyricsFrame<'a> {
    pub size: (u32, u32),
    pub ms: &'a MusicState,
    pub fx: &'a Fx,
    pub theme: &'a [[f32; 3]; 5],
    pub options: &'a LyricsOptions,
    pub time_s: f32,
}

#[derive(Default)]
pub struct LyricsLayer {
    track: Option<TrackId>,
    lyrics: Option<Arc<Lyrics>>,
    line: Option<usize>,
    previous: Option<usize>,
    changed_at: f32,
}

fn smooth(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// HSV (all 0..1) to linear RGB, for the rainbow sweep.
fn hsv(hue: f32, sat: f32, val: f32) -> [f32; 3] {
    let sector = hue.rem_euclid(1.0) * 6.0;
    let chroma = val * sat;
    let second = chroma * (1.0 - ((sector % 2.0) - 1.0).abs());
    let (red, green, blue) = match sector as u32 {
        0 => (chroma, second, 0.0),
        1 => (second, chroma, 0.0),
        2 => (0.0, chroma, second),
        3 => (0.0, second, chroma),
        4 => (second, 0.0, chroma),
        _ => (chroma, 0.0, second),
    };
    let floor = val - chroma;
    [red + floor, green + floor, blue + floor]
}

fn mix(a: [f32; 3], b: [f32; 3], t: f32) -> [f32; 3] {
    [
        a[0] + (b[0] - a[0]) * t,
        a[1] + (b[1] - a[1]) * t,
        a[2] + (b[2] - a[2]) * t,
    ]
}

/// The track's accent, lifted so it reads on any scene.
fn accent(theme: &[[f32; 3]; 5]) -> [f32; 3] {
    let c = theme[2];
    let m = c[0].max(c[1]).max(c[2]).max(0.05);
    mix([c[0] / m, c[1] / m, c[2] / m], [1.0; 3], 0.25)
}

/// A word placed on screen.
struct Placed {
    text: String,
    x: f32,
    y: f32,
    /// Word index within the line, for Karaoke and the rainbow.
    index: usize,
}

impl LyricsLayer {
    pub fn new() -> Self {
        Self::default()
    }

    /// The lyrics for `track`, or `None` when it has none. Replaces what was shown.
    pub fn set(&mut self, track: TrackId, lyrics: Option<Arc<Lyrics>>) {
        self.track = Some(track);
        self.lyrics = lyrics;
        self.line = None;
        self.previous = None;
    }

    /// Lays out one line's words centred at `y`, wrapping to a second row if too wide.
    fn layout(text: &mut TextLayer, words: &[String], px: f32, width: f32, y: f32) -> Vec<Placed> {
        let space = px * 0.3;
        let max_w = width * 0.84;
        let sizes: Vec<(f32, f32)> = words
            .iter()
            .map(|w| text.measure(&TextItem::new(w.as_str(), px, (0.0, 0.0)).weight(700)))
            .collect();
        // Split into rows that fit.
        let mut rows: Vec<Vec<usize>> = vec![Vec::new()];
        let mut row_w = 0.0;
        for (i, (w, _)) in sizes.iter().enumerate() {
            let add = if rows.last().is_some_and(Vec::is_empty) { *w } else { space + w };
            if row_w + add > max_w && !rows.last().is_some_and(Vec::is_empty) {
                rows.push(Vec::new());
                row_w = 0.0;
            }
            let add = if rows.last().is_some_and(Vec::is_empty) { *w } else { space + w };
            rows.last_mut().expect("a row").push(i);
            row_w += add;
        }
        let line_h = sizes.iter().map(|s| s.1).fold(px, f32::max) * 1.08;
        let top = y - line_h * rows.len() as f32 * 0.5;
        let mut out = Vec::with_capacity(words.len());
        for (r, row) in rows.iter().enumerate() {
            let total: f32 = row.iter().map(|&i| sizes[i].0).sum::<f32>()
                + space * row.len().saturating_sub(1) as f32;
            let mut x = (width - total) * 0.5;
            for &i in row {
                out.push(Placed {
                    text: words[i].clone(),
                    x,
                    y: top + r as f32 * line_h,
                    index: i,
                });
                x += sizes[i].0 + space;
            }
        }
        out
    }

    /// The text to draw this frame; empty when there is nothing to sing.
    #[allow(clippy::too_many_lines)] // one frame of the lyrics, read top to bottom
    pub fn items(&mut self, text: &mut TextLayer, frame: &LyricsFrame<'_>) -> Vec<TextItem> {
        let opts = frame.options;
        let Some(lyrics) = self.lyrics.clone() else {
            return Vec::new();
        };
        let same_track = frame.ms.track.as_ref().map(|t| t.id) == self.track;
        if !opts.enabled || !same_track || lyrics.lines.is_empty() {
            return Vec::new();
        }
        let playhead = frame.ms.playhead_s as f32 + opts.offset_s;
        let at = lyrics.at(playhead);
        let now_line = at.map(|a| a.line);
        if now_line != self.line {
            self.previous = self.line;
            self.line = now_line;
            self.changed_at = frame.time_s;
        }
        let since = frame.time_s - self.changed_at;
        let appear = smooth(since / APPEAR_S);

        let (width, height) = (frame.size.0 as f32, frame.size.1 as f32);
        let px = LINE_PX * height / 1080.0 * opts.size.clamp(0.5, 2.5);
        let centre_y = height * match opts.place {
            LyricPlace::Top => 0.2,
            LyricPlace::Centre => 0.5,
            LyricPlace::Lower => 0.72,
        };
        let accent = accent(frame.theme);
        // How much the lyrics move: the drop and, more gently, the build-up towards it.
        let amount = opts.drop_fx.clamp(0.0, 1.0) * frame.fx.drop_hit.max(frame.fx.build * 0.6);
        let kick = frame.ms.audio.kick.clamp(0.0, 1.0);
        let mut items = Vec::new();

        // The previous line drifts up and fades as the new one arrives.
        if let Some(prev) = self.previous.filter(|_| appear < 1.0)
            && let Some(line) = lyrics.lines.get(prev)
        {
            let words: Vec<String> = line.words.iter().map(|w| w.text.clone()).collect();
            let fade = 1.0 - appear;
            for word in Self::layout(text, &words, px, width, centre_y - px * 0.8 * appear) {
                items.push(
                    TextItem::new(word.text, px, (word.x, word.y))
                        .weight(700)
                        .color(linear_to_srgb8([1.0; 3], 0.55 * fade)),
                );
            }
        }

        let (Some(sung_at), Some(line)) = (at, now_line.and_then(|n| lyrics.lines.get(n))) else {
            return items;
        };
        let words: Vec<String> = line.words.iter().map(|w| w.text.clone()).collect();
        let lift = (1.0 - appear) * px * 0.5;
        for word in Self::layout(text, &words, px, width, centre_y + lift) {
            let index = word.index;
            // Style colour.
            let sung = match opts.style {
                LyricStyle::Clean => 1.0,
                LyricStyle::Glow => {
                    if index <= sung_at.word {
                        1.0
                    } else {
                        0.0
                    }
                }
                LyricStyle::Karaoke => match index.cmp(&sung_at.word) {
                    std::cmp::Ordering::Less => 1.0,
                    std::cmp::Ordering::Equal => sung_at.word_progress,
                    std::cmp::Ordering::Greater => 0.0,
                },
            };
            let (base, lit_col) = match opts.style {
                LyricStyle::Clean => ([1.0; 3], [1.0; 3]),
                LyricStyle::Glow => (mix([1.0; 3], accent, 0.35), accent),
                LyricStyle::Karaoke => ([1.0; 3], accent),
            };
            let mut col = mix(base, lit_col, sung);
            let mut alpha = appear * if opts.style == LyricStyle::Karaoke { 0.6 + 0.4 * sung } else { 1.0 };
            // Drop motion: a rainbow sweep across the words, a wave, a lift on the kick.
            let hue = (index as f32 * 0.09 + frame.time_s * 0.2).fract();
            col = mix(col, hsv(hue, 0.75, 1.0), amount * 0.8);
            let wave = (frame.time_s * 2.4 + index as f32 * 0.7).sin() * px * 0.12 * amount;
            let bounce = kick * amount * px * 0.1;
            let (wx, wy) = (word.x, word.y + wave - bounce);
            alpha = alpha.clamp(0.0, 1.0);

            // Shadow for legibility over bright scenes.
            items.push(
                TextItem::new(word.text.as_str(), px, (wx + px * 0.035, wy + px * 0.045))
                    .weight(700)
                    .color([0, 0, 0, (150.0 * alpha) as u8]),
            );
            // Glow: soft copies in the accent around the lit words.
            if opts.style == LyricStyle::Glow && sung > 0.0 {
                for (dx, dy) in [(-1.0f32, 0.0f32), (1.0, 0.0), (0.0, -1.0), (0.0, 1.0)] {
                    items.push(
                        TextItem::new(word.text.as_str(), px, (wx + dx * px * 0.03, wy + dy * px * 0.03))
                            .weight(700)
                            .color(linear_to_srgb8(accent, 0.22 * alpha)),
                    );
                }
            }
            // Colour split at drops: red one way, cyan the other.
            if amount > 0.05 {
                let split = px * 0.06 * amount;
                items.push(
                    TextItem::new(word.text.as_str(), px, (wx + split, wy))
                        .weight(700)
                        .color(linear_to_srgb8([1.0, 0.1, 0.2], 0.45 * amount * alpha)),
                );
                items.push(
                    TextItem::new(word.text.as_str(), px, (wx - split, wy))
                        .weight(700)
                        .color(linear_to_srgb8([0.1, 0.8, 1.0], 0.45 * amount * alpha)),
                );
            }
            items.push(
                TextItem::new(word.text, px, (wx, wy))
                    .weight(700)
                    .color(linear_to_srgb8(col, alpha)),
            );
        }

        // The next line waits below, small and dim (Glow and Karaoke).
        if opts.style != LyricStyle::Clean
            && let Some(next) = lyrics.lines.get(sung_at.line + 1)
        {
            let small = px * 0.55;
            let words: Vec<String> = next.words.iter().map(|w| w.text.clone()).collect();
            let rows = Self::layout(text, &words, small, width, 0.0);
            let below = centre_y + px * 1.25 + if rows.iter().any(|p| p.y > rows[0].y) { small * 0.6 } else { 0.0 };
            for word in Self::layout(text, &words, small, width, below) {
                items.push(
                    TextItem::new(word.text, small, (word.x, word.y))
                        .weight(500)
                        .color(linear_to_srgb8([1.0; 3], 0.4 * appear)),
                );
            }
        }
        items
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hsv_hits_the_primaries() {
        let close = |a: [f32; 3], b: [f32; 3]| a.iter().zip(b).all(|(x, y)| (x - y).abs() < 1e-4);
        assert!(close(hsv(0.0, 1.0, 1.0), [1.0, 0.0, 0.0]));
        assert!(close(hsv(1.0 / 3.0, 1.0, 1.0), [0.0, 1.0, 0.0]));
        assert!(close(hsv(2.0 / 3.0, 1.0, 1.0), [0.0, 0.0, 1.0]));
    }

    #[test]
    fn the_accent_is_never_dark() {
        let mut theme = [[0.0; 3]; 5];
        theme[2] = [0.02, 0.0, 0.1];
        let a = accent(&theme);
        assert!(a[2] > 0.9 && a[0] > 0.2, "{a:?}");
    }
}
