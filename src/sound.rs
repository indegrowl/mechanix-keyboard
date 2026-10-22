//! Key-tap feedback sound.
//!
//! The output stream is opened once at startup and kept running for the life
//! of the app, and the tap sample is decoded up front into PCM already in the
//! stream's channel count and sample rate. Playing a tap is then just handing
//! a cheap (`Arc`-backed) clone of that buffer to the mixer — no device open,
//! decode, or resample on the press path. The mixer sums every active source,
//! so overlapping taps (multi-touch, fast typing) layer instead of queueing.

use std::io::Cursor;

use mecha_wayland::prelude::Resource;
use rodio::buffer::SamplesBuffer;
use rodio::cpal::BufferSize;
use rodio::source::UniformSourceIterator;
use rodio::{Decoder, DeviceSinkBuilder, MixerDeviceSink, Source};

static POP: &[u8] = include_bytes!("../resources/pop.ogg");

/// Output buffer size in frames (~10 ms at 48 kHz). Smaller than rodio's
/// ~50 ms default so a tap is heard promptly after the key is hit.
const BUFFER_FRAMES: u32 = 512;

/// The tap sound and the open output it plays through. `None` when no audio
/// device is available (or the sample fails to decode) — the keyboard then
/// simply stays silent.
pub struct KeySound(Option<Output>);

struct Output {
    /// Owns the device stream; playback stops when this is dropped.
    sink: MixerDeviceSink,
    pop: SamplesBuffer,
}

impl Resource for KeySound {}

impl KeySound {
    pub fn load() -> Self {
        match Output::open() {
            Ok(out) => Self(Some(out)),
            Err(err) => {
                tracing::warn!("key sound disabled: {err}");
                Self(None)
            }
        }
    }

    /// Start one tap sound, mixed over any still playing.
    pub fn play(&self) {
        if let Some(out) = &self.0 {
            out.sink.mixer().add(out.pop.clone());
        }
    }
}

impl Output {
    fn open() -> Result<Self, Box<dyn std::error::Error>> {
        let mut sink = DeviceSinkBuilder::from_default_device()
            .and_then(|b| {
                b.with_buffer_size(BufferSize::Fixed(BUFFER_FRAMES))
                    .open_stream()
            })
            .or_else(|err| {
                tracing::debug!("low-latency audio stream failed ({err}); using defaults");
                DeviceSinkBuilder::open_default_sink()
            })?;
        sink.log_on_drop(false);

        let config = sink.config();
        // Collect the decoder into a plain buffer first: converting the decoder
        // directly (`record()` / `UniformSourceIterator`) yields no samples for
        // this Vorbis stream. Then convert once to the stream's format.
        let decoder = Decoder::try_from(Cursor::new(POP))?;
        let raw = SamplesBuffer::new(
            decoder.channels(),
            decoder.sample_rate(),
            decoder.collect::<Vec<_>>(),
        );
        let pop = SamplesBuffer::new(
            config.channel_count(),
            config.sample_rate(),
            UniformSourceIterator::new(raw, config.channel_count(), config.sample_rate())
                .collect::<Vec<_>>(),
        );

        Ok(Self { sink, pop })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pop_decodes_to_audible_samples() {
        let Ok(out) = Output::open() else { return }; // no audio device (CI)
        assert!(out.pop.clone().any(|s| s.abs() > 0.01));
    }
}
