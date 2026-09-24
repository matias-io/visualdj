//! Beat grid: the beat times rekordbox analysed for a track, and phase queries on them.

/// One analysed beat.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Beat {
    pub time_ms: u32,
    /// Position inside the bar, 1..=4.
    pub beat_in_bar: u8,
    /// Tempo at this beat as rekordbox stored it.
    pub bpm: f32,
}

/// The full beat grid of a track, sorted by time.
#[derive(Debug, Clone, PartialEq, Default, serde::Serialize, serde::Deserialize)]
pub struct BeatGrid {
    beats: Vec<Beat>,
}

impl BeatGrid {
    pub fn new(mut beats: Vec<Beat>) -> Self {
        beats.sort_by_key(|b| b.time_ms);
        Self { beats }
    }

    pub fn beats(&self) -> &[Beat] {
        &self.beats
    }

    pub fn len(&self) -> usize {
        self.beats.len()
    }

    pub fn is_empty(&self) -> bool {
        self.beats.is_empty()
    }

    /// Index of the last beat at or before `t_ms`; `None` before the first beat.
    pub fn beat_at(&self, t_ms: f64) -> Option<usize> {
        let first = self.beats.first()?;
        if t_ms < f64::from(first.time_ms) {
            return None;
        }
        let i = self.beats.partition_point(|b| f64::from(b.time_ms) <= t_ms);
        Some(i - 1)
    }

    /// Duration of beat `i`: the gap to the next beat, or one period of its tempo for the last.
    fn period_ms(&self, i: usize) -> f64 {
        match self.beats.get(i + 1) {
            Some(next) => f64::from(next.time_ms.saturating_sub(self.beats[i].time_ms)).max(1.0),
            None => 60_000.0 / f64::from(self.beats[i].bpm.max(1.0)),
        }
    }

    /// Beat index and 0..1 progress towards the next beat.
    pub fn beat_phase(&self, t_ms: f64) -> Option<(usize, f32)> {
        let i = self.beat_at(t_ms)?;
        let ph = (t_ms - f64::from(self.beats[i].time_ms)) / self.period_ms(i);
        Some((i, ph.clamp(0.0, 0.999_999) as f32))
    }

    /// 0..1 progress through the current 4-beat bar.
    pub fn bar_phase(&self, t_ms: f64) -> Option<f32> {
        let (i, ph) = self.beat_phase(t_ms)?;
        let pos = f32::from(self.beats[i].beat_in_bar.clamp(1, 4) - 1);
        Some((pos + ph) / 4.0)
    }

    pub fn time_of(&self, beat_index: usize) -> Option<f64> {
        self.beats.get(beat_index).map(|b| f64::from(b.time_ms))
    }

    pub fn bpm_at(&self, t_ms: f64) -> Option<f32> {
        self.beat_at(t_ms).map(|i| self.beats[i].bpm)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 120 BPM => 500 ms per beat, 16 beats starting at 1000 ms.
    fn grid_120() -> BeatGrid {
        BeatGrid::new(
            (0..16u32)
                .map(|i| Beat {
                    time_ms: 1000 + i * 500,
                    beat_in_bar: u8::try_from(i % 4 + 1).unwrap(),
                    bpm: 120.0,
                })
                .collect(),
        )
    }

    #[test]
    fn beat_at_before_first_is_none() {
        assert_eq!(grid_120().beat_at(999.0), None);
    }

    #[test]
    fn beat_phase_midway() {
        let (i, ph) = grid_120().beat_phase(1250.0).unwrap();
        assert_eq!(i, 0);
        assert!((ph - 0.5).abs() < 1e-6);
    }

    #[test]
    fn bar_phase_third_beat() {
        // beat index 2 (third beat of bar 1) at its start => half way through the bar
        assert!((grid_120().bar_phase(2000.0).unwrap() - 0.5).abs() < 1e-6);
    }

    #[test]
    fn phase_after_last_beat_extrapolates_with_last_bpm() {
        let (i, ph) = grid_120()
            .beat_phase(1000.0 + 15.0 * 500.0 + 250.0)
            .unwrap();
        assert_eq!(i, 15);
        assert!((ph - 0.5).abs() < 1e-6);
    }

    #[test]
    fn bpm_at_reports_grid_tempo() {
        assert_eq!(grid_120().bpm_at(5000.0), Some(120.0));
        assert_eq!(grid_120().bpm_at(0.0), None);
    }
}
