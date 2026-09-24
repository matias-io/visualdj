//! Captures the mix that rekordbox's PC MASTER OUT sends to a Windows output endpoint.
//!
//! There is no dedicated loopback API in cpal: on Windows, calling
//! [`DeviceTrait::build_input_stream`] on an *output* device opens a WASAPI loopback capture of
//! whatever that device is playing, instead of failing as "not an input device". That is the
//! mechanism this module relies on; it is a Windows-only trick, but this crate only ever targets
//! Windows (see the workspace README).

use anyhow::{Context, Result, anyhow};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{Device, Host, Sample, SampleFormat, Stream, StreamConfig};
use crossbeam_channel::{Receiver, Sender, bounded};

/// Number of mono frames per block sent on the capture channel.
const BLOCK_FRAMES: usize = 256;
/// Capacity of the capture channel, in blocks.
const CHANNEL_CAPACITY: usize = 256;

/// Names of every output device on the default host: the endpoints selectable for loopback
/// capture. Returns an empty vector (rather than erroring) when the host or its device list is
/// unavailable, since this is primarily used to populate a picker.
#[must_use]
pub fn list_output_endpoints() -> Vec<String> {
    let host = cpal::default_host();
    host.output_devices()
        .map(|devices| devices.map(|device| device.to_string()).collect())
        .unwrap_or_default()
}

/// A live WASAPI loopback capture. Keep this alive for as long as capture should continue;
/// dropping it stops and tears down the underlying `cpal::Stream`.
pub struct LoopbackCapture {
    /// Kept only to hold the stream open (and stop it on drop); never read otherwise.
    _stream: Stream,
}

impl LoopbackCapture {
    /// Starts loopback capture on the output device whose name contains `endpoint_name`
    /// (case-insensitive), falling back to the default output device when `endpoint_name` is
    /// `None` or matches no device.
    ///
    /// Returns the capture handle, a receiver of 256-frame mono blocks (downmixed from whatever
    /// channel layout the device reports), and the device's sample rate in Hz.
    ///
    /// # Errors
    ///
    /// Returns an error if no output device is available, its default config cannot be read, the
    /// device's sample format is not one this module converts (see [`build_stream`]), or the
    /// stream fails to build or start.
    pub fn start(endpoint_name: Option<&str>) -> Result<(Self, Receiver<Vec<f32>>, u32)> {
        let host = cpal::default_host();
        let device = select_output_device(&host, endpoint_name)
            .context("no output device available for loopback capture")?;
        let config = device
            .default_output_config()
            .context("failed to read the output device's default config")?;
        let sample_rate = config.sample_rate();
        let channels = usize::from(config.channels());
        let sample_format = config.sample_format();
        let stream_config: StreamConfig = config.config();

        let (tx, rx) = bounded::<Vec<f32>>(CHANNEL_CAPACITY);
        let stream = build_stream(&device, &stream_config, sample_format, channels, tx)?;
        stream
            .play()
            .context("failed to start the loopback stream")?;

        Ok((Self { _stream: stream }, rx, sample_rate))
    }
}

/// Picks the output device whose name contains `endpoint_name` (case-insensitive), or the
/// default output device when no name is given or nothing matches.
fn select_output_device(host: &Host, endpoint_name: Option<&str>) -> Option<Device> {
    if let Some(name) = endpoint_name {
        let needle = name.to_lowercase();
        let matched = host.output_devices().ok().and_then(|mut devices| {
            devices.find(|d| d.to_string().to_lowercase().contains(&needle))
        });
        if matched.is_some() {
            return matched;
        }
    }
    host.default_output_device()
}

/// Builds the input (loopback) stream for whichever sample format the device reports, converting
/// integer formats to `f32` via [`Sample::to_float_sample`]. Returns an error for sample formats
/// this module does not (yet) convert.
fn build_stream(
    device: &Device,
    config: &StreamConfig,
    sample_format: SampleFormat,
    channels: usize,
    tx: Sender<Vec<f32>>,
) -> Result<Stream> {
    match sample_format {
        SampleFormat::F32 => {
            let mut pending = Vec::with_capacity(BLOCK_FRAMES * 2);
            device
                .build_input_stream(
                    *config,
                    move |data: &[f32], _: &cpal::InputCallbackInfo| {
                        push_downmixed(&mut pending, data, channels, &tx);
                    },
                    log_stream_error,
                    None,
                )
                .context("failed to build f32 loopback input stream")
        }
        SampleFormat::I16 => {
            let mut pending = Vec::with_capacity(BLOCK_FRAMES * 2);
            let mut floats = Vec::new();
            device
                .build_input_stream(
                    *config,
                    move |data: &[i16], _: &cpal::InputCallbackInfo| {
                        floats.clear();
                        floats.extend(data.iter().map(|s| s.to_float_sample()));
                        push_downmixed(&mut pending, &floats, channels, &tx);
                    },
                    log_stream_error,
                    None,
                )
                .context("failed to build i16 loopback input stream")
        }
        SampleFormat::U16 => {
            let mut pending = Vec::with_capacity(BLOCK_FRAMES * 2);
            let mut floats = Vec::new();
            device
                .build_input_stream(
                    *config,
                    move |data: &[u16], _: &cpal::InputCallbackInfo| {
                        floats.clear();
                        floats.extend(data.iter().map(|s| s.to_float_sample()));
                        push_downmixed(&mut pending, &floats, channels, &tx);
                    },
                    log_stream_error,
                    None,
                )
                .context("failed to build u16 loopback input stream")
        }
        other => Err(anyhow!(
            "output device uses sample format {other:?}, which loopback capture does not convert \
             (only F32, I16 and U16 are supported)"
        )),
    }
}

// Takes `err` by value because it must match cpal's `FnMut(cpal::Error)` error-callback signature.
#[allow(clippy::needless_pass_by_value)]
fn log_stream_error(err: cpal::Error) {
    tracing::warn!("loopback capture stream error: {err}");
}

/// Downmixes interleaved `data` (with `channels` channels per frame) to mono, appends it to
/// `pending`, and sends every complete `BLOCK_FRAMES`-frame block on `tx`. Drops a block silently
/// if the channel is full: capture must never block the audio callback.
fn push_downmixed(pending: &mut Vec<f32>, data: &[f32], channels: usize, tx: &Sender<Vec<f32>>) {
    if channels <= 1 {
        pending.extend_from_slice(data);
    } else {
        pending.extend(
            data.chunks_exact(channels)
                .map(|frame| frame.iter().sum::<f32>() / channels as f32),
        );
    }
    while pending.len() >= BLOCK_FRAMES {
        let block: Vec<f32> = pending.drain(..BLOCK_FRAMES).collect();
        let _ = tx.try_send(block);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn listing_output_endpoints_does_not_panic() {
        let endpoints = list_output_endpoints();
        // May legitimately be empty on a CI runner with no audio devices.
        let _ = endpoints;
    }

    /// Manual smoke test: starts real loopback capture on the default output device for 500 ms
    /// and reports how many 256-frame blocks arrived. Requires an actual audio device, so it is
    /// `#[ignore]`d; run explicitly with `cargo test -p onset-audio -- --ignored capture`.
    #[test]
    #[ignore = "requires a real output device"]
    fn manual_smoke_default_output_loopback() {
        let (_capture, rx, sample_rate) =
            LoopbackCapture::start(None).expect("loopback capture should start");
        std::thread::sleep(std::time::Duration::from_millis(500));
        let mut blocks = 0usize;
        while rx.try_recv().is_ok() {
            blocks += 1;
        }
        println!("sample_rate={sample_rate} blocks_received={blocks}");
    }
}
