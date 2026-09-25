//! rekordbox's own per-track analysis of where the energy and the vocals are: the 3-band
//! waveform (`PWV7`: low, mid and high heights, 150 frames a second) and the vocal detection
//! (`PVDI`: a 0-4 vocal level per frame). Read at the playhead, it tells the show what the
//! loaded track is doing right now with no audio latency and no room noise.

#[derive(Debug, Clone, PartialEq, Default)]
pub struct TrackBands {
    /// Frames per second of `frames`.
    pub rate_hz: f32,
    /// Low, mid and high heights per frame, as rekordbox stores them.
    pub frames: Vec<[u8; 3]>,
    /// Frames per second of `vocal`.
    pub vocal_rate_hz: f32,
    /// rekordbox's vocal level per frame, 0 (none) to 4 (lead vocal).
    pub vocal: Vec<u8>,
    /// The loudest height of each band over the track, for scaling to 0..1.
    peak: [u8; 3],
}

/// What the analysis says at one moment, scaled to 0..1.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct BandsAt {
    pub low: f32,
    pub mid: f32,
    pub high: f32,
    pub vocal: f32,
}

impl TrackBands {
    pub fn new(rate_hz: f32, frames: Vec<[u8; 3]>, vocal_rate_hz: f32, vocal: Vec<u8>) -> Self {
        let mut peak = [1u8; 3];
        for f in &frames {
            for (p, v) in peak.iter_mut().zip(f) {
                *p = (*p).max(*v);
            }
        }
        Self {
            rate_hz,
            frames,
            vocal_rate_hz,
            vocal,
            peak,
        }
    }

    /// The bands and vocal level at `t_s` seconds into the track, linearly interpolated
    /// between frames; zero outside the analysed range.
    pub fn at(&self, t_s: f64) -> BandsAt {
        let mut out = BandsAt::default();
        if let Some([l, m, h]) = sample3(&self.frames, self.rate_hz, t_s) {
            out.low = l / f32::from(self.peak[0]);
            out.mid = m / f32::from(self.peak[1]);
            out.high = h / f32::from(self.peak[2]);
        }
        if let Some(v) = sample1(&self.vocal, self.vocal_rate_hz, t_s) {
            out.vocal = (v / 4.0).clamp(0.0, 1.0);
        }
        out
    }
}

fn position(len: usize, rate_hz: f32, t_s: f64) -> Option<(usize, usize, f32)> {
    if len == 0 || rate_hz <= 0.0 || !t_s.is_finite() || t_s < 0.0 {
        return None;
    }
    let x = t_s * f64::from(rate_hz);
    let i = x.floor() as usize;
    if i >= len {
        return None;
    }
    Some((i, (i + 1).min(len - 1), (x - x.floor()) as f32))
}

fn sample3(frames: &[[u8; 3]], rate_hz: f32, t_s: f64) -> Option<[f32; 3]> {
    let (i, j, k) = position(frames.len(), rate_hz, t_s)?;
    let lerp = |c: usize| f32::from(frames[i][c]) * (1.0 - k) + f32::from(frames[j][c]) * k;
    Some([lerp(0), lerp(1), lerp(2)])
}

fn sample1(values: &[u8], rate_hz: f32, t_s: f64) -> Option<f32> {
    let (i, j, k) = position(values.len(), rate_hz, t_s)?;
    Some(f32::from(values[i]) * (1.0 - k) + f32::from(values[j]) * k)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bands() -> TrackBands {
        // 2 frames a second: a kick frame, then a hat frame, then silence.
        TrackBands::new(2.0, vec![[200, 10, 0], [0, 10, 100], [0, 0, 0]], 1.0, vec![0, 4])
    }

    #[test]
    fn reads_the_frame_at_the_playhead_scaled_by_the_track_peak() {
        let a = bands().at(0.0);
        assert!((a.low - 1.0).abs() < 1e-6 && a.high.abs() < 1e-6);
        let b = bands().at(0.5);
        assert!(b.low.abs() < 1e-6 && (b.high - 1.0).abs() < 1e-6);
    }

    #[test]
    fn interpolates_between_frames() {
        let a = bands().at(0.25);
        assert!((a.low - 0.5).abs() < 1e-6, "{a:?}");
        let v = bands().at(0.5).vocal;
        assert!((v - 0.5).abs() < 1e-6, "{v}");
    }

    #[test]
    fn is_silent_outside_the_analysis() {
        assert_eq!(bands().at(-1.0), BandsAt::default());
        assert_eq!(bands().at(99.0), BandsAt::default());
        assert_eq!(TrackBands::default().at(1.0), BandsAt::default());
    }

    #[test]
    fn vocal_level_four_is_full_vocal() {
        assert!((bands().at(1.0).vocal - 1.0).abs() < 1e-6);
    }
}
