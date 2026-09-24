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
    /// When the last change started, in the renderer's seconds.
    fade_start: Option<f32>,
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
            fade_start: None,
        }
    }

    /// Progress of the current crossfade, 0 at the change and 1 once settled.
    pub fn fade(&self, time_s: f32) -> f32 {
        self.fade_start
            .map_or(1.0, |start| smoothstep((time_s - start) / FADE_S))
    }

    /// True while a card is on screen or fading out.
    pub fn is_visible(&self) -> bool {
        self.current.is_some() || self.previous.is_some()
    }

    /// Follows the master track: a new id starts a crossfade and an artwork decode.
    pub fn update(&mut self, gpu: &Gpu, track: Option<&TrackMeta>, time_s: f32) {
        let current_id = self.current.as_ref().map(|s| s.meta.id);
        if track.map(|t| t.id) != current_id {
            self.previous = self.current.take();
            self.fade_start = Some(time_s);
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

        while let Some((path, image)) = self.loader.try_take() {
            if let Some(slot) = self.current.as_mut()
                && slot.meta.artwork_path.as_deref() == Some(path.as_path())
                && !slot.art_ready
            {
                slot.art = self.quads.upload(gpu, &image);
                slot.art_ready = true;
            }
        }

        if self.fade(time_s) >= 1.0 {
            self.previous = None;
            if self.current.is_none() {
                self.fade_start = None;
            }
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
        let t = self.fade(time_s);
        let alpha_new = if self.current.is_some() { t } else { 0.0 };
        let alpha_old = if self.previous.is_some() {
            1.0 - t
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
            file_path: None,
            artwork_path: None,
            analysis_path: None,
            isrc: None,
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
