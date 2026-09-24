//! Simulator transport: plays a file through the default output device and reports an exact
//! playhead, so the whole app can run and be tested without rekordbox.
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::time::Instant;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use onset_core::transport::{TrackRef, TransportSnapshot};

use crate::decode::{DecodedAudio, decode_file};
use crate::source::{SourceStatus, TransportSource};

/// Fixed-point scale for the shared playhead (file frames × 1000).
const POS_SCALE: f64 = 1000.0;
/// Mono blocks handed to the analyzer.
const TAP_BLOCK: usize = 256;

pub struct SimPlayer {
    _stream: cpal::Stream,
    pos_milli: Arc<AtomicU64>,
    rate_milli: Arc<AtomicU64>,
    gain_bits: Arc<AtomicU32>,
    paused: Arc<AtomicBool>,
    file_rate: u32,
    device_rate: u32,
    track: TrackRef,
    bpm_original: f32,
    audio_rx: crossbeam_channel::Receiver<Vec<f32>>,
}

/// Linear-interpolated stereo sample at fractional frame `p`.
fn sample_at(d: &DecodedAudio, p: f64) -> (f32, f32) {
    let ch = usize::from(d.channels.max(1));
    let frames = d.frames();
    let i = p.floor() as usize;
    if i + 1 >= frames {
        return (0.0, 0.0);
    }
    let frac = (p - i as f64) as f32;
    let at = |frame: usize, c: usize| d.samples[frame * ch + c.min(ch - 1)];
    let lerp = |c: usize| at(i, c) + (at(i + 1, c) - at(i, c)) * frac;
    (lerp(0), lerp(1))
}

impl SimPlayer {
    /// Decodes `path` (blocking; a few hundred ms for a FLAC) and starts playback from 0.
    pub fn start(path: &Path, track: TrackRef, bpm_original: f32) -> anyhow::Result<Self> {
        let decoded = Arc::new(decode_file(path)?);
        let host = cpal::default_host();
        let device = host
            .default_output_device()
            .ok_or_else(|| anyhow::anyhow!("no default output device"))?;
        let supported = device.default_output_config()?;
        let device_rate = supported.sample_rate();
        let config: cpal::StreamConfig = supported.config();
        let out_channels = usize::from(config.channels);

        let pos_milli = Arc::new(AtomicU64::new(0));
        let rate_milli = Arc::new(AtomicU64::new(1000));
        let gain_bits = Arc::new(AtomicU32::new(1.0f32.to_bits()));
        let paused = Arc::new(AtomicBool::new(false));
        let (audio_tx, audio_rx) = crossbeam_channel::bounded::<Vec<f32>>(64);

        // Playback advances through the file at rate × (file Hz / device Hz) frames per
        // output frame, which resamples by linear interpolation.
        let rate_ratio = f64::from(decoded.sample_rate) / f64::from(device_rate);
        let (d, pos, rate, gain, pz) = (
            decoded.clone(),
            pos_milli.clone(),
            rate_milli.clone(),
            gain_bits.clone(),
            paused.clone(),
        );
        let mut mono_block: Vec<f32> = Vec::with_capacity(TAP_BLOCK);
        let stream = device.build_output_stream(
            config,
            move |out: &mut [f32], _| {
                let step = rate.load(Ordering::Relaxed) as f64 / POS_SCALE * rate_ratio;
                let g = f32::from_bits(gain.load(Ordering::Relaxed));
                let is_paused = pz.load(Ordering::Relaxed);
                let mut p = pos.load(Ordering::Relaxed) as f64 / POS_SCALE;
                for frame in out.chunks_mut(out_channels) {
                    let (l, r) = if is_paused {
                        (0.0, 0.0)
                    } else {
                        let s = sample_at(&d, p);
                        p += step;
                        s
                    };
                    for (c, slot) in frame.iter_mut().enumerate() {
                        *slot = if c % 2 == 0 { l } else { r } * g;
                    }
                    mono_block.push(0.5 * (l + r));
                    if mono_block.len() == TAP_BLOCK {
                        let _ = audio_tx.try_send(std::mem::replace(
                            &mut mono_block,
                            Vec::with_capacity(TAP_BLOCK),
                        ));
                    }
                }
                if !is_paused {
                    pos.store((p * POS_SCALE) as u64, Ordering::Relaxed);
                }
            },
            |e| tracing::error!("sim output stream error: {e}"),
            None,
        )?;
        stream.play()?;
        tracing::info!(
            file_rate = decoded.sample_rate,
            device_rate,
            secs = decoded.duration_s(),
            "sim player started"
        );

        Ok(Self {
            _stream: stream,
            pos_milli,
            rate_milli,
            gain_bits,
            paused,
            file_rate: decoded.sample_rate,
            device_rate,
            track,
            bpm_original,
            audio_rx,
        })
    }

    /// Playback rate; 1.0 is the file's tempo, 1.05 is +5 %.
    pub fn set_rate(&self, rate: f32) {
        let r = (f64::from(rate.max(0.01)) * POS_SCALE).round();
        self.rate_milli.store(r as u64, Ordering::Relaxed);
    }

    pub fn seek(&self, seconds: f64) {
        let frames = seconds.max(0.0) * f64::from(self.file_rate);
        self.pos_milli
            .store((frames * POS_SCALE) as u64, Ordering::Relaxed);
    }

    pub fn pause(&self, paused: bool) {
        self.paused.store(paused, Ordering::Relaxed);
    }

    /// Output gain, 0..1. The analyzer tap is taken before gain.
    pub fn set_gain(&self, gain: f32) {
        self.gain_bits
            .store(gain.clamp(0.0, 1.0).to_bits(), Ordering::Relaxed);
    }

    /// Mono blocks of 256 frames at [`Self::tap_sample_rate`], pre-gain.
    pub fn tap_audio(&self) -> crossbeam_channel::Receiver<Vec<f32>> {
        self.audio_rx.clone()
    }

    pub fn tap_sample_rate(&self) -> u32 {
        self.device_rate
    }

    pub fn playhead_s(&self) -> f64 {
        self.pos_milli.load(Ordering::Relaxed) as f64 / POS_SCALE / f64::from(self.file_rate)
    }
}

impl TransportSource for SimPlayer {
    fn name(&self) -> &'static str {
        "sim"
    }

    fn status(&self) -> SourceStatus {
        SourceStatus::Connected
    }

    fn poll(&mut self) -> Option<TransportSnapshot> {
        let rate = self.rate_milli.load(Ordering::Relaxed) as f32 / POS_SCALE as f32;
        Some(TransportSnapshot {
            deck: 0,
            track: self.track.clone(),
            playhead_s: self.playhead_s(),
            bpm_now: self.bpm_original * rate,
            bpm_original: self.bpm_original,
            playing: !self.paused.load(Ordering::Relaxed),
            read_at: Instant::now(),
        })
    }
}
