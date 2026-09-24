//! Text rendering with glyphon (cosmic-text shaping, a glyph atlas on the GPU). Positions and
//! sizes are physical pixels: callers that lay out in logical units multiply by the window's
//! scale factor themselves, so text is always rasterised at the display's real resolution.
use glyphon::{
    Attrs, Buffer, Cache, Color, Family, FontSystem, Metrics, Resolution, Shaping, SwashCache,
    TextArea, TextAtlas, TextBounds, TextRenderer, Viewport, Weight, fontdb,
};

use crate::gpu::Gpu;

/// The bundled typeface (SIL Open Font License 1.1, see `assets/fonts/LICENSE`).
pub const FONT_FAMILY: &str = "Inter";

const FONTS: [&[u8]; 4] = [
    include_bytes!("../../../assets/fonts/Inter-Regular.ttf"),
    include_bytes!("../../../assets/fonts/Inter-Medium.ttf"),
    include_bytes!("../../../assets/fonts/Inter-SemiBold.ttf"),
    include_bytes!("../../../assets/fonts/Inter-Bold.ttf"),
];

/// Line height as a multiple of the font size.
pub const LINE_HEIGHT: f32 = 1.2;

/// One run of text to draw. `pos` is the top-left corner in physical pixels.
#[derive(Debug, Clone, PartialEq)]
pub struct TextItem {
    pub text: String,
    /// Font size in physical pixels.
    pub px: f32,
    pub pos: (f32, f32),
    /// sRGB with alpha.
    pub color: [u8; 4],
    /// CSS weight: 400 regular, 500 medium, 600 semibold, 700 bold.
    pub weight: u16,
    /// Wrap width in physical pixels; unbounded when `None`.
    pub max_width: Option<f32>,
}

impl TextItem {
    pub fn new(text: impl Into<String>, px: f32, pos: (f32, f32)) -> Self {
        Self {
            text: text.into(),
            px,
            pos,
            color: [255, 255, 255, 255],
            weight: 400,
            max_width: None,
        }
    }

    #[must_use]
    pub fn color(mut self, color: [u8; 4]) -> Self {
        self.color = color;
        self
    }

    #[must_use]
    pub fn weight(mut self, weight: u16) -> Self {
        self.weight = weight;
        self
    }

    #[must_use]
    pub fn max_width(mut self, width: f32) -> Self {
        self.max_width = Some(width);
        self
    }
}

/// What a shaped buffer depends on; equal keys mean the buffer can be reused as is.
#[derive(Debug, Clone, PartialEq)]
struct LayoutKey {
    text: String,
    px: u32,
    weight: u16,
    max_width: Option<u32>,
}

impl LayoutKey {
    fn of(item: &TextItem) -> Self {
        Self {
            text: item.text.clone(),
            px: item.px.to_bits(),
            weight: item.weight,
            max_width: item.max_width.map(f32::to_bits),
        }
    }
}

pub struct TextLayer {
    font_system: FontSystem,
    swash: SwashCache,
    atlas: TextAtlas,
    renderer: TextRenderer,
    viewport: Viewport,
    buffers: Vec<(LayoutKey, Buffer)>,
}

impl TextLayer {
    /// Builds the font system from the bundled fonts only: no system font scan, so start-up
    /// is fast and layout is identical on every machine.
    pub fn new(gpu: &Gpu, format: wgpu::TextureFormat) -> Self {
        let mut db = fontdb::Database::new();
        for bytes in FONTS {
            db.load_font_data(bytes.to_vec());
        }
        db.set_sans_serif_family(FONT_FAMILY);
        let font_system = FontSystem::new_with_locale_and_db("en-US".to_string(), db);

        let cache = Cache::new(&gpu.device);
        let mut atlas = TextAtlas::new(&gpu.device, &gpu.queue, &cache, format);
        let renderer = TextRenderer::new(
            &mut atlas,
            &gpu.device,
            wgpu::MultisampleState::default(),
            None,
        );
        let viewport = Viewport::new(&gpu.device, &cache);
        Self {
            font_system,
            swash: SwashCache::new(),
            atlas,
            renderer,
            viewport,
            buffers: Vec::new(),
        }
    }

    fn shape(&mut self, item: &TextItem) -> Buffer {
        let metrics = Metrics::new(item.px, item.px * LINE_HEIGHT);
        let mut buffer = Buffer::new(&mut self.font_system, metrics);
        buffer.set_size(item.max_width, None);
        let attrs = Attrs::new()
            .family(Family::Name(FONT_FAMILY))
            .weight(Weight(item.weight));
        buffer.set_text(&item.text, &attrs, Shaping::Advanced, None);
        buffer.shape_until_scroll(&mut self.font_system, false);
        buffer
    }

    /// Reuses an existing buffer for an unchanged item, shapes a new one otherwise.
    fn take_or_shape(&mut self, item: &TextItem) -> (LayoutKey, Buffer) {
        let key = LayoutKey::of(item);
        if let Some(i) = self.buffers.iter().position(|(k, _)| *k == key) {
            return self.buffers.swap_remove(i);
        }
        let buffer = self.shape(item);
        (key, buffer)
    }

    /// Laid-out size in physical pixels: the widest line and the total line height.
    pub fn measure(&mut self, item: &TextItem) -> (f32, f32) {
        let (key, buffer) = self.take_or_shape(item);
        let size = measure_buffer(&buffer);
        self.buffers.push((key, buffer));
        size
    }

    /// Shapes and uploads every item; call once per frame, then [`Self::render`] inside a pass.
    pub fn prepare(
        &mut self,
        gpu: &Gpu,
        size_px: (u32, u32),
        items: &[TextItem],
    ) -> Result<(), glyphon::PrepareError> {
        self.viewport.update(
            &gpu.queue,
            Resolution {
                width: size_px.0,
                height: size_px.1,
            },
        );
        let mut next: Vec<(LayoutKey, Buffer)> = Vec::with_capacity(items.len());
        for item in items {
            next.push(self.take_or_shape(item));
        }
        self.buffers = next;

        let areas = items
            .iter()
            .zip(self.buffers.iter())
            .map(|(item, (_, buffer))| TextArea {
                buffer,
                left: item.pos.0,
                top: item.pos.1,
                scale: 1.0,
                bounds: TextBounds::default(),
                default_color: Color::rgba(
                    item.color[0],
                    item.color[1],
                    item.color[2],
                    item.color[3],
                ),
                custom_glyphs: &[],
            });
        self.renderer.prepare(
            &gpu.device,
            &gpu.queue,
            &mut self.font_system,
            &mut self.atlas,
            &self.viewport,
            areas,
            &mut self.swash,
        )
    }

    /// Draws what the last [`Self::prepare`] produced.
    pub fn render(&self, pass: &mut wgpu::RenderPass<'_>) {
        if let Err(e) = self.renderer.render(&self.atlas, &self.viewport, pass) {
            tracing::warn!("text render: {e}");
        }
    }

    /// Frees atlas space for glyphs not used since the last call; call once per frame.
    pub fn trim(&mut self) {
        self.atlas.trim();
    }
}

fn measure_buffer(buffer: &Buffer) -> (f32, f32) {
    let mut width = 0.0f32;
    let mut height = 0.0f32;
    for run in buffer.layout_runs() {
        width = width.max(run.line_w);
        height += run.line_height;
    }
    (width, height)
}

/// Linear RGB (0..1) to sRGB bytes with the given alpha, for text and quad colours.
pub fn linear_to_srgb8(rgb: [f32; 3], alpha: f32) -> [u8; 4] {
    fn channel(c: f32) -> u8 {
        let c = c.clamp(0.0, 1.0);
        let s = if c <= 0.003_130_8 {
            12.92 * c
        } else {
            1.055 * c.powf(1.0 / 2.4) - 0.055
        };
        (s * 255.0).round() as u8
    }
    [
        channel(rgb[0]),
        channel(rgb[1]),
        channel(rgb[2]),
        (alpha.clamp(0.0, 1.0) * 255.0).round() as u8,
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn srgb_conversion_hits_the_known_points() {
        assert_eq!(linear_to_srgb8([0.0, 0.0, 0.0], 1.0), [0, 0, 0, 255]);
        assert_eq!(linear_to_srgb8([1.0, 1.0, 1.0], 0.5), [255, 255, 255, 128]);
        // 0.5 linear is 188 in sRGB.
        assert_eq!(linear_to_srgb8([0.5, 0.5, 0.5], 1.0)[0], 188);
    }

    #[test]
    fn layout_key_ignores_position_and_colour() {
        let a = TextItem::new("Move", 56.0, (0.0, 0.0)).color([1, 2, 3, 4]);
        let b = TextItem::new("Move", 56.0, (100.0, 50.0));
        assert_eq!(LayoutKey::of(&a), LayoutKey::of(&b));
        let c = TextItem::new("Move", 56.0, (0.0, 0.0)).weight(700);
        assert_ne!(LayoutKey::of(&a), LayoutKey::of(&c));
    }
}
