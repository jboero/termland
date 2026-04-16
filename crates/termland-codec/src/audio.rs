use thiserror::Error;

#[derive(Debug, Error)]
pub enum AudioError {
    #[error("opus error: {0}")]
    Opus(#[from] opus::Error),
    #[error("audio error: {0}")]
    Other(String),
}

pub const SAMPLE_RATE: u32 = 48000;
pub const CHANNELS: u8 = 2;
/// Opus frame size: 20ms at 48kHz = 960 samples per channel.
pub const FRAME_SIZE: usize = 960;

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
        encoder.set_bitrate(opus::Bitrate::Bits(32000))?;
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
