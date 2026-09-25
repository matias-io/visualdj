//! Turns raw band magnitudes into what visuals react to: every band normalised to 0..1 by
//! its own recent peak (so a quiet room and a loud club look the same), six frequency
//! groups, and separate kick, snare and hi-hat hits detected from spectral flux inside
//! their frequency ranges. Runs once per analysis hop.
use onset_core::audio_features::{BANDS, GROUPS};

/// Band index ranges (inclusive start, exclusive end) for the six groups: sub, bass,
/// low-mid, mid, high-mid, treble. With 24 log bands from 30 Hz to 16 kHz each band spans
/// about 0.38 octave: sub 30-66 Hz, bass 66-144, low-mid 144-410, mid 410-1170,
/// high-mid 1.2-3.3 kHz, treble 3.3-16 kHz.
pub const GROUP_RANGES: [(usize, usize); GROUPS] =
    [(0, 3), (3, 6), (6, 10), (10, 14), (14, 18), (18, 24)];

/// A drum voice: which bands carry it, how long it rings visually, and how soon it may
/// fire again.
struct Voice {
    bands: (usize, usize),
    decay_s: f32,
    refractory_s: f32,
}

/// Kick 40-145 Hz, snare body and crack 530 Hz-3.3 kHz, hats 5.6-16 kHz.
const VOICES: [Voice; 3] = [
    Voice {
        bands: (1, 6),
        decay_s: 0.14,
        refractory_s: 0.09,
    },
    Voice {
        bands: (11, 18),
        decay_s: 0.11,
        refractory_s: 0.07,
    },
    Voice {
        bands: (20, 24),
        decay_s: 0.06,
        refractory_s: 0.04,
    },
];

/// Seconds for a band's remembered peak to fall to 1/e: long enough that a breakdown
/// reads as quieter than the drop, short enough to adapt within a mix.
const PEAK_DECAY_S: f32 = 6.0;
/// A band's peak never drops below this share of the loudest band's, so an empty band
/// shows its noise as noise instead of stretching it to full scale.
const RELATIVE_FLOOR: f32 = 0.2;
/// Absolute floor for peaks, below any real signal.
const ABSOLUTE_FLOOR: f32 = 1e-3;
/// Flux must exceed this multiple of its running average to count as a hit.
const HIT_RATIO: f32 = 1.8;
/// Time constant of the running flux average per voice.
const FLUX_AVERAGE_S: f32 = 0.5;
/// Loudness: RMS smoothed over this, relative to a peak decaying over `LOUDNESS_PEAK_S`.
const LOUDNESS_SMOOTH_S: f32 = 0.4;
const LOUDNESS_PEAK_S: f32 = 20.0;

/// What the reactor adds to a hop's features.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Reaction {
    pub levels: [f32; BANDS],
    pub groups: [f32; GROUPS],
    pub kick: f32,
    pub snare: f32,
    pub hat: f32,
    pub loudness: f32,
    pub brightness: f32,
}

impl Reaction {
    pub const SILENT: Self = Self {
        levels: [0.0; BANDS],
        groups: [0.0; GROUPS],
        kick: 0.0,
        snare: 0.0,
        hat: 0.0,
        loudness: 0.0,
        brightness: 0.0,
    };
}

pub struct Reactor {
    hop_s: f32,
    peaks: [f32; BANDS],
    prev_raw: [f32; BANDS],
    flux_avg: [f32; 3],
    hits: [f32; 3],
    since_hit: [f32; 3],
    rms_smooth: f32,
    rms_peak: f32,
}

fn decay_factor(dt: f32, tau: f32) -> f32 {
    (-dt / tau).exp()
}

impl Reactor {
    /// `hop_s` is the time between calls (hop size / sample rate).
    pub fn new(hop_s: f32) -> Self {
        Self {
            hop_s,
            peaks: [ABSOLUTE_FLOOR; BANDS],
            prev_raw: [0.0; BANDS],
            flux_avg: [0.0; 3],
            hits: [0.0; 3],
            since_hit: [f32::MAX; 3],
            rms_smooth: 0.0,
            rms_peak: ABSOLUTE_FLOOR,
        }
    }

    /// Forgets everything (after silence, so the next track starts from scratch).
    pub fn reset(&mut self) {
        *self = Self::new(self.hop_s);
    }

    /// `env` is the envelope-followed band values, `raw` this hop's unsmoothed magnitudes.
    pub fn update(&mut self, env: &[f32; BANDS], raw: &[f32; BANDS], rms: f32) -> Reaction {
        let dt = self.hop_s;

        // Per-band auto-gain.
        let fall = decay_factor(dt, PEAK_DECAY_S);
        for (peak, e) in self.peaks.iter_mut().zip(env) {
            *peak = (*peak * fall).max(*e).max(ABSOLUTE_FLOOR);
        }
        let loudest = self.peaks.iter().copied().fold(ABSOLUTE_FLOOR, f32::max);
        let floor = (loudest * RELATIVE_FLOOR).max(ABSOLUTE_FLOOR);
        let mut levels = [0.0f32; BANDS];
        for ((level, e), peak) in levels.iter_mut().zip(env).zip(&self.peaks) {
            *level = (e / peak.max(floor)).clamp(0.0, 1.0);
        }

        let mut groups = [0.0f32; GROUPS];
        for (g, (lo, hi)) in groups.iter_mut().zip(GROUP_RANGES) {
            *g = levels[lo..hi].iter().sum::<f32>() / (hi - lo) as f32;
        }

        // Drum hits: positive flux inside each voice's bands, against its running average.
        let avg_k = 1.0 - decay_factor(dt, FLUX_AVERAGE_S);
        for (i, v) in VOICES.iter().enumerate() {
            let (lo, hi) = v.bands;
            let flux: f32 = (lo..hi)
                .map(|b| (raw[b] - self.prev_raw[b]).max(0.0) / self.peaks[b].max(floor))
                .sum::<f32>()
                / (hi - lo) as f32;
            self.since_hit[i] += dt;
            let threshold = (self.flux_avg[i] * HIT_RATIO).max(0.02);
            if flux > threshold && self.since_hit[i] >= v.refractory_s {
                let strength = ((flux / threshold - 1.0) * 1.5 + 0.5).clamp(0.4, 1.0);
                self.hits[i] = self.hits[i].max(strength);
                self.since_hit[i] = 0.0;
            } else {
                self.hits[i] *= decay_factor(dt, v.decay_s);
            }
            self.flux_avg[i] += avg_k * (flux - self.flux_avg[i]);
        }
        self.prev_raw = *raw;

        let k = 1.0 - decay_factor(dt, LOUDNESS_SMOOTH_S);
        self.rms_smooth += k * (rms - self.rms_smooth);
        self.rms_peak = (self.rms_peak * decay_factor(dt, LOUDNESS_PEAK_S))
            .max(self.rms_smooth)
            .max(ABSOLUTE_FLOOR);

        let total: f32 = env.iter().sum();
        let brightness = if total > 0.0 {
            env.iter()
                .enumerate()
                .map(|(i, e)| i as f32 * e)
                .sum::<f32>()
                / total
                / (BANDS - 1) as f32
        } else {
            0.0
        };

        Reaction {
            levels,
            groups,
            kick: self.hits[0],
            snare: self.hits[1],
            hat: self.hits[2],
            loudness: (self.rms_smooth / self.rms_peak).clamp(0.0, 1.0),
            brightness,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HOP_S: f32 = 256.0 / 48_000.0;

    /// Raw bands with energy `amp` in `lo..hi` and a little noise elsewhere.
    fn bands(lo: usize, hi: usize, amp: f32) -> [f32; BANDS] {
        let mut b = [0.001f32; BANDS];
        for x in &mut b[lo..hi] {
            *x = amp;
        }
        b
    }

    /// Feeds `hops` hops, with a hit of `amp` in `lo..hi` every `every` hops, and returns the
    /// maximum kick, snare and hat seen.
    fn drive(r: &mut Reactor, lo: usize, hi: usize, amp: f32, every: usize, hops: usize) -> [f32; 3] {
        let mut max = [0.0f32; 3];
        for h in 0..hops {
            let raw = if h % every < 2 { bands(lo, hi, amp) } else { bands(lo, hi, amp * 0.1) };
            let out = r.update(&raw, &raw, 0.1);
            max[0] = max[0].max(out.kick);
            max[1] = max[1].max(out.snare);
            max[2] = max[2].max(out.hat);
        }
        max
    }

    #[test]
    fn a_kick_pattern_lights_the_kick_not_the_hats() {
        let mut r = Reactor::new(HOP_S);
        // Four-on-the-floor at 125 BPM: a hit every 0.48 s, 90 hops apart.
        let max = drive(&mut r, 1, 6, 1.0, 90, 900);
        assert!(max[0] > 0.8, "kick {max:?}");
        assert!(max[2] < 0.2, "hat {max:?}");
    }

    #[test]
    fn hats_light_the_hat_voice() {
        let mut r = Reactor::new(HOP_S);
        let max = drive(&mut r, 20, 24, 0.5, 23, 900);
        assert!(max[2] > 0.8, "hat {max:?}");
        assert!(max[0] < 0.2, "kick {max:?}");
    }

    #[test]
    fn levels_do_not_depend_on_the_volume() {
        let settle = |amp: f32| {
            let mut r = Reactor::new(HOP_S);
            let raw = bands(8, 12, amp);
            let mut out = Reaction::SILENT;
            for _ in 0..2000 {
                out = r.update(&raw, &raw, amp);
            }
            out
        };
        let quiet = settle(0.05);
        let loud = settle(5.0);
        assert!((quiet.levels[9] - loud.levels[9]).abs() < 0.05);
        assert!(loud.levels[9] > 0.9, "a steady tone reads near full scale");
        assert!(loud.levels[22] < 0.1, "an empty band stays low: {}", loud.levels[22]);
        assert!(loud.groups[2] > loud.groups[5]);
    }

    #[test]
    fn a_quieter_passage_reads_lower_than_the_peak() {
        let mut r = Reactor::new(HOP_S);
        let loud = bands(3, 6, 1.0);
        for _ in 0..400 {
            r.update(&loud, &loud, 0.5);
        }
        let soft = bands(3, 6, 0.3);
        let mut out = Reaction::SILENT;
        for _ in 0..100 {
            out = r.update(&soft, &soft, 0.15);
        }
        assert!(out.groups[1] < 0.5, "bass {}", out.groups[1]);
        assert!(out.loudness < 0.6, "loudness {}", out.loudness);
    }

    #[test]
    fn brightness_follows_the_spectrum() {
        let mut r = Reactor::new(HOP_S);
        let low = r.update(&bands(0, 4, 1.0), &bands(0, 4, 1.0), 0.1).brightness;
        let high = r.update(&bands(20, 24, 1.0), &bands(20, 24, 1.0), 0.1).brightness;
        assert!(high > low + 0.5, "low {low} high {high}");
    }
}
