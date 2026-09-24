//! Decode an audio file fully into interleaved `f32` samples (FLAC, MP3, WAV).
use std::fs::File;
use std::path::Path;

use symphonia::core::audio::sample::Sample;
use symphonia::core::codecs::audio::AudioDecoderOptions;
use symphonia::core::errors::Error;
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, TrackType};
use symphonia::core::io::{MediaSourceStream, MediaSourceStreamOptions};
use symphonia::core::meta::MetadataOptions;

pub struct DecodedAudio {
    pub sample_rate: u32,
    pub channels: u16,
    /// Interleaved samples, `channels` per frame.
    pub samples: Vec<f32>,
}

impl DecodedAudio {
    pub fn frames(&self) -> usize {
        self.samples.len() / usize::from(self.channels.max(1))
    }

    pub fn duration_s(&self) -> f64 {
        self.frames() as f64 / f64::from(self.sample_rate.max(1))
    }
}

pub fn decode_file(path: &Path) -> anyhow::Result<DecodedAudio> {
    let file = Box::new(File::open(path)?);
    let mss = MediaSourceStream::new(file, MediaSourceStreamOptions::default());
    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }

    let mut format = symphonia::default::get_probe().probe(
        &hint,
        mss,
        FormatOptions::default(),
        MetadataOptions::default(),
    )?;
    let track = format
        .default_track(TrackType::Audio)
        .ok_or_else(|| anyhow::anyhow!("no audio track in {}", path.display()))?;
    let track_id = track.id;
    let codec_params = track
        .codec_params
        .as_ref()
        .and_then(|p| p.audio())
        .ok_or_else(|| anyhow::anyhow!("no audio codec parameters in {}", path.display()))?;
    let mut decoder = symphonia::default::get_codecs()
        .make_audio_decoder(codec_params, &AudioDecoderOptions::default())?;

    let mut out: Vec<f32> = Vec::new();
    let mut scratch: Vec<f32> = Vec::new();
    let mut sample_rate = 0u32;
    let mut channels = 0u16;

    while let Some(packet) = format.next_packet()? {
        if packet.track_id != track_id {
            continue;
        }
        match decoder.decode(&packet) {
            Ok(buf) => {
                if sample_rate == 0 {
                    let spec = buf.spec();
                    sample_rate = spec.rate();
                    channels = u16::try_from(spec.channels().count()).unwrap_or(2);
                }
                scratch.resize(buf.samples_interleaved(), f32::MID);
                buf.copy_to_slice_interleaved(&mut scratch);
                out.extend_from_slice(&scratch);
            }
            // A damaged frame should not abort the whole track.
            Err(Error::DecodeError(e)) => tracing::warn!("decode error skipped: {e}"),
            Err(e) => return Err(e.into()),
        }
    }

    anyhow::ensure!(
        sample_rate > 0 && !out.is_empty(),
        "no audio decoded from {}",
        path.display()
    );
    Ok(DecodedAudio {
        sample_rate,
        channels,
        samples: out,
    })
}
