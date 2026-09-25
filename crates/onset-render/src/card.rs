//! The Now Playing card: artwork, title, artists and a metadata line over a scrim, bottom
//! left. A master change crossfades the old card into the new one over [`FADE_S`].
//!
//! Layout is proportional to the output height (a 1080-line frame gets the design sizes
//! below), so the card looks the same on a projector and on a phone-sized test target.
use onset_core::track::TrackMeta;

use crate::artwork::{ArtworkLoader, placeholder};
use crate::gpu::Gpu;
use crate::quad::{QuadPipeline, QuadTexture, scrim_image};
use crate::text::{TextItem, TextLayer, linear_to_srgb8};

/// Crossfade length in seconds.
pub const FADE_S: f32 = 1.2;

/// Design sizes at 1080 lines, scaled by `height / 1080` at render time.
const DESIGN_HEIGHT: f32 = 1080.0;
const MARGIN: f32 = 56.0;
const ART_SIDE: f32 = 176.0;
const GAP: f32 = 32.0;
const TITLE_PX: f32 = 56.0;
const ARTIST_PX: f32 = 32.0;
const META_PX: f32 = 24.0;
const LINE_GAP: f32 = 6.0;
const SCRIM_HEIGHT: f32 = 400.0;

struct Slot {
    meta: TrackMeta,
    art: QuadTexture,
    /// True once the real artwork (not the placeholder) is on the GPU.
    art_ready: bool,
}

/// The crossfade between the previous and the current card. A change that lands while a
/// fade is still running continues from the opacities on screen, so nothing flashes when
/// the master bounces between decks during a blend.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FadeState {
    changed_at: Option<f32>,
    /// Opacity the new card starts from (0, or the old card's opacity on a swap back).
    new_from: f32,
    /// Opacity the old card starts from.
    old_from: f32,
}

impl Default for FadeState {
    fn default() -> Self {
        Self {
            changed_at: None,
            new_from: 1.0,
            old_from: 0.0,
        }
    }
}

impl FadeState {
    fn progress(&self, now: f32) -> f32 {
        self.changed_at
            .map_or(1.0, |start| smoothstep((now - start) / FADE_S))
    }

    /// (new card opacity, old card opacity) at `now`.
    pub fn alphas(&self, now: f32) -> (f32, f32) {
        let s = self.progress(now);
        (
            self.new_from + (1.0 - self.new_from) * s,
            self.old_from * (1.0 - s),
        )
    }

    /// True once the old card is gone and the new one is fully visible.
    pub fn settled(&self, now: f32) -> bool {
        self.progress(now) >= 1.0
    }

    /// Starts a new fade at `now`. `swap_back` means the incoming card is the one that was
    /// fading out, so it resumes from its current opacity instead of from zero.
    pub fn change(&mut self, now: f32, swap_back: bool) {
        let (new_alpha, old_alpha) = self.alphas(now);
        self.changed_at = Some(now);
        if swap_back {
            self.new_from = old_alpha;
            self.old_from = new_alpha;
        } else {
            self.new_from = 0.0;
            self.old_from = new_alpha;
        }
    }
}

/// Per-frame inputs to [`Card::draw`].
#[derive(Debug, Clone, Copy)]
pub struct CardFrame<'a> {
    /// Output size in physical pixels.
    pub size: (u32, u32),
    /// The frame's five theme colours; text uses the second (the text colour).
    pub theme: &'a [[f32; 3]; 5],
    /// Tempo being played, shown instead of the analysed BPM when known.
    pub live_bpm: f32,
    pub time_s: f32,
}

pub struct Card {
    quads: QuadPipeline,
    loader: ArtworkLoader,
    scrim: QuadTexture,
    current: Option<Slot>,
    previous: Option<Slot>,
    fade: FadeState,
}

fn smoothstep(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// "Album · 2024 · 11B · 120 BPM", skipping what is unknown.
pub fn meta_line(meta: &TrackMeta, live_bpm: f32) -> String {
    let mut parts: Vec<String> = Vec::new();
    if !meta.album.is_empty() && meta.album != meta.title {
        parts.push(meta.album.clone());
    }
    if let Some(y) = meta.year {
        parts.push(y.to_string());
    }
    if let Some(k) = &meta.key {
        parts.push(k.clone());
    }
    let bpm = if live_bpm > 0.0 {
        Some(live_bpm)
    } else {
        meta.bpm
    };
    if let Some(b) = bpm {
        parts.push(format!("{b:.0} BPM"));
    }
    parts.join("  ·  ")
}

impl Card {
    pub fn new(gpu: &Gpu, format: wgpu::TextureFormat) -> Self {
        let quads = QuadPipeline::new(gpu, format);
        let scrim = quads.upload(gpu, &scrim_image(0.85, 64));
        Self {
            quads,
            loader: ArtworkLoader::new(),
            scrim,
            current: None,
            previous: None,
            fade: FadeState::default(),
        }
    }

    /// Progress of the current crossfade, 0 at the change and 1 once settled.
    pub fn fade(&self, time_s: f32) -> f32 {
        self.fade.progress(time_s)
    }

    /// True while a card is on screen or fading out.
    pub fn is_visible(&self) -> bool {
        self.current.is_some() || self.previous.is_some()
    }

    /// Follows the master track: a new id starts a crossfade and an artwork decode.
    pub fn update(&mut self, gpu: &Gpu, track: Option<&TrackMeta>, time_s: f32) {
        let current_id = self.current.as_ref().map(|s| s.meta.id);
        let wanted_id = track.map(|t| t.id);
        if wanted_id != current_id {
            let swap_back =
                wanted_id.is_some() && self.previous.as_ref().map(|s| s.meta.id) == wanted_id;
            self.fade.change(time_s, swap_back);
            if swap_back {
                // The track that was fading out is back: keep its artwork, just swap roles.
                std::mem::swap(&mut self.current, &mut self.previous);
            } else {
                self.previous = self.current.take();
                if let Some(meta) = track {
                    let art = self.quads.upload(gpu, &placeholder());
                    if let Some(path) = &meta.artwork_path {
                        self.loader.request(path.clone());
                    }
                    self.current = Some(Slot {
                        meta: meta.clone(),
                        art,
                        art_ready: false,
                    });
                }
            }
        }

        while let Some((path, image)) = self.loader.try_take() {
            if let Some(slot) = self.current.as_mut()
                && slot.meta.artwork_path.as_deref() == Some(path.as_path())
                && !slot.art_ready
            {
                slot.art = self.quads.upload(gpu, &image);
                slot.art_ready = true;
            }
        }

        if self.fade.settled(time_s) {
            self.previous = None;
        }
    }

    /// Places and draws the quads in their own pass and returns the text to draw over them.
    pub fn draw(
        &mut self,
        gpu: &Gpu,
        encoder: &mut wgpu::CommandEncoder,
        target: &wgpu::TextureView,
        text: &mut TextLayer,
        frame: CardFrame<'_>,
    ) -> Vec<TextItem> {
        if !self.is_visible() {
            return Vec::new();
        }
        let CardFrame {
            size,
            theme,
            live_bpm,
            time_s,
        } = frame;
        let (fade_new, fade_old) = self.fade.alphas(time_s);
        let alpha_new = if self.current.is_some() {
            fade_new
        } else {
            0.0
        };
        let alpha_old = if self.previous.is_some() {
            fade_old
        } else {
            0.0
        };
        let k = size.1 as f32 / DESIGN_HEIGHT;
        let (w, h) = (size.0 as f32, size.1 as f32);

        let scrim_h = SCRIM_HEIGHT * k;
        self.scrim.place(
            gpu,
            [0.0, h - scrim_h, w, scrim_h],
            size,
            alpha_new.max(alpha_old),
        );
        let art_side = ART_SIDE * k;
        let art_rect = [MARGIN * k, h - MARGIN * k - art_side, art_side, art_side];
        let text_x = MARGIN * k + art_side + GAP * k;
        let max_w = (w - text_x - MARGIN * k).max(64.0);
        let bottom = h - MARGIN * k;
        let text_rgb = theme[1];

        let mut items = Vec::new();
        for (slot, alpha) in [
            (self.previous.as_ref(), alpha_old),
            (self.current.as_ref(), alpha_new),
        ] {
            let Some(slot) = slot else { continue };
            if alpha <= 0.0 {
                continue;
            }
            slot.art.place(gpu, art_rect, size, alpha);
            items.extend(layout_text(
                text, &slot.meta, live_bpm, text_x, bottom, max_w, k, text_rgb, alpha,
            ));
        }

        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("card"),
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
        self.quads.draw(&mut pass, &self.scrim);
        if let Some(slot) = &self.previous
            && alpha_old > 0.0
        {
            self.quads.draw(&mut pass, &slot.art);
        }
        if let Some(slot) = &self.current
            && alpha_new > 0.0
        {
            self.quads.draw(&mut pass, &slot.art);
        }
        drop(pass);
        items
    }
}

/// Title, artist and metadata stacked upward from `bottom`, left-aligned at `x`.
#[allow(clippy::too_many_arguments)]
fn layout_text(
    text: &mut TextLayer,
    meta: &TrackMeta,
    live_bpm: f32,
    x: f32,
    bottom: f32,
    max_w: f32,
    k: f32,
    rgb: [f32; 3],
    alpha: f32,
) -> Vec<TextItem> {
    let title = TextItem::new(meta.title.as_str(), TITLE_PX * k, (x, 0.0))
        .weight(700)
        .color(linear_to_srgb8(rgb, alpha))
        .max_width(max_w);
    let artist = TextItem::new(meta.artist.as_str(), ARTIST_PX * k, (x, 0.0))
        .weight(500)
        .color(linear_to_srgb8(rgb, alpha * 0.92))
        .max_width(max_w);
    let line = meta_line(meta, live_bpm);
    let info = TextItem::new(line, META_PX * k, (x, 0.0))
        .weight(400)
        .color(linear_to_srgb8(rgb, alpha * 0.75))
        .max_width(max_w);

    let mut stacked: Vec<TextItem> = Vec::with_capacity(3);
    let mut y = bottom;
    for mut item in [info, artist, title] {
        if item.text.is_empty() {
            continue;
        }
        let (_, height) = text.measure(&item);
        y -= height;
        item.pos.1 = y;
        y -= LINE_GAP * k;
        stacked.push(item);
    }
    stacked
}

#[cfg(test)]
mod tests {
    use super::*;
    use onset_core::track::TrackId;

    fn meta() -> TrackMeta {
        TrackMeta {
            id: TrackId(1),
            title: "Move".into(),
            artist: "Adam Port".into(),
            album: "Move".into(),
            year: Some(2024),
            key: Some("11B".into()),
            bpm: Some(120.0),
            duration_s: None,
            sample_rate: None,
            file_path: None,
            artwork_path: None,
            analysis_path: None,
            isrc: None,
            genre: None,
        }
    }

    #[test]
    fn meta_line_skips_album_equal_to_title_and_unknowns() {
        assert_eq!(meta_line(&meta(), 0.0), "2024  ·  11B  ·  120 BPM");
        let mut m = meta();
        m.album = "Keinemusik Anthology".into();
        m.year = None;
        m.key = None;
        assert_eq!(meta_line(&m, 126.4), "Keinemusik Anthology  ·  126 BPM");
    }

    #[test]
    fn smoothstep_is_clamped_and_monotonic() {
        assert!((smoothstep(-1.0)).abs() < f32::EPSILON);
        assert!((smoothstep(2.0) - 1.0).abs() < f32::EPSILON);
        assert!(smoothstep(0.25) < smoothstep(0.5));
        assert!((smoothstep(0.5) - 0.5).abs() < 1e-6);
    }
}

#[cfg(test)]
mod fade_tests {
    use super::*;

    fn max_step(samples: &[(f32, f32)]) -> f32 {
        samples
            .windows(2)
            .map(|w| ((w[1].0 - w[0].0).abs()).max((w[1].1 - w[0].1).abs()))
            .fold(0.0, f32::max)
    }

    #[test]
    fn a_quick_second_change_starts_from_the_current_opacities() {
        let mut f = FadeState::default();
        f.change(0.0, false);
        let before = f.alphas(0.3);
        f.change(0.3, false);
        let after = f.alphas(0.3);
        assert!((after.0).abs() < 1e-6, "new card starts invisible");
        assert!(
            (after.1 - before.0).abs() < 1e-6,
            "old card continues from where the previous new card was: {before:?} -> {after:?}"
        );
    }

    #[test]
    fn switching_back_keeps_both_cards_continuous() {
        // Track A has been on screen long enough to be settled when the bounce starts.
        let mut f = FadeState::default();
        f.change(0.0, false);
        // Samples are (opacity of A, opacity of B), whichever role each holds.
        let mut samples = Vec::new();
        let mut a_is_new = true;
        let mut t = 1.5f32;
        while t < 4.5 {
            if (t - 1.8).abs() < 1e-6 {
                f.change(t, false); // B comes in
                a_is_new = false;
            }
            if (t - 2.1).abs() < 1e-6 {
                f.change(t, true); // A comes back: roles swap, nothing jumps
                a_is_new = true;
            }
            let (new, old) = f.alphas(t);
            samples.push(if a_is_new { (new, old) } else { (old, new) });
            t = ((t + 0.1) * 10.0).round() / 10.0;
        }
        assert!(max_step(&samples) < 0.2, "opacity jumped: {samples:?}");
        let last = samples.last().unwrap();
        assert!(
            (last.0 - 1.0).abs() < 1e-3 && last.1.abs() < 1e-3,
            "settles: {last:?}"
        );
    }

    #[test]
    fn settled_state_is_fully_visible() {
        let mut f = FadeState::default();
        f.change(1.0, false);
        assert_eq!(f.alphas(1.0 + FADE_S + 0.1), (1.0, 0.0));
        assert!(f.settled(1.0 + FADE_S + 0.1));
    }
}
