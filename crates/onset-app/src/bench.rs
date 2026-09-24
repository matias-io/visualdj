//! Frame-time statistics for `--bench`: the numbers the performance gate reads.

/// A frame longer than this counts as dropped (a missed 50 Hz deadline; 60 Hz would be 16.7).
pub const DROPPED_MS: f32 = 20.0;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BenchStats {
    pub frames: usize,
    pub mean_ms: f32,
    pub p99_ms: f32,
    pub dropped: usize,
}

impl BenchStats {
    /// Statistics over frame intervals in milliseconds; empty input gives zeros.
    pub fn from_intervals(intervals: &[f32]) -> Self {
        if intervals.is_empty() {
            return Self {
                frames: 0,
                mean_ms: 0.0,
                p99_ms: 0.0,
                dropped: 0,
            };
        }
        let mut sorted = intervals.to_vec();
        sorted.sort_by(f32::total_cmp);
        let idx = ((sorted.len() as f32 * 0.99).ceil() as usize).clamp(1, sorted.len()) - 1;
        Self {
            frames: intervals.len(),
            mean_ms: intervals.iter().sum::<f32>() / intervals.len() as f32,
            p99_ms: sorted[idx],
            dropped: intervals.iter().filter(|ms| **ms > DROPPED_MS).count(),
        }
    }

    /// One machine-readable line; `scene` names what was measured.
    pub fn line(&self, scene: &str, size: (u32, u32)) -> String {
        format!(
            "bench scene={scene} size={}x{} frames={} mean_ms={:.2} p99_ms={:.2} dropped={}",
            size.0, size.1, self.frames, self.mean_ms, self.p99_ms, self.dropped
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stats_count_slow_frames_and_the_tail() {
        // Two slow frames in a hundred sit at ranks 99 and 100, so the nearest-rank p99
        // lands on the first of them; one outlier in a hundred would only show at p100.
        let mut v = vec![8.0f32; 98];
        v.extend([25.0, 30.0]);
        let s = BenchStats::from_intervals(&v);
        assert_eq!(s.frames, 100);
        assert_eq!(s.dropped, 2);
        assert!((s.p99_ms - 25.0).abs() < f32::EPSILON, "{}", s.p99_ms);
        assert!((s.mean_ms - 8.39).abs() < 0.01, "{}", s.mean_ms);
    }

    #[test]
    fn empty_input_is_zeros_not_a_panic() {
        let s = BenchStats::from_intervals(&[]);
        assert_eq!(s.frames, 0);
        assert_eq!(s.dropped, 0);
    }

    #[test]
    fn line_has_every_key() {
        let l = BenchStats::from_intervals(&[10.0]).line("ring", (1920, 1080));
        for key in [
            "scene=ring",
            "size=1920x1080",
            "frames=1",
            "mean_ms=",
            "p99_ms=",
            "dropped=0",
        ] {
            assert!(l.contains(key), "{key} in {l}");
        }
    }
}
