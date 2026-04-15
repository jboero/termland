use thiserror::Error;

#[derive(Debug, Error)]
pub enum DecoderError {
    #[error("decoder init failed: {0}")]
    InitFailed(String),
    #[error("decode failed: {0}")]
    DecodeFailed(String),
    #[error("no frame available")]
    NoFrame,
}

/// Which decoder backend is in use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecoderBackend {
    IntelQsv,
    NvidiaCuvid,
    Dav1d,
}

impl std::fmt::Display for DecoderBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::IntelQsv => write!(f, "Intel QSV (av1_qsv)"),
            Self::NvidiaCuvid => write!(f, "NVIDIA CUVID (av1_cuvid)"),
            Self::Dav1d => write!(f, "dav1d (software)"),
        }
    }
}

impl DecoderBackend {
    fn codec_name(&self) -> &'static str {
        match self {
            Self::IntelQsv => "av1_qsv",
            Self::NvidiaCuvid => "av1_cuvid",
            Self::Dav1d => "libdav1d",
        }
    }
}

/// Backends to try, in priority order.
const BACKEND_PRIORITY: &[DecoderBackend] = &[
    DecoderBackend::IntelQsv,
    DecoderBackend::NvidiaCuvid,
    DecoderBackend::Dav1d,
];

/// AV1 decoder using FFmpeg hardware decoders with automatic fallback.
///
/// Because FFmpeg decoders don't always fail cleanly at init time, we verify
/// the decoder works by decoding the first packet. If that fails, we fall
/// back to the next backend in the priority list.
pub struct Av1Decoder {
    backend: DecoderBackend,
    decoder: ffmpeg_next::decoder::Video,
    scaler: Option<SendScaler>,
    width: u32,
    height: u32,
    /// Index into BACKEND_PRIORITY for the currently-selected backend.
    backend_index: usize,
    /// Backends we've already tried and failed with.
    failed_backends: Vec<DecoderBackend>,
    /// Have we successfully decoded at least one frame? Once true, we trust
    /// this backend and won't fall back on transient errors.
    confirmed_working: bool,
}

struct SendScaler(ffmpeg_next::software::scaling::Context);
unsafe impl Send for SendScaler {}
impl std::ops::Deref for SendScaler {
    type Target = ffmpeg_next::software::scaling::Context;
    fn deref(&self) -> &Self::Target { &self.0 }
}
impl std::ops::DerefMut for SendScaler {
    fn deref_mut(&mut self) -> &mut ffmpeg_next::software::scaling::Context { &mut self.0 }
}

impl Av1Decoder {
    /// Create a new AV1 decoder, probing hardware first. The first backend
    /// that can be initialized is returned; if it later fails to decode,
    /// we'll transparently fall back to the next one.
    pub fn new() -> Result<Self, DecoderError> {
        ffmpeg_next::init().map_err(|e| DecoderError::InitFailed(format!("ffmpeg: {e}")))?;
        Self::init_from_index(0, Vec::new())
    }

    fn init_from_index(start: usize, failed: Vec<DecoderBackend>) -> Result<Self, DecoderError> {
        for (idx, backend) in BACKEND_PRIORITY.iter().enumerate().skip(start) {
            if failed.contains(backend) {
                continue;
            }
            tracing::info!("Probing AV1 decoder: {backend}...");
            match Self::open_codec(*backend) {
                Ok(decoder) => {
                    tracing::info!("Selected AV1 decoder: {backend}");
                    return Ok(Self {
                        backend: *backend,
                        decoder,
                        scaler: None,
                        width: 0,
                        height: 0,
                        backend_index: idx,
                        failed_backends: failed,
                        confirmed_working: false,
                    });
                }
                Err(e) => tracing::debug!("  {backend}: {e}"),
            }
        }
        Err(DecoderError::InitFailed("no AV1 decoder available".into()))
    }

    fn open_codec(backend: DecoderBackend) -> Result<ffmpeg_next::decoder::Video, DecoderError> {
        let codec = ffmpeg_next::decoder::find_by_name(backend.codec_name())
            .ok_or_else(|| DecoderError::InitFailed(format!("codec '{}' not found", backend.codec_name())))?;

        let ctx = ffmpeg_next::codec::context::Context::new_with_codec(codec);
        ctx.decoder().video()
            .map_err(|e| DecoderError::InitFailed(format!("{backend}: {e}")))
    }

    pub fn backend(&self) -> DecoderBackend {
        self.backend
    }

    /// Decode an AV1 packet. Returns (width, height, pixels).
    ///
    /// On repeated decode errors with an unconfirmed backend, automatically
    /// falls back to the next decoder in the priority list.
    pub fn decode(&mut self, data: &[u8]) -> Result<(u32, u32, Vec<u32>), DecoderError> {
        let packet = ffmpeg_next::Packet::copy(data);

        let send_result = self.decoder.send_packet(&packet);
        if let Err(e) = &send_result {
            if !self.confirmed_working {
                return self.fallback_and_retry(data, format!("send packet: {e}"));
            }
            return Err(DecoderError::DecodeFailed(format!("send packet: {e}")));
        }

        let mut frame = ffmpeg_next::frame::Video::empty();
        match self.decoder.receive_frame(&mut frame) {
            Ok(()) => {}
            Err(ffmpeg_next::Error::Other { errno: libc::EAGAIN }) => {
                return Err(DecoderError::NoFrame);
            }
            Err(e) => {
                if !self.confirmed_working {
                    return self.fallback_and_retry(data, format!("receive frame: {e}"));
                }
                return Err(DecoderError::DecodeFailed(format!("receive frame: {e}")));
            }
        }

        // Got a real frame - this backend works!
        if !self.confirmed_working {
            self.confirmed_working = true;
            tracing::info!("Decoder {} confirmed working", self.backend);
        }

        let w = frame.width();
        let h = frame.height();
        let fmt = frame.format();

        if self.width != w || self.height != h || self.scaler.is_none() {
            self.width = w;
            self.height = h;
            let scaler = ffmpeg_next::software::scaling::Context::get(
                fmt, w, h,
                ffmpeg_next::format::Pixel::RGBA, w, h,
                ffmpeg_next::software::scaling::Flags::BILINEAR
                    | ffmpeg_next::software::scaling::Flags::ACCURATE_RND
                    | ffmpeg_next::software::scaling::Flags::FULL_CHR_H_INT,
            ).map_err(|e| DecoderError::DecodeFailed(format!("create scaler: {e}")))?;
            self.scaler = Some(SendScaler(scaler));
        }

        let mut rgba_frame = ffmpeg_next::frame::Video::new(
            ffmpeg_next::format::Pixel::RGBA, w, h,
        );
        self.scaler.as_mut().unwrap().run(&frame, &mut rgba_frame)
            .map_err(|e| DecoderError::DecodeFailed(format!("scale: {e}")))?;

        let stride = rgba_frame.stride(0);
        let data = rgba_frame.data(0);
        let mut pixels = Vec::with_capacity((w * h) as usize);
        for row in 0..h as usize {
            let row_start = row * stride;
            for col in 0..w as usize {
                let i = row_start + col * 4;
                let r = data[i] as u32;
                let g = data[i + 1] as u32;
                let b = data[i + 2] as u32;
                pixels.push((r << 16) | (g << 8) | b);
            }
        }

        Ok((w, h, pixels))
    }

    /// Mark the current backend as failed and retry with the next one.
    fn fallback_and_retry(&mut self, data: &[u8], reason: String) -> Result<(u32, u32, Vec<u32>), DecoderError> {
        tracing::warn!("Decoder {} failed ({}), trying next backend", self.backend, reason);
        let mut failed = std::mem::take(&mut self.failed_backends);
        failed.push(self.backend);
        let next_index = self.backend_index + 1;
        let new_decoder = Self::init_from_index(next_index, failed)?;
        *self = new_decoder;
        self.decode(data)
    }
}
