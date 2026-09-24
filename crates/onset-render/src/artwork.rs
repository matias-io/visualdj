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
    fn placeholder_is_a_gradient_not_a_flat_fill() {
        let p = placeholder();
        let a = p.get_pixel(0, 0);
        let b = p.get_pixel(PLACEHOLDER_SIDE - 1, PLACEHOLDER_SIDE - 1);
        assert_ne!(a, b);
        assert_eq!(a[3], 255);
    }
}
