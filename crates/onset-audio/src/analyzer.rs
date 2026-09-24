//! Turns a stream of mono samples into `AudioFeatures`: a 2048-sample Hann-windowed FFT run
//! every 256-sample hop, folded into 24 log-spaced bands (30 Hz-16 kHz) with per-band envelope
//! followers, RMS, a spectral-flux onset flag, and a running-silence flag.

use onset_core::audio_features::{AudioFeatures, BANDS};
use rustfft::num_complex::Complex32;
use rustfft::{Fft, FftPlanner};
use std::collections::VecDeque;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// FFT window length, in samples.
const WINDOW: usize = 2048;
/// Hop length, in samples: `push` emits one `AudioFeatures` per `HOP` samples consumed.
const HOP: usize = 256;
/// Lowest edge of the lowest band, in Hz.
const BAND_LO_HZ: f32 = 30.0;
/// Highest edge of the highest band, in Hz.
const BAND_HI_HZ: f32 = 16_000.0;
/// Envelope follower attack coefficient, applied per hop when the input rises.
const ATTACK: f32 = 0.6;
/// Envelope follower release coefficient, applied per hop when the input falls.
const RELEASE: f32 = 0.08;
/// Number of past flux values kept for the running-median onset threshold.
const FLUX_HISTORY: usize = 43;
/// Onset fires when the current flux exceeds this multiple of the running median.
const ONSET_MULTIPLIER: f32 = 1.5;
/// RMS below this is considered silent for a single hop.
const SILENT_RMS: f32 = 1e-4;
/// Consecutive silent hops required before `AudioFeatures::silent` is set.
const SILENT_HOPS: u32 = 20;

/// Streaming band-energy, RMS, onset and silence analyzer.
///
/// Feed it mono samples of any chunk size via [`Analyzer::push`]; it buffers internally and
/// emits one [`AudioFeatures`] per completed `HOP`-sample hop.
pub struct Analyzer {
    sample_rate: u32,
    fft: Arc<dyn Fft<f32>>,
    hann: Vec<f32>,
    /// Rolling `WINDOW`-sample buffer of the most recent audio, oldest first.
    window: Vec<f32>,
    /// Samples pushed but not yet consumed into a full hop.
    pending: Vec<f32>,
    /// Envelope-followed value per band; this is what `AudioFeatures::bands` reports.
    band_env: [f32; BANDS],
    /// Raw (pre-envelope) per-band magnitude from the previous hop, for spectral flux.
    prev_band_raw: [f32; BANDS],
    /// Recent flux values, for the running-median onset threshold.
    flux_history: VecDeque<f32>,
    /// Consecutive hops with RMS below `SILENT_RMS`.
    silent_run: u32,
    /// When the most recent block was pushed, and the features it produced.
    last_push: Option<Instant>,
    last: AudioFeatures,
}

/// Features older than this are reported as silence: the capture stopped delivering blocks
/// (idle WASAPI endpoint, device removed), so the last bands are stale.
pub const STALE_AFTER: Duration = Duration::from_millis(300);

impl Analyzer {
    /// Creates an analyzer for a `sample_rate`-Hz mono stream.
    #[must_use]
    pub fn new(sample_rate: u32) -> Self {
        let mut planner = FftPlanner::<f32>::new();
        let fft = planner.plan_fft_forward(WINDOW);
        let hann = (0..WINDOW)
            .map(|n| {
                0.5 - 0.5 * (2.0 * std::f32::consts::PI * n as f32 / (WINDOW - 1) as f32).cos()
            })
            .collect();
        Self {
            sample_rate,
            fft,
            hann,
            window: vec![0.0; WINDOW],
            pending: Vec::with_capacity(HOP * 2),
            band_env: [0.0; BANDS],
            prev_band_raw: [0.0; BANDS],
            flux_history: VecDeque::with_capacity(FLUX_HISTORY),
            silent_run: 0,
            last_push: None,
            last: AudioFeatures::silent(),
        }
    }

    /// Feeds `mono` samples in; returns the features for the last hop completed by this call,
    /// or `None` if `mono` did not fill a full hop.
    pub fn push(&mut self, mono: &[f32]) -> Option<AudioFeatures> {
        self.push_at(mono, Instant::now())
    }

    /// The freshest features, or [`AudioFeatures::silent`] when no block arrived within
    /// [`STALE_AFTER`] of `now`.
    pub fn latest(&self, now: Instant) -> AudioFeatures {
        match self.last_push {
            Some(t) if now.saturating_duration_since(t) <= STALE_AFTER => self.last,
            _ => AudioFeatures::silent(),
        }
    }

    /// Like [`Self::push`] with an explicit timestamp (for tests and replay).
    pub fn push_at(&mut self, mono: &[f32], now: Instant) -> Option<AudioFeatures> {
        self.last_push = Some(now);
        self.pending.extend_from_slice(mono);
        let mut last = None;
        while self.pending.len() >= HOP {
            let hop: Vec<f32> = self.pending.drain(..HOP).collect();
            last = Some(self.process_hop(&hop));
        }
        last
    }

    fn process_hop(&mut self, hop: &[f32]) -> AudioFeatures {
        debug_assert_eq!(hop.len(), HOP);

        // Slide the analysis window forward by one hop.
        self.window.copy_within(HOP.., 0);
        self.window[WINDOW - HOP..].copy_from_slice(hop);

        let rms = {
            let sum_sq: f32 = hop.iter().map(|s| s * s).sum();
            (sum_sq / hop.len() as f32).sqrt()
        };

        let band_raw = self.band_magnitudes();

        let flux: f32 = band_raw
            .iter()
            .zip(self.prev_band_raw.iter())
            .map(|(cur, prev)| (cur - prev).max(0.0))
            .sum();
        let median = running_median(&self.flux_history);
        let onset = flux > ONSET_MULTIPLIER * median;
        if self.flux_history.len() == FLUX_HISTORY {
            self.flux_history.pop_front();
        }
        self.flux_history.push_back(flux);
        self.prev_band_raw = band_raw;

        for (env, raw) in self.band_env.iter_mut().zip(band_raw.iter()) {
            let coeff = if *raw > *env { ATTACK } else { RELEASE };
            *env += coeff * (*raw - *env);
        }

        self.silent_run = if rms < SILENT_RMS {
            self.silent_run + 1
        } else {
            0
        };

        let silent = self.silent_run >= SILENT_HOPS;
        if silent {
            // Stale envelopes must not keep the visuals moving in silence.
            self.band_env = [0.0; BANDS];
        }
        let features = AudioFeatures {
            bands: self.band_env,
            rms,
            onset: onset && !silent,
            silent,
        };
        self.last = features;
        features
    }

    /// Runs the FFT over the current windowed buffer and folds the magnitude spectrum into
    /// `BANDS` log-spaced bands, each scaled by `1 / sqrt(bins_in_band)`.
    fn band_magnitudes(&self) -> [f32; BANDS] {
        let mut buf: Vec<Complex32> = self
            .window
            .iter()
            .zip(self.hann.iter())
            .map(|(s, w)| Complex32::new(s * w, 0.0))
            .collect();
        self.fft.process(&mut buf);

        let bin_width = self.sample_rate as f32 / WINDOW as f32;
        let nyquist_bin = WINDOW / 2;
        let edges = band_edges();

        let mut bands = [0.0f32; BANDS];
        for (k, band) in bands.iter_mut().enumerate() {
            let lo = freq_to_bin(edges[k], bin_width, nyquist_bin);
            let hi = freq_to_bin(edges[k + 1], bin_width, nyquist_bin).max(lo + 1);
            let hi = hi.min(nyquist_bin + 1);
            let sum: f32 = buf[lo..hi].iter().map(|c| c.norm()).sum();
            let bins_in_band = (hi - lo) as f32;
            *band = sum / bins_in_band.sqrt();
        }
        bands
    }
}

/// The `BANDS + 1` log-spaced edge frequencies (Hz) from `BAND_LO_HZ` to `BAND_HI_HZ`.
fn band_edges() -> [f32; BANDS + 1] {
    let mut edges = [0.0f32; BANDS + 1];
    let ratio = BAND_HI_HZ / BAND_LO_HZ;
    for (k, edge) in edges.iter_mut().enumerate() {
        *edge = BAND_LO_HZ * ratio.powf(k as f32 / BANDS as f32);
    }
    edges
}

fn freq_to_bin(freq: f32, bin_width: f32, max_bin: usize) -> usize {
    (freq / bin_width).round().clamp(0.0, max_bin as f32) as usize
}

/// A rough running median: sorts a copy of the history and takes the middle element (or the
/// average of the two middle elements). Good enough for a threshold on <= 43 values.
fn running_median(history: &VecDeque<f32>) -> f32 {
    if history.is_empty() {
        return 0.0;
    }
    let mut sorted: Vec<f32> = history.iter().copied().collect();
    sorted.sort_by(f32::total_cmp);
    let mid = sorted.len() / 2;
    if sorted.len().is_multiple_of(2) {
        f32::midpoint(sorted[mid - 1], sorted[mid])
    } else {
        sorted[mid]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn sine(freq: f32, secs: f32, sr: u32) -> Vec<f32> {
        (0..(secs * sr as f32) as usize)
            .map(|i| (2.0 * std::f32::consts::PI * freq * i as f32 / sr as f32).sin() * 0.5)
            .collect()
    }
    fn run(a: &mut Analyzer, buf: &[f32]) -> AudioFeatures {
        let mut last = None;
        for c in buf.chunks(256) {
            if let Some(f) = a.push(c) {
                last = Some(f);
            }
        }
        last.unwrap()
    }
    #[test]
    fn low_tone_lands_in_low_bands() {
        let mut a = Analyzer::new(48_000);
        let f = run(&mut a, &sine(60.0, 1.0, 48_000));
        let (lo, hi): (f32, f32) = (f.bands[..4].iter().sum(), f.bands[16..].iter().sum());
        assert!(lo > 5.0 * hi, "lo {lo} hi {hi}");
        assert!(!f.silent);
    }
    #[test]
    fn high_tone_lands_in_high_bands() {
        let mut a = Analyzer::new(48_000);
        let f = run(&mut a, &sine(8_000.0, 1.0, 48_000));
        let (lo, hi): (f32, f32) = (f.bands[..4].iter().sum(), f.bands[16..].iter().sum());
        assert!(hi > 5.0 * lo, "lo {lo} hi {hi}");
    }
    #[test]
    fn silence_is_flagged() {
        let mut a = Analyzer::new(48_000);
        let f = run(&mut a, &vec![0.0f32; 48_000]);
        assert!(f.silent);
        assert!(f.rms < 1e-4);
    }
    #[test]
    fn onset_fires_on_a_step() {
        let mut a = Analyzer::new(48_000);
        let mut buf = vec![0.0f32; 24_000];
        buf.extend(sine(1_000.0, 0.5, 48_000));
        let mut fired = false;
        for c in buf.chunks(256) {
            if let Some(f) = a.push(c) {
                fired |= f.onset;
            }
        }
        assert!(fired);
    }
}

#[cfg(test)]
mod staleness_tests {
    use super::*;
    use std::time::{Duration, Instant};

    fn sine(freq: f32, secs: f32, sr: u32) -> Vec<f32> {
        (0..(secs * sr as f32) as usize)
            .map(|i| (2.0 * std::f32::consts::PI * freq * i as f32 / sr as f32).sin() * 0.5)
            .collect()
    }

    #[test]
    fn silence_after_a_tone_zeroes_the_bands() {
        let mut a = Analyzer::new(48_000);
        let mut last = None;
        for c in sine(440.0, 1.0, 48_000).chunks(256) {
            if let Some(f) = a.push(c) {
                last = Some(f);
            }
        }
        assert!(
            last.unwrap().bands.iter().any(|b| *b > 0.01),
            "tone must register"
        );
        for c in vec![0.0f32; 48_000].chunks(256) {
            if let Some(f) = a.push(c) {
                last = Some(f);
            }
        }
        let f = last.unwrap();
        assert!(f.silent);
        assert!(f.bands.iter().all(|b| *b < 1e-3), "{:?}", f.bands);
    }

    #[test]
    fn latest_reports_silent_when_blocks_stop_arriving() {
        let t0 = Instant::now();
        let mut a = Analyzer::new(48_000);
        for c in sine(440.0, 0.5, 48_000).chunks(256) {
            a.push_at(c, t0);
        }
        let fresh = a.latest(t0 + Duration::from_millis(50));
        assert!(
            !fresh.silent,
            "features are fresh 50 ms after the last block"
        );
        let stale = a.latest(t0 + Duration::from_millis(500));
        assert!(stale.silent);
        assert!(stale.bands.iter().all(|b| *b == 0.0));
    }
}
