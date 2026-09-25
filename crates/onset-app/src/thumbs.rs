//! Small pictures of every scene for the launcher, rendered from the synthetic gallery
//! (`crates/onset-render/tests/gallery.rs`) and built into the binary.
use std::collections::HashMap;

/// JPEG bytes by scene name.
const THUMBS: &[(&str, &[u8])] = &[
    ("cover", include_bytes!("../../../assets/thumbs/cover.jpg")),
    ("crystal", include_bytes!("../../../assets/thumbs/crystal.jpg")),
    ("kaleido", include_bytes!("../../../assets/thumbs/kaleido.jpg")),
    ("lasers", include_bytes!("../../../assets/thumbs/lasers.jpg")),
    ("liquid", include_bytes!("../../../assets/thumbs/liquid.jpg")),
    ("nebula", include_bytes!("../../../assets/thumbs/nebula.jpg")),
    ("pulse", include_bytes!("../../../assets/thumbs/pulse.jpg")),
    ("ribbons", include_bytes!("../../../assets/thumbs/ribbons.jpg")),
    ("ring", include_bytes!("../../../assets/thumbs/ring.jpg")),
    ("synthwave", include_bytes!("../../../assets/thumbs/synthwave.jpg")),
    ("tunnel", include_bytes!("../../../assets/thumbs/tunnel.jpg")),
    ("voronoi", include_bytes!("../../../assets/thumbs/voronoi.jpg")),
    ("warp", include_bytes!("../../../assets/thumbs/warp.jpg")),
    ("zerog", include_bytes!("../../../assets/thumbs/zerog.jpg")),
];

/// The scene thumbnails as egui textures, decoded once and kept in the context.
pub fn textures(ctx: &egui::Context) -> HashMap<String, egui::TextureHandle> {
    let id = egui::Id::new("onset-scene-thumbs");
    if let Some(t) = ctx.data(|d| d.get_temp::<HashMap<String, egui::TextureHandle>>(id)) {
        return t;
    }
    let mut out = HashMap::new();
    for (name, bytes) in THUMBS {
        let Ok(img) = image::load_from_memory(bytes) else {
            continue;
        };
        let rgba = img.to_rgba8();
        let size = [rgba.width() as usize, rgba.height() as usize];
        let color = egui::ColorImage::from_rgba_unmultiplied(size, rgba.as_raw());
        let tex = ctx.load_texture(format!("thumb-{name}"), color, egui::TextureOptions::LINEAR);
        out.insert((*name).to_string(), tex);
    }
    ctx.data_mut(|d| d.insert_temp(id, out.clone()));
    out
}
