use thiserror::Error;

#[derive(Debug, Error)]
pub enum EncoderError {
    #[error("no encoder available")]
    NoEncoder,
    #[error("encoder initialization failed: {0}")]
    InitFailed(String),
    #[error("encode failed: {0}")]
    EncodeFailed(String),
}

/// Which AV1 encoder backend is in use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EncoderBackend {
    IntelQsv,
    NvidiaEnc,
    AmdAmf,
    AmdVaapi,
    SvtAv1,
}

impl EncoderBackend {
    /// FFmpeg encoder name for this backend.
    fn codec_name(&self) -> &'static str {
        match self {
            Self::IntelQsv => "av1_qsv",
            Self::NvidiaEnc => "av1_nvenc",
            Self::AmdAmf => "av1_amf",
            Self::AmdVaapi => "av1_vaapi",
            Self::SvtAv1 => "libsvtav1",
        }
    }
}

impl std::fmt::Display for EncoderBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::IntelQsv => write!(f, "Intel QSV (av1_qsv)"),
            Self::NvidiaEnc => write!(f, "NVIDIA NVENC (av1_nvenc)"),
            Self::AmdAmf => write!(f, "AMD AMF (av1_amf)"),
            Self::AmdVaapi => write!(f, "VA-API (av1_vaapi)"),
            Self::SvtAv1 => write!(f, "SVT-AV1 (libsvtav1, software)"),
        }
    }
}

#[derive(Debug, Clone)]
pub struct EncoderConfig {
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub bitrate_kbps: u32,
    pub keyframe_interval: u32,
    /// Override encoder preset. None = backend default.
    /// SVT-AV1: "0".."13", QSV: "veryfast".."veryslow", NVENC: "p1".."p7".
    pub preset: Option<String>,
    /// Override constant rate factor (SVT-AV1 only). None = 35.
    pub crf: Option<u8>,
    /// Extra svtav1-params to merge with the mandatory low-delay ones.
    /// Format: "key=value:key=value". SVT-AV1 only.
    pub extra_svt_params: Option<String>,
}

impl Default for EncoderConfig {
    fn default() -> Self {
        Self {
            width: 1920,
            height: 1080,
            fps: 30,
            bitrate_kbps: 5000,
            keyframe_interval: 60,
            preset: None,
            crf: None,
            extra_svt_params: None,
        }
    }
}

/// Encoded frame output.
pub struct EncodedFrame {
    pub data: Vec<u8>,
    pub keyframe: bool,
    pub timestamp_us: u64,
}

/// Trait for AV1 encoding backends.
pub trait Av1Encoder: Send {
    fn backend(&self) -> EncoderBackend;

    /// Encode an RGBA frame. Returns all available encoded packets
    /// (may be empty if encoder is still buffering, or multiple if draining).
    fn encode_frame(
        &mut self,
        rgba_data: &[u8],
        timestamp_us: u64,
        force_keyframe: bool,
    ) -> Result<Vec<EncodedFrame>, EncoderError>;

    fn flush(&mut self) -> Result<Vec<EncodedFrame>, EncoderError>;
}

/// Wrapper to make ffmpeg scaler Send-safe.
/// The scaler is only used from one thread (the capture thread).
struct SendScaler(ffmpeg_next::software::scaling::Context);
unsafe impl Send for SendScaler {}
impl std::ops::Deref for SendScaler {
    type Target = ffmpeg_next::software::scaling::Context;
    fn deref(&self) -> &Self::Target { &self.0 }
}
impl std::ops::DerefMut for SendScaler {
    fn deref_mut(&mut self) -> &mut Self::Target { &mut self.0 }
}

/// FFmpeg-based AV1 encoder. Works with any backend (HW or SW).
pub struct FfmpegAv1Encoder {
    backend: EncoderBackend,
    encoder: ffmpeg_next::encoder::Video,
    converter: SendScaler,
    pixel_fmt: ffmpeg_next::format::Pixel,
    frame_index: i64,
    config: EncoderConfig,
}

impl FfmpegAv1Encoder {
    fn new(backend: EncoderBackend, config: &EncoderConfig) -> Result<Self, EncoderError> {
        let codec_name = backend.codec_name();

        let codec = ffmpeg_next::encoder::find_by_name(codec_name)
            .ok_or_else(|| EncoderError::InitFailed(format!("codec '{codec_name}' not found")))?;

        let ctx = ffmpeg_next::codec::context::Context::new_with_codec(codec);
        let mut encoder = ctx.encoder().video().map_err(|e| {
            EncoderError::InitFailed(format!("create video encoder: {e}"))
        })?;

        // Hardware encoders need NV12, software uses YUV420P
        let is_hw = matches!(
            backend,
            EncoderBackend::IntelQsv | EncoderBackend::NvidiaEnc |
            EncoderBackend::AmdAmf | EncoderBackend::AmdVaapi
        );
        let pixel_fmt = if is_hw {
            ffmpeg_next::format::Pixel::NV12
        } else {
            ffmpeg_next::format::Pixel::YUV420P
        };

        encoder.set_width(config.width);
        encoder.set_height(config.height);
        encoder.set_format(pixel_fmt);
        encoder.set_time_base(ffmpeg_next::Rational::new(1, config.fps as i32));
        encoder.set_gop(config.keyframe_interval);

        // Backend-specific tuning
        let mut opts = ffmpeg_next::Dictionary::new();
        match backend {
            EncoderBackend::SvtAv1 => {
                // Preset: user override OR our default (10)
                let preset = config.preset.as_deref().unwrap_or("10");
                opts.set("preset", preset);

                // CRF: user override OR our default (35)
                let crf = config.crf.map(|c| c.to_string()).unwrap_or_else(|| "35".to_string());
                opts.set("crf", &crf);

                // svtav1-params: mandatory low-delay setting + any user extras.
                // `pred-struct=1` selects the low-delay B prediction structure,
                // which implies no lookahead. SVT-AV1 v3+ removed the `lad`
                // alias, so we rely on pred-struct alone for low-latency.
                let mut svt_params = String::from("pred-struct=1");
                if let Some(extra) = &config.extra_svt_params {
                    if !extra.is_empty() {
                        svt_params.push(':');
                        svt_params.push_str(extra);
                    }
                }
                opts.set("svtav1-params", &svt_params);
                encoder.set_max_b_frames(0);

                tracing::info!("SVT-AV1 tuning: preset={preset} crf={crf} svtav1-params={svt_params}");
            }
            EncoderBackend::IntelQsv => {
                encoder.set_bit_rate(config.bitrate_kbps as usize * 1000);
                let preset = config.preset.as_deref().unwrap_or("veryfast");
                opts.set("preset", preset);
                opts.set("look_ahead", "0");
                opts.set("async_depth", "1");
                opts.set("low_delay_brc", "1");
                encoder.set_max_b_frames(0);
                tracing::info!("QSV tuning: preset={preset}");
            }
            EncoderBackend::NvidiaEnc => {
                encoder.set_bit_rate(config.bitrate_kbps as usize * 1000);
                let preset = config.preset.as_deref().unwrap_or("p1");
                opts.set("preset", preset);
                opts.set("tune", "ull");
                opts.set("rc", "cbr");
                tracing::info!("NVENC tuning: preset={preset}");
            }
            EncoderBackend::AmdAmf | EncoderBackend::AmdVaapi => {
                encoder.set_bit_rate(config.bitrate_kbps as usize * 1000);
                if let Some(preset) = &config.preset {
                    opts.set("preset", preset);
                }
            }
        }

        let encoder = encoder.open_with(opts).map_err(|e| {
            EncoderError::InitFailed(format!("{backend}: open encoder: {e}"))
        })?;

        // Create RGBA -> target pixel format converter
        let converter = SendScaler(ffmpeg_next::software::scaling::Context::get(
            ffmpeg_next::format::Pixel::RGBA,
            config.width,
            config.height,
            pixel_fmt,
            config.width,
            config.height,
            ffmpeg_next::software::scaling::Flags::BILINEAR,
        )
        .map_err(|e| EncoderError::InitFailed(format!("create scaler: {e}")))?);

        tracing::info!("AV1 encoder initialized: {backend} @ {}x{} fmt={pixel_fmt:?}",
            config.width, config.height);

        Ok(Self {
            backend,
            encoder,
            converter,
            pixel_fmt,
            frame_index: 0,
            config: config.clone(),
        })
    }
}

impl Av1Encoder for FfmpegAv1Encoder {
    fn backend(&self) -> EncoderBackend {
        self.backend
    }

    fn encode_frame(
        &mut self,
        rgba_data: &[u8],
        timestamp_us: u64,
        force_keyframe: bool,
    ) -> Result<Vec<EncodedFrame>, EncoderError> {
        let w = self.config.width;
        let h = self.config.height;
        let expected_size = (w * h * 4) as usize;

        if rgba_data.len() != expected_size {
            return Err(EncoderError::EncodeFailed(format!(
                "expected {} bytes, got {}",
                expected_size,
                rgba_data.len()
            )));
        }

        // Create RGBA input frame
        let mut rgba_frame = ffmpeg_next::frame::Video::new(
            ffmpeg_next::format::Pixel::RGBA,
            w,
            h,
        );
        rgba_frame.data_mut(0)[..expected_size].copy_from_slice(rgba_data);

        // Convert RGBA -> target pixel format
        let mut yuv_frame = ffmpeg_next::frame::Video::new(
            self.pixel_fmt,
            w,
            h,
        );
        self.converter.run(&rgba_frame, &mut yuv_frame).map_err(|e| {
            EncoderError::EncodeFailed(format!("color convert: {e}"))
        })?;

        yuv_frame.set_pts(Some(self.frame_index));
        if force_keyframe {
            yuv_frame.set_kind(ffmpeg_next::picture::Type::I);
        }
        self.frame_index += 1;

        // Send frame to encoder
        self.encoder.send_frame(&yuv_frame).map_err(|e| {
            EncoderError::EncodeFailed(format!("send frame: {e}"))
        })?;

        // Drain all available encoded packets.
        let mut results = Vec::new();
        let mut packet = ffmpeg_next::Packet::empty();
        loop {
            match self.encoder.receive_packet(&mut packet) {
                Ok(()) => {
                    let data = packet.data().unwrap_or(&[]).to_vec();
                    if !data.is_empty() {
                        results.push(EncodedFrame {
                            data,
                            keyframe: packet.is_key(),
                            timestamp_us,
                        });
                    }
                }
                Err(ffmpeg_next::Error::Other { errno: libc::EAGAIN }) => break,
                Err(e) => return Err(EncoderError::EncodeFailed(format!("receive packet: {e}"))),
            }
        }

        Ok(results)
    }

    fn flush(&mut self) -> Result<Vec<EncodedFrame>, EncoderError> {
        self.encoder.send_eof().map_err(|e| {
            EncoderError::EncodeFailed(format!("send eof: {e}"))
        })?;

        let mut frames = Vec::new();
        let mut packet = ffmpeg_next::Packet::empty();
        while self.encoder.receive_packet(&mut packet).is_ok() {
            frames.push(EncodedFrame {
                data: packet.data().unwrap_or(&[]).to_vec(),
                keyframe: packet.is_key(),
                timestamp_us: 0,
            });
        }
        Ok(frames)
    }
}

/// Probe available AV1 encoders and return the best one.
/// Priority: QSV > NVENC > AMF > VA-API > SVT-AV1.
pub fn probe_best_encoder(config: &EncoderConfig) -> Result<Box<dyn Av1Encoder>, EncoderError> {
    ffmpeg_next::init().map_err(|e| EncoderError::InitFailed(format!("ffmpeg init: {e}")))?;

    let backends = [
        EncoderBackend::IntelQsv,
        EncoderBackend::NvidiaEnc,
        EncoderBackend::AmdAmf,
        EncoderBackend::AmdVaapi,
        EncoderBackend::SvtAv1,
    ];

    for backend in &backends {
        tracing::info!("Probing AV1 encoder: {backend}...");
        match FfmpegAv1Encoder::new(*backend, config) {
            Ok(enc) => {
                tracing::info!("Selected AV1 encoder: {backend}");
                return Ok(Box::new(enc));
            }
            Err(e) => {
                tracing::debug!("  {backend}: {e}");
            }
        }
    }

    Err(EncoderError::NoEncoder)
}
