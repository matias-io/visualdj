//! Smooth playhead from sparse, jittery transport reads.
//!
//! Transport sources report the playhead at 60-120 Hz with read jitter; the renderer needs a
//! value every frame that moves at the playback rate. The clock keeps an anchor and a rate,
//! extrapolates between reads, absorbs small errors gradually, and snaps on jumps.
use std::time::Instant;

use crate::transport::TransportSnapshot;

/// Prediction errors above this are treated as jumps (cue press, loop, seek) and snapped.
pub const SNAP_THRESHOLD_S: f64 = 0.080;
/// Fraction of a small prediction error corrected per observation.
pub const SMOOTHING: f64 = 0.15;

#[derive(Debug, Default)]
pub struct Clock {
    /// Playhead (seconds) known to be correct at the paired instant.
    anchor: Option<(f64, Instant)>,
    /// Playback rate: `bpm_now / bpm_original`.
    rate: f64,
    playing: bool,
}

impl Clock {
    pub fn new() -> Self {
        Self {
            anchor: None,
            rate: 1.0,
            playing: false,
        }
    }

    pub fn rate(&self) -> f64 {
        self.rate
    }

    pub fn is_playing(&self) -> bool {
        self.playing
    }

    /// Playhead in seconds at `now`, extrapolated from the last observation while playing.
    pub fn playhead_at(&self, now: Instant) -> Option<f64> {
        let (p, t) = self.anchor?;
        if !self.playing {
            return Some(p);
        }
        Some(p + self.rate * now.saturating_duration_since(t).as_secs_f64())
    }

    pub fn observe(&mut self, s: &TransportSnapshot) {
        let predicted = self.playhead_at(s.read_at);
        let was_playing = self.playing;

        self.rate = if s.bpm_original > 0.0 && s.bpm_now > 0.0 {
            f64::from(s.bpm_now) / f64::from(s.bpm_original)
        } else {
            1.0
        };
        self.playing = s.playing;

        match predicted {
            Some(pred)
                if was_playing == s.playing && (s.playhead_s - pred).abs() <= SNAP_THRESHOLD_S =>
            {
                // Small disagreement: read jitter. Nudge the anchor instead of jumping.
                let corrected = pred + SMOOTHING * (s.playhead_s - pred);
                self.anchor = Some((corrected, s.read_at));
            }
            _ => self.anchor = Some((s.playhead_s, s.read_at)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::{TrackRef, TransportSnapshot};
    use std::time::{Duration, Instant};

    fn snap(
        t0: Instant,
        dt_ms: u64,
        playhead_s: f64,
        bpm: f32,
        playing: bool,
    ) -> TransportSnapshot {
        TransportSnapshot {
            deck: 0,
            track: TrackRef::Unknown,
            playhead_s,
            bpm_now: bpm,
            bpm_original: 120.0,
            playing,
            read_at: t0 + Duration::from_millis(dt_ms),
        }
    }

    #[test]
    fn nothing_before_first_observation() {
        assert_eq!(Clock::new().playhead_at(Instant::now()), None);
    }

    #[test]
    fn extrapolates_between_reads() {
        let t0 = Instant::now();
        let mut c = Clock::new();
        c.observe(&snap(t0, 0, 10.0, 120.0, true));
        let p = c.playhead_at(t0 + Duration::from_millis(500)).unwrap();
        assert!((p - 10.5).abs() < 1e-6, "{p}");
    }

    #[test]
    fn follows_rate_changes() {
        let t0 = Instant::now();
        let mut c = Clock::new();
        c.observe(&snap(t0, 0, 10.0, 126.0, true)); // +5 %
        let p = c.playhead_at(t0 + Duration::from_millis(1000)).unwrap();
        assert!((p - 11.05).abs() < 1e-6, "{p}");
        assert!((c.rate() - 1.05).abs() < 1e-9);
    }

    /// Pitch fader moved mid-track: the playhead must stay continuous and the new rate must
    /// drive extrapolation from that point on.
    #[test]
    fn rate_change_mid_stream_is_continuous() {
        let t0 = Instant::now();
        let mut c = Clock::new();
        c.observe(&snap(t0, 0, 10.0, 120.0, true));
        // One second later the read agrees with the prediction and reports +5 %.
        c.observe(&snap(t0, 1000, 11.0, 126.0, true));
        let at_change = c.playhead_at(t0 + Duration::from_millis(1000)).unwrap();
        assert!((at_change - 11.0).abs() < 1e-6, "{at_change}");
        let later = c.playhead_at(t0 + Duration::from_millis(2000)).unwrap();
        assert!((later - 12.05).abs() < 1e-6, "{later}");
    }

    #[test]
    fn snaps_on_discontinuity() {
        let t0 = Instant::now();
        let mut c = Clock::new();
        c.observe(&snap(t0, 0, 10.0, 120.0, true));
        c.observe(&snap(t0, 100, 40.0, 120.0, true)); // hot cue jump
        let p = c.playhead_at(t0 + Duration::from_millis(100)).unwrap();
        assert!((p - 40.0).abs() < 1e-3, "{p}");
    }

    #[test]
    fn smooths_small_jitter() {
        let t0 = Instant::now();
        let mut c = Clock::new();
        c.observe(&snap(t0, 0, 10.0, 120.0, true));
        // Prediction at +100 ms is 10.10; the read says 10.12 (20 ms early).
        c.observe(&snap(t0, 100, 10.12, 120.0, true));
        let p = c.playhead_at(t0 + Duration::from_millis(100)).unwrap();
        assert!(p > 10.10 && p < 10.12, "{p}");
    }

    #[test]
    fn freezes_when_paused() {
        let t0 = Instant::now();
        let mut c = Clock::new();
        c.observe(&snap(t0, 0, 10.0, 120.0, false));
        assert!(!c.is_playing());
        let p = c.playhead_at(t0 + Duration::from_secs(3)).unwrap();
        assert!((p - 10.0).abs() < 1e-9);
    }

    #[test]
    fn resuming_from_pause_snaps_to_the_read() {
        let t0 = Instant::now();
        let mut c = Clock::new();
        c.observe(&snap(t0, 0, 10.0, 120.0, false));
        c.observe(&snap(t0, 2000, 10.0, 120.0, true));
        let p = c.playhead_at(t0 + Duration::from_millis(2500)).unwrap();
        assert!((p - 10.5).abs() < 1e-6, "{p}");
    }
}
