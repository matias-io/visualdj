//! Identifies a streamed track (TIDAL, Beatport, SoundCloud...) that rekordbox plays from
//! memory, so no file names it. rekordbox still keeps its 3-band waveform in the analysis
//! files, so Onset compares what it hears with each streaming track's waveform at the
//! deck's playhead and takes a clear winner. It only runs while the playing deck's track is
//! unknown, and it only answers when one candidate stands well above the rest.
use std::collections::VecDeque;
use std::sync::Arc;

use onset_core::bands::TrackBands;
use onset_core::track::TrackMeta;

/// Seconds of listening kept for the comparison.
const WINDOW_S: f64 = 10.0;
/// Least listening before an answer.
const MIN_S: f64 = 5.0;
/// Samples kept per second.
const RATE_HZ: f64 = 25.0;
/// Audio reaches Onset a little after the playhead passes; these lags (seconds) are tried.
const LAGS_S: [f64; 5] = [0.0, 0.05, 0.1, 0.15, 0.2];
/// A match needs this correlation, and this lead over the runner-up.
const MIN_SCORE: f32 = 0.55;
const MIN_LEAD: f32 = 0.12;

/// One moment of listening: the playhead and the live low, mid and high levels.
#[derive(Debug, Clone, Copy)]
struct Heard {
    playhead_s: f64,
    bands: [f32; 3],
}

pub struct Candidate {
    pub meta: TrackMeta,
    pub bands: Arc<TrackBands>,
}

#[derive(Default)]
pub struct StreamMatcher {
    candidates: Vec<Candidate>,
    heard: VecDeque<Heard>,
    last_sample_s: f64,
}

/// Pearson correlation; 0 when either side is flat.
fn correlation(a: &[f32], b: &[f32]) -> f32 {
    let n = a.len().min(b.len());
    if n < 3 {
        return 0.0;
    }
    let mean = |x: &[f32]| x[..n].iter().sum::<f32>() / n as f32;
    let (ma, mb) = (mean(a), mean(b));
    let (mut num, mut va, mut vb) = (0.0f32, 0.0f32, 0.0f32);
    for i in 0..n {
        let (da, db) = (a[i] - ma, b[i] - mb);
        num += da * db;
        va += da * da;
        vb += db * db;
    }
    if va < 1e-6 || vb < 1e-6 {
        0.0
    } else {
        num / (va * vb).sqrt()
    }
}

impl StreamMatcher {
    pub fn new(candidates: Vec<Candidate>) -> Self {
        Self {
            candidates,
            ..Self::default()
        }
    }

    pub fn has_candidates(&self) -> bool {
        !self.candidates.is_empty()
    }

    /// Forgets what was heard (a new deck or a jump in the playhead).
    pub fn reset(&mut self) {
        self.heard.clear();
    }

    /// Records the live levels at `playhead_s`; `now_s` is a monotonic clock. A playhead
    /// that jumps (a cue, a loop) starts the listening over.
    pub fn listen(&mut self, now_s: f64, playhead_s: f64, bands: [f32; 3]) {
        if now_s - self.last_sample_s < 1.0 / RATE_HZ {
            return;
        }
        self.last_sample_s = now_s;
        if let Some(last) = self.heard.back() {
            let step = playhead_s - last.playhead_s;
            if !(-0.05..=1.0).contains(&step) {
                self.heard.clear();
            }
        }
        self.heard.push_back(Heard { playhead_s, bands });
        while self
            .heard
            .front()
            .is_some_and(|h| playhead_s - h.playhead_s > WINDOW_S)
        {
            self.heard.pop_front();
        }
    }

    /// How well a track's waveform matches what was heard: the mean band correlation at the
    /// best lag.
    fn score(&self, bands: &TrackBands) -> f32 {
        let mut best = f32::MIN;
        for lag in LAGS_S {
            let mut per_band = [0.0f32; 3];
            for (b, out) in per_band.iter_mut().enumerate() {
                let live: Vec<f32> = self.heard.iter().map(|h| h.bands[b]).collect();
                let track: Vec<f32> = self
                    .heard
                    .iter()
                    .map(|h| {
                        let at = bands.at(h.playhead_s - lag);
                        [at.low, at.mid, at.high][b]
                    })
                    .collect();
                *out = correlation(&live, &track);
            }
            // The low band carries the kicks and drops; weight it most.
            let s = 0.5 * per_band[0] + 0.3 * per_band[1] + 0.2 * per_band[2];
            best = best.max(s);
        }
        best
    }

    /// The streaming track playing, once enough has been heard and one clearly wins.
    pub fn best(&self) -> Option<&TrackMeta> {
        let (first, last) = (self.heard.front()?, self.heard.back()?);
        if last.playhead_s - first.playhead_s < MIN_S {
            return None;
        }
        let playhead = last.playhead_s;
        let mut scored: Vec<(f32, &Candidate)> = self
            .candidates
            .iter()
            .filter(|c| c.meta.duration_s.is_none_or(|d| f64::from(d) + 1.0 >= playhead))
            .map(|c| (self.score(&c.bands), c))
            .collect();
        scored.sort_by(|a, b| b.0.total_cmp(&a.0));
        let (top, winner) = *scored.first()?;
        let second = scored.get(1).map_or(0.0, |s| s.0);
        (top >= MIN_SCORE && top - second >= MIN_LEAD).then_some(&winner.meta)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use onset_core::track::{TrackExtra, TrackId};

    fn meta(id: u64) -> TrackMeta {
        TrackMeta {
            id: TrackId(id),
            title: format!("t{id}"),
            artist: "a".into(),
            album: String::new(),
            year: None,
            key: None,
            bpm: None,
            duration_s: Some(200.0),
            sample_rate: None,
            file_path: None,
            artwork_path: None,
            analysis_path: None,
            isrc: None,
            genre: None,
            extra: TrackExtra::default(),
        }
    }

    /// A waveform whose bands follow `f` at 150 frames a second.
    fn bands(f: impl Fn(f64) -> [u8; 3]) -> Arc<TrackBands> {
        let frames = (0..150 * 200).map(|i| f(f64::from(i) / 150.0)).collect();
        Arc::new(TrackBands::new(150.0, frames, 0.0, Vec::new()))
    }

    fn pulse(t: f64, period: f64) -> u8 {
        if (t / period).fract() < 0.25 { 220 } else { 30 }
    }

    #[test]
    fn correlation_is_one_for_the_same_shape_and_zero_for_flat() {
        let a = [1.0, 3.0, 2.0, 5.0];
        let b = [2.0, 6.0, 4.0, 10.0];
        assert!((correlation(&a, &b) - 1.0).abs() < 1e-5);
        assert!(correlation(&a, &[1.0; 4]).abs() < 1e-6);
    }

    #[test]
    fn finds_the_track_whose_waveform_matches_what_is_heard() {
        let right = bands(|t| [pulse(t, 0.47), pulse(t + 0.2, 1.9), pulse(t, 0.23)]);
        let wrong = bands(|t| [pulse(t, 0.61), pulse(t, 2.7), pulse(t + 0.1, 0.31)]);
        let mut m = StreamMatcher::new(vec![
            Candidate { meta: meta(2), bands: wrong },
            Candidate { meta: meta(1), bands: right.clone() },
        ]);
        assert!(m.best().is_none(), "nothing heard yet");
        // Listen from 60 s, hearing the right track with 0.1 s of latency.
        for k in 0..300 {
            let now = f64::from(k) / 25.0;
            let playhead = 60.0 + now;
            let at = right.at(playhead - 0.1);
            m.listen(now, playhead, [at.low, at.mid, at.high]);
        }
        assert_eq!(m.best().map(|t| t.id), Some(TrackId(1)));
    }

    #[test]
    fn stays_quiet_when_nothing_matches() {
        let a = bands(|t| [pulse(t, 0.47), 40, 40]);
        let b = bands(|t| [pulse(t, 0.61), 40, 40]);
        let mut m = StreamMatcher::new(vec![
            Candidate { meta: meta(1), bands: a },
            Candidate { meta: meta(2), bands: b },
        ]);
        for k in 0..300 {
            let now = f64::from(k) / 25.0;
            // Something else entirely: a slow wobble.
            let v = (now * 0.9).sin() as f32 * 0.5 + 0.5;
            m.listen(now, 60.0 + now, [v, 1.0 - v, v * 0.5]);
        }
        assert!(m.best().is_none());
    }

    #[test]
    fn a_jump_in_the_playhead_starts_over() {
        let mut m = StreamMatcher::new(Vec::new());
        for k in 0..200 {
            let now = f64::from(k) / 25.0;
            m.listen(now, 60.0 + now, [0.5; 3]);
        }
        m.listen(8.1, 10.0, [0.5; 3]);
        assert_eq!(m.heard.len(), 1);
    }
}
