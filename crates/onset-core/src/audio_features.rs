//! The per-hop audio feature frame produced by `onset-audio` and consumed by the core's
//! `MusicState` assembly. No I/O; just the shared data shape.

/// Number of log-spaced frequency bands the analyzer reports.
pub const BANDS: usize = 24;
/// Frequency groups: sub, bass, low-mid, mid, high-mid, treble.
pub const GROUPS: usize = 6;

/// Audio features for a single analysis hop: band energies, loudness, and onset/silence flags.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AudioFeatures {
    /// Per-band envelope-followed magnitude, 24 log-spaced bands from 30 Hz to 16 kHz.
    pub bands: [f32; BANDS],
    /// Root-mean-square loudness over the hop.
    pub rms: f32,
    /// True when spectral flux exceeded its running-median threshold on this hop.
    pub onset: bool,
    /// True when the signal has been below the silence threshold for enough consecutive hops.
    pub silent: bool,
    /// Each band normalised to 0..1 against its own recent peak (volume-independent).
    pub levels: [f32; BANDS],
    /// Mean level of sub, bass, low-mid, mid, high-mid and treble, 0..1.
    pub groups: [f32; GROUPS],
    /// Drum hits, 1 at the hit and decaying over about a tenth of a second.
    pub kick: f32,
    pub snare: f32,
    pub hat: f32,
    /// Loudness relative to the last twenty seconds' peak, 0..1.
    pub loudness: f32,
    /// Spectral centroid mapped to 0 (all bass) ..1 (all treble).
    pub brightness: f32,
}

impl AudioFeatures {
    /// All-zero features flagged as silent: the value used before any audio has arrived.
    #[must_use]
    pub fn silent() -> Self {
        Self {
            bands: [0.0; BANDS],
            rms: 0.0,
            onset: false,
            silent: true,
            levels: [0.0; BANDS],
            groups: [0.0; GROUPS],
            kick: 0.0,
            snare: 0.0,
            hat: 0.0,
            loudness: 0.0,
            brightness: 0.0,
        }
    }
}

impl Default for AudioFeatures {
    fn default() -> Self {
        Self::silent()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[allow(clippy::float_cmp)] // exact 0.0 from a fresh `silent()`, not a computed float
    fn silent_is_all_zero_and_flagged() {
        let f = AudioFeatures::silent();
        assert_eq!(f.bands, [0.0; BANDS]);
        assert_eq!(f.rms, 0.0);
        assert!(!f.onset);
        assert!(f.silent);
    }

    #[test]
    fn default_matches_silent() {
        assert_eq!(AudioFeatures::default(), AudioFeatures::silent());
    }
}
