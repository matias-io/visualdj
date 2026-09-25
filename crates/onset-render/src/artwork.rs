//! Album artwork decoding on a worker thread. The render thread only ever asks and polls, so
//! a slow or broken image never costs a frame.
use std::path::{Path, PathBuf};

use image::RgbaImage;

/// Artwork is downsized to fit this square before upload; larger is wasted on a card.
pub const MAX_SIDE: u32 = 1024;

const PLACEHOLDER_SIDE: u32 = 256;

pub struct ArtworkLoader {
    requests: crossbeam_channel::Sender<PathBuf>,
    results: crossbeam_channel::Receiver<(PathBuf, RgbaImage)>,
}

impl Default for ArtworkLoader {
    fn default() -> Self {
        Self::new()
    }
}

impl ArtworkLoader {
    /// Spawns the decoder thread. It exits when the loader is dropped.
    pub fn new() -> Self {
        let (req_tx, req_rx) = crossbeam_channel::unbounded::<PathBuf>();
        let (res_tx, res_rx) = crossbeam_channel::unbounded::<(PathBuf, RgbaImage)>();
        let spawned = std::thread::Builder::new()
            .name("onset-artwork".into())
            .spawn(move || {
                while let Ok(path) = req_rx.recv() {
                    let image = load(&path);
                    if res_tx.send((path, image)).is_err() {
                        break;
                    }
                }
            });
        if let Err(e) = spawned {
            tracing::error!("artwork thread failed to start: {e}");
        }
        Self {
            requests: req_tx,
            results: res_rx,
        }
    }

    /// Queues a decode; returns at once.
    pub fn request(&self, path: PathBuf) {
        if self.requests.send(path).is_err() {
            tracing::error!("artwork thread is gone");
        }
    }

    /// The next finished decode, if any. Never blocks.
    pub fn try_take(&self) -> Option<(PathBuf, RgbaImage)> {
        self.results.try_recv().ok()
    }
}

/// Decodes and downsizes `path`; a missing or corrupt file yields the placeholder.
pub fn load(path: &Path) -> RgbaImage {
    match decode(path) {
        Ok(img) => img,
        Err(e) => {
            tracing::warn!(path = %path.display(), "artwork unavailable: {e}");
            placeholder()
        }
    }
}

fn decode(path: &Path) -> Result<RgbaImage, image::ImageError> {
    let img = image::ImageReader::open(path)?
        .with_guessed_format()?
        .decode()?;
    let img = if img.width() > MAX_SIDE || img.height() > MAX_SIDE {
        img.thumbnail(MAX_SIDE, MAX_SIDE)
    } else {
        img
    };
    Ok(img.to_rgba8())
}

/// A neutral diagonal gradient shown while artwork loads or when a track has none.
pub fn placeholder() -> RgbaImage {
    let n = PLACEHOLDER_SIDE;
    RgbaImage::from_fn(n, n, |x, y| {
        let t = (x + y) as f32 / (2 * (n - 1)) as f32;
        let v = 40.0 + 50.0 * t;
        let b = 55.0 + 60.0 * t;
        image::Rgba([v as u8, v as u8, b as u8, 255])
    })
}

// ------------------------------------------------------------------------------------------
// Palette
// ------------------------------------------------------------------------------------------

fn srgb_to_linear(c: u8) -> f32 {
    let x = f32::from(c) / 255.0;
    if x <= 0.04045 {
        x / 12.92
    } else {
        ((x + 0.055) / 1.055).powf(2.4)
    }
}

/// Hue (0..1), saturation and value of a linear RGB colour.
fn to_hsv(c: [f32; 3]) -> (f32, f32, f32) {
    let max = c[0].max(c[1]).max(c[2]);
    let min = c[0].min(c[1]).min(c[2]);
    let d = max - min;
    let s = if max > 0.0 { d / max } else { 0.0 };
    let h = if d <= 1e-6 {
        0.0
    } else if (max - c[0]).abs() < f32::EPSILON {
        ((c[1] - c[2]) / d).rem_euclid(6.0) / 6.0
    } else if (max - c[1]).abs() < f32::EPSILON {
        ((c[2] - c[0]) / d + 2.0) / 6.0
    } else {
        ((c[0] - c[1]) / d + 4.0) / 6.0
    };
    (h, s, max)
}

fn from_hsv(hue: f32, sat: f32, val: f32) -> [f32; 3] {
    let sector = hue.rem_euclid(1.0) * 6.0;
    let chroma = val * sat;
    let second = chroma * (1.0 - (sector % 2.0 - 1.0).abs());
    let base = val - chroma;
    let rgb = match sector as u32 {
        0 => [chroma, second, 0.0],
        1 => [second, chroma, 0.0],
        2 => [0.0, chroma, second],
        3 => [0.0, second, chroma],
        4 => [second, 0.0, chroma],
        _ => [chroma, 0.0, second],
    };
    [rgb[0] + base, rgb[1] + base, rgb[2] + base]
}

const HUE_BINS: usize = 24;

/// Five theme colours from a cover (linear RGB): background, text and three accents. The
/// accents are the cover's strongest distinct hues, pushed bright and saturated enough to
/// read as light on a dark stage; a colourless cover keeps `fallback`'s accents, tinted.
pub fn palette(img: &RgbaImage, fallback: &[[f32; 3]; 5]) -> [[f32; 3]; 5] {
    let small = image::imageops::resize(img, 48, 48, image::imageops::FilterType::Triangle);
    let mut weight = [0.0f32; HUE_BINS];
    let mut sum = [[0.0f32; 3]; HUE_BINS];
    let mut lums: Vec<(f32, [f32; 3])> = Vec::with_capacity(48 * 48);
    for p in small.pixels() {
        let rgb = [
            srgb_to_linear(p[0]),
            srgb_to_linear(p[1]),
            srgb_to_linear(p[2]),
        ];
        let (hue, sat, val) = to_hsv(rgb);
        lums.push((0.2126 * rgb[0] + 0.7152 * rgb[1] + 0.0722 * rgb[2], rgb));
        if sat > 0.25 && val > 0.08 {
            let bin = ((hue * HUE_BINS as f32) as usize).min(HUE_BINS - 1);
            let strength = sat * sat * val.sqrt();
            weight[bin] += strength;
            for (acc, v) in sum[bin].iter_mut().zip(rgb) {
                *acc += v * strength;
            }
        }
    }
    // Strongest bins at least 45 degrees apart.
    let total: f32 = weight.iter().sum();
    let mut order: Vec<usize> = (0..HUE_BINS).collect();
    order.sort_by(|a, b| weight[*b].total_cmp(&weight[*a]));
    let mut picked: Vec<usize> = Vec::new();
    for b in order {
        if weight[b] <= 1e-4 || weight[b] < total * 0.04 || picked.len() == 3 {
            break;
        }
        let far = picked.iter().all(|p| {
            let d = b.abs_diff(*p);
            d.min(HUE_BINS - d) >= 3
        });
        if far {
            picked.push(b);
        }
    }
    let vivid = |c: [f32; 3]| {
        let (h, s, v) = to_hsv(c);
        from_hsv(h, s.max(0.6), v.clamp(0.55, 1.0))
    };
    let mut accents: Vec<[f32; 3]> = picked
        .iter()
        .map(|b| {
            let w = weight[*b].max(1e-6);
            vivid([sum[*b][0] / w, sum[*b][1] / w, sum[*b][2] / w])
        })
        .collect();
    // Fill missing accents with neighbours of the first hue (or the fallback's).
    let base_hue = accents
        .first()
        .map_or_else(|| to_hsv(fallback[2]).0, |c| to_hsv(*c).0);
    let mut k = 1.0;
    while accents.len() < 3 {
        if picked.is_empty() {
            accents.push(fallback[2 + accents.len()]);
        } else {
            accents.push(from_hsv(base_hue + 0.11 * k, 0.75, 0.85));
            k = -k * 1.8;
        }
    }
    lums.sort_by(|a, b| a.0.total_cmp(&b.0));
    let dark = &lums[..(lums.len() / 5).max(1)];
    let mut bg = [0.0f32; 3];
    for (_, c) in dark {
        for i in 0..3 {
            bg[i] += c[i] / dark.len() as f32;
        }
    }
    let brightest = bg[0].max(bg[1]).max(bg[2]).max(1e-4);
    let scale = (0.05 / brightest).min(1.0);
    let bg = [bg[0] * scale, bg[1] * scale, bg[2] * scale];
    let a = accents[0];
    let text = [0.9 + 0.1 * a[0], 0.9 + 0.1 * a[1], 0.9 + 0.1 * a[2]];
    [bg, text, accents[0], accents[1], accents[2]]
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    fn wait_for(loader: &ArtworkLoader, limit: Duration) -> Option<(PathBuf, RgbaImage)> {
        let start = Instant::now();
        while start.elapsed() < limit {
            if let Some(r) = loader.try_take() {
                return Some(r);
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        None
    }

    fn any_fixture_artwork() -> Option<PathBuf> {
        let root = std::env::var_os("ONSET_FIXTURES_DIR").map_or_else(
            || PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/private"),
            PathBuf::from,
        );
        let root = root.join("share/PIONEER/Artwork");
        let mut stack = vec![root];
        while let Some(dir) = stack.pop() {
            for entry in std::fs::read_dir(dir).ok()?.flatten() {
                let p = entry.path();
                if p.is_dir() {
                    stack.push(p);
                } else if p.file_name().is_some_and(|n| n == "artwork.jpg") {
                    return Some(p);
                }
            }
        }
        None
    }

    #[test]
    fn missing_artwork_uses_placeholder() {
        let loader = ArtworkLoader::new();
        let path = PathBuf::from(r"Z:\definitely\not\here\artwork.jpg");
        loader.request(path.clone());
        let (got_path, img) = wait_for(&loader, Duration::from_millis(500)).expect("a result");
        assert_eq!(got_path, path);
        assert_eq!(img, placeholder());
    }

    #[test]
    fn artwork_decode_off_thread() {
        let Some(path) = any_fixture_artwork() else {
            eprintln!("skipping: no fixture artwork");
            return;
        };
        let loader = ArtworkLoader::new();
        let t = Instant::now();
        loader.request(path.clone());
        let asked_in = t.elapsed();
        assert!(
            asked_in < Duration::from_millis(1),
            "request blocked {asked_in:?}"
        );
        let (_, img) = wait_for(&loader, Duration::from_secs(5)).expect("decoded");
        assert!(img.width() > 0 && img.width() <= MAX_SIDE);
        assert!(img.height() > 0 && img.height() <= MAX_SIDE);
        assert_ne!(img, placeholder(), "a real file must not fall back");
    }

    #[test]
    fn palette_finds_the_covers_hues() {
        // Left half red, right half blue.
        let img = RgbaImage::from_fn(64, 64, |x, _| {
            if x < 32 {
                image::Rgba([220, 20, 30, 255])
            } else {
                image::Rgba([20, 40, 230, 255])
            }
        });
        let p = palette(&img, &onset_core::music_state::DEFAULT_THEME);
        let reddish = p[2..].iter().any(|c| c[0] > 0.4 && c[2] < 0.2);
        let bluish = p[2..].iter().any(|c| c[2] > 0.4 && c[0] < 0.2);
        assert!(reddish && bluish, "{p:?}");
        assert!(
            p[0].iter().all(|v| *v < 0.2),
            "background stays dark: {:?}",
            p[0]
        );
    }

    #[test]
    fn a_grey_cover_keeps_the_default_accents() {
        let img = RgbaImage::from_pixel(32, 32, image::Rgba([128, 128, 128, 255]));
        let fallback = onset_core::music_state::DEFAULT_THEME;
        let p = palette(&img, &fallback);
        assert!(
            p[2].iter()
                .zip(fallback[2])
                .all(|(a, b)| (a - b).abs() < 1e-6)
        );
    }

    #[test]
    fn placeholder_is_a_gradient_not_a_flat_fill() {
        let p = placeholder();
        let a = p.get_pixel(0, 0);
        let b = p.get_pixel(PLACEHOLDER_SIDE - 1, PLACEHOLDER_SIDE - 1);
        assert_ne!(a, b);
        assert_eq!(a[3], 255);
    }
}
