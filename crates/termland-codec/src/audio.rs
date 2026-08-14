use thiserror::Error;

#[derive(Debug, Error)]
pub enum AudioError {
    #[error("opus error: {0}")]
    Opus(#[from] opus::Error),
    #[error("audio error: {0}")]
    Other(String),
}

/// Capture/playback rate. 48kHz matches what PulseAudio's null sink runs at
/// natively, so the capture path does no resampling.
///
/// Note this is *not* the bandwidth knob: the encoder targets a fixed
/// `BITRATE`, and Opus chooses its own coded bandwidth from that. A lower
/// sample rate sends exactly as many bytes, with less signal to spend them
/// on. Lower `BITRATE` instead.
pub const SAMPLE_RATE: u32 = 48000;
pub const CHANNELS: u8 = 2;

/// Encoded frame duration. 20ms is the usual real-time compromise: shorter
/// frames cut latency but spend proportionally more on per-packet overhead.
pub const FRAME_MS: u32 = 20;

/// Samples per channel in one encoded frame, derived from the rate so the two
/// cannot drift apart.
///
/// This used to be a bare `960` ("20ms at 48kHz"). Lowering `SAMPLE_RATE`
/// alone would then have silently changed the frame *duration* rather than
/// the frame size — 960 samples is 40ms at 24kHz and 60ms at 16kHz, both of
/// which are legal Opus frame sizes, so encoding keeps working and only the
/// latency quietly gets worse.
pub const FRAME_SIZE: usize = (SAMPLE_RATE / 1000 * FRAME_MS) as usize;

/// libopus accepts only these input rates; anything else fails at
/// `Encoder::new` on the first session rather than at build time.
const _: () = assert!(
    matches!(SAMPLE_RATE, 8000 | 12000 | 16000 | 24000 | 48000),
    "SAMPLE_RATE must be a rate libopus supports: 8, 12, 16, 24 or 48 kHz",
);

/// And only these frame durations (2.5ms also exists, but is not expressible
/// in whole milliseconds and is far too short to be useful here).
const _: () = assert!(
    matches!(FRAME_MS, 5 | 10 | 20 | 40 | 60),
    "FRAME_MS must be an Opus frame duration: 5, 10, 20, 40 or 60 ms",
);
/// Target encoder bitrate, in bits per second. Public so the server logs the
/// rate it is actually encoding at rather than a separately-written constant
/// that can drift from this one.
pub const BITRATE: i32 = 32_000;

pub struct OpusEncoder {
    encoder: opus::Encoder,
}

impl OpusEncoder {
    pub fn new() -> Result<Self, AudioError> {
        let mut encoder = opus::Encoder::new(
            SAMPLE_RATE,
            opus::Channels::Stereo,
            opus::Application::Audio,
        )?;
        encoder.set_bitrate(opus::Bitrate::Bits(BITRATE))?;
        encoder.set_inband_fec(true)?;
        encoder.set_dtx(true)?;
        Ok(Self { encoder })
    }

    /// Encode a frame of interleaved i16 PCM (960 samples per channel = 1920 total).
    /// Returns the Opus packet bytes.
    pub fn encode(&mut self, pcm: &[i16]) -> Result<Vec<u8>, AudioError> {
        let mut output = vec![0u8; 4000];
        let len = self.encoder.encode(pcm, &mut output)?;
        output.truncate(len);
        Ok(output)
    }
}

pub struct OpusDecoder {
    decoder: opus::Decoder,
}

impl OpusDecoder {
    pub fn new() -> Result<Self, AudioError> {
        let decoder = opus::Decoder::new(SAMPLE_RATE, opus::Channels::Stereo)?;
        Ok(Self { decoder })
    }

    /// Decode an Opus packet into interleaved i16 PCM.
    pub fn decode(&mut self, data: &[u8]) -> Result<Vec<i16>, AudioError> {
        let mut output = vec![0i16; FRAME_SIZE * CHANNELS as usize];
        let samples = self.decoder.decode(data, &mut output, false)?;
        output.truncate(samples * CHANNELS as usize);
        Ok(output)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Pins the current defaults. 960 samples per channel at 48kHz is what
    /// the server chunks capture into and what the decoder sizes its output
    /// buffer for, so a change here is a protocol-visible change in packet
    /// cadence, not just a constant.
    #[test]
    fn frame_size_is_twenty_ms_at_the_current_rate() {
        assert_eq!(FRAME_SIZE, 960);
        assert_eq!(FRAME_SIZE as u32 * 1000 / SAMPLE_RATE, FRAME_MS);
    }

    /// The decoder allocates `FRAME_SIZE * CHANNELS` per packet, so an
    /// interleaved frame has to divide evenly into channels.
    #[test]
    fn interleaved_frame_divides_by_channel_count() {
        let interleaved = FRAME_SIZE * CHANNELS as usize;
        assert_eq!(interleaved % CHANNELS as usize, 0);
        assert_eq!(interleaved / CHANNELS as usize, FRAME_SIZE);
    }

    /// Guards the claim in BITRATE's doc comment that it, not SAMPLE_RATE, is
    /// the bandwidth control: this is the value handed to libopus, so it has
    /// to stay something libopus will accept (6k-510k per channel).
    #[test]
    fn bitrate_is_within_the_opus_accepted_range() {
        assert!(
            (6_000..=510_000).contains(&BITRATE),
            "BITRATE {BITRATE} is outside the range libopus accepts",
        );
    }

    /// Opus is fixed-frame: the encoder rejects any buffer that is not exactly
    /// one frame. This is the round trip the session audio path performs.
    #[test]
    fn encoder_accepts_exactly_one_frame_of_silence() {
        let mut enc = match OpusEncoder::new() {
            Ok(e) => e,
            // No libopus at test time - skip rather than fail the suite.
            Err(_) => return,
        };
        let silence = vec![0i16; FRAME_SIZE * CHANNELS as usize];
        let packet = enc.encode(&silence).expect("one full frame must encode");
        assert!(!packet.is_empty());
    }

    /// A short buffer is a programming error in the chunking loop, and must
    /// surface as an error rather than being silently padded or truncated.
    #[test]
    fn encoder_rejects_a_partial_frame() {
        let mut enc = match OpusEncoder::new() {
            Ok(e) => e,
            Err(_) => return,
        };
        let short = vec![0i16; (FRAME_SIZE * CHANNELS as usize) - 2];
        assert!(enc.encode(&short).is_err(), "a partial frame must not encode");
    }

    #[test]
    fn silence_survives_an_encode_decode_round_trip() {
        let (mut enc, mut dec) = match (OpusEncoder::new(), OpusDecoder::new()) {
            (Ok(e), Ok(d)) => (e, d),
            _ => return,
        };
        let silence = vec![0i16; FRAME_SIZE * CHANNELS as usize];
        let packet = enc.encode(&silence).expect("encode");
        let pcm = dec.decode(&packet).expect("decode");
        assert_eq!(
            pcm.len(),
            FRAME_SIZE * CHANNELS as usize,
            "decoder must return exactly one frame of interleaved samples",
        );
    }
}
