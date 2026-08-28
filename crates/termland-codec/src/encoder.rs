use thiserror::Error;
use termland_protocol::VideoCodec;

#[derive(Debug, Error)]
pub enum EncoderError {
    #[error("no encoder available")]
    NoEncoder,
    #[error("encoder initialization failed: {0}")]
    InitFailed(String),
    #[error("encode failed: {0}")]
    EncodeFailed(String),
}

/// Which video encoder backend is in use.
/// Priority order (best to worst):
/// 1. AV1 (modern, efficient)
/// 2. VP9 (open source, good compression)
/// 3. VP8 (open source, widely supported)
/// 4. H.265/HEVC (patent-encumbered, good compression)
/// 5. H.264/AVC (patent-encumbered, universal support)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EncoderBackend {
    // AV1 backends
    IntelQsv,
    NvidiaEnc,
    AmdAmf,
    AmdVaapi,
    SvtAv1,
    
    // VP9 backends (open source priority)
    Vp9Vaapi,
    Vp9V4l2m2m,
    SvtVp9,
    
    // VP8 backends (open source priority)
    Vp8V4l2m2m,
    Vp8Libvpx,
    
    // H.265/HEVC backends
    H265Qsv,
    H265Nvenc,
    H265Amf,
    H265Vaapi,
    H265V4l2m2m,
    Libx265,
    
    // H.264 backends (last resort)
    H264Qsv,
    H264Nvenc,
    H264Amf,
    H264Vaapi,
    H264V4l2m2m,
    Libx264,
}

impl EncoderBackend {
    /// FFmpeg encoder name for this backend.
    fn codec_name(&self) -> &'static str {
        match self {
            // AV1
            EncoderBackend::IntelQsv => "av1_qsv",
            EncoderBackend::NvidiaEnc => "av1_nvenc",
            EncoderBackend::AmdAmf => "av1_amf",
            EncoderBackend::AmdVaapi => "av1_vaapi",
            EncoderBackend::SvtAv1 => "libsvtav1",
            
            // VP9
            EncoderBackend::Vp9Vaapi => "vp9_vaapi",
            EncoderBackend::Vp9V4l2m2m => "vp9_v4l2m2m",
            EncoderBackend::SvtVp9 => "libvpx-vp9",
            
            // VP8
            EncoderBackend::Vp8V4l2m2m => "vp8_v4l2m2m",
            EncoderBackend::Vp8Libvpx => "libvpx",
            
            // H.265/HEVC
            EncoderBackend::H265Qsv => "hevc_qsv",
            EncoderBackend::H265Nvenc => "hevc_nvenc",
            EncoderBackend::H265Amf => "hevc_amf",
            EncoderBackend::H265Vaapi => "hevc_vaapi",
            EncoderBackend::H265V4l2m2m => "hevc_v4l2m2m",
            EncoderBackend::Libx265 => "libx265",
            
            // H.264
            EncoderBackend::H264Qsv => "h264_qsv",
            EncoderBackend::H264Nvenc => "h264_nvenc",
            EncoderBackend::H264Amf => "h264_amf",
            EncoderBackend::H264Vaapi => "h264_vaapi",
            EncoderBackend::H264V4l2m2m => "h264_v4l2m2m",
            EncoderBackend::Libx264 => "libx264",
        }
    }

    /// The wire codec produced by this backend.
    pub fn codec(&self) -> VideoCodec {
        match self {
            EncoderBackend::IntelQsv
            | EncoderBackend::NvidiaEnc
            | EncoderBackend::AmdAmf
            | EncoderBackend::AmdVaapi
            | EncoderBackend::SvtAv1 => VideoCodec::Av1,

            EncoderBackend::Vp9Vaapi
            | EncoderBackend::Vp9V4l2m2m
            | EncoderBackend::SvtVp9 => VideoCodec::Vp9,

            EncoderBackend::Vp8V4l2m2m
            | EncoderBackend::Vp8Libvpx => VideoCodec::Vp8,

            EncoderBackend::H265Qsv
            | EncoderBackend::H265Nvenc
            | EncoderBackend::H265Amf
            | EncoderBackend::H265Vaapi
            | EncoderBackend::H265V4l2m2m
            | EncoderBackend::Libx265 => VideoCodec::H265,

            EncoderBackend::H264Qsv
            | EncoderBackend::H264Nvenc
            | EncoderBackend::H264Amf
            | EncoderBackend::H264Vaapi
            | EncoderBackend::H264V4l2m2m
            | EncoderBackend::Libx264 => VideoCodec::H264,
        }
    }
}

impl std::fmt::Display for EncoderBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            // AV1
            EncoderBackend::IntelQsv => write!(f, "Intel QSV (av1_qsv)"),
            EncoderBackend::NvidiaEnc => write!(f, "NVIDIA NVENC (av1_nvenc)"),
            EncoderBackend::AmdAmf => write!(f, "AMD AMF (av1_amf)"),
            EncoderBackend::AmdVaapi => write!(f, "VA-API (av1_vaapi)"),
            EncoderBackend::SvtAv1 => write!(f, "SVT-AV1 (libsvtav1, software)"),
            
            // VP9
            EncoderBackend::Vp9Vaapi => write!(f, "VA-API (vp9_vaapi)"),
            EncoderBackend::Vp9V4l2m2m => write!(f, "v4l2m2m (vp9_v4l2m2m)"),
            EncoderBackend::SvtVp9 => write!(f, "SVT-VP9 (libvpx-vp9, software)"),
            
            // VP8
            EncoderBackend::Vp8V4l2m2m => write!(f, "v4l2m2m (vp8_v4l2m2m)"),
            EncoderBackend::Vp8Libvpx => write!(f, "libvpx (vp8, software)"),
            
            // H.265
            EncoderBackend::H265Qsv => write!(f, "Intel QSV (hevc_qsv)"),
            EncoderBackend::H265Nvenc => write!(f, "NVIDIA NVENC (hevc_nvenc)"),
            EncoderBackend::H265Amf => write!(f, "AMD AMF (hevc_amf)"),
            EncoderBackend::H265Vaapi => write!(f, "VA-API (hevc_vaapi)"),
            EncoderBackend::H265V4l2m2m => write!(f, "v4l2m2m (hevc_v4l2m2m)"),
            EncoderBackend::Libx265 => write!(f, "libx265 (software)"),
            
            // H.264
            EncoderBackend::H264Qsv => write!(f, "Intel QSV (h264_qsv)"),
            EncoderBackend::H264Nvenc => write!(f, "NVIDIA NVENC (h264_nvenc)"),
            EncoderBackend::H264Amf => write!(f, "AMD AMF (h264_amf)"),
            EncoderBackend::H264Vaapi => write!(f, "VA-API (h264_vaapi)"),
            EncoderBackend::H264V4l2m2m => write!(f, "v4l2m2m (h264_v4l2m2m)"),
            EncoderBackend::Libx264 => write!(f, "libx264 (software)"),
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

/// Trait for video encoding backends.
pub trait VideoEncoder: Send {
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

/// FFmpeg-based encoder. Works with any backend (HW or SW).
pub struct FfmpegVideoEncoder {
    backend: EncoderBackend,
    encoder: ffmpeg_next::encoder::Video,
    converter: SendScaler,
    pixel_fmt: ffmpeg_next::format::Pixel,
    frame_index: i64,
    config: EncoderConfig,
}

impl FfmpegVideoEncoder {
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
            EncoderBackend::IntelQsv
            | EncoderBackend::NvidiaEnc
            | EncoderBackend::AmdAmf
            | EncoderBackend::AmdVaapi
            | EncoderBackend::Vp9Vaapi
            | EncoderBackend::Vp8V4l2m2m
            | EncoderBackend::H265Qsv
            | EncoderBackend::H265Nvenc
            | EncoderBackend::H265Amf
            | EncoderBackend::H265Vaapi
            | EncoderBackend::H265V4l2m2m
            | EncoderBackend::H264Qsv
            | EncoderBackend::H264Nvenc
            | EncoderBackend::H264Amf
            | EncoderBackend::H264Vaapi
            | EncoderBackend::H264V4l2m2m
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
            // AV1 tuning
            EncoderBackend::SvtAv1 => {
                let preset = config.preset.as_deref().unwrap_or("10");
                opts.set("preset", preset);

                let crf = config.crf.map(|c| c.to_string()).unwrap_or_else(|| "35".to_string());
                opts.set("crf", &crf);

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

            // VP9 tuning (SVT-VP9)
            EncoderBackend::SvtVp9 => {
                let preset = config.preset.as_deref().unwrap_or("8");
                opts.set("preset", preset);

                let crf = config.crf.map(|c| c.to_string()).unwrap_or_else(|| "35".to_string());
                opts.set("crf", &crf);
                encoder.set_max_b_frames(0);

                tracing::info!("SVT-VP9 tuning: preset={preset} crf={crf}");
            }

            // H.265/HEVC tuning
            EncoderBackend::H265Qsv => {
                encoder.set_bit_rate(config.bitrate_kbps as usize * 1000);
                let preset = config.preset.as_deref().unwrap_or("veryfast");
                opts.set("preset", preset);
                opts.set("look_ahead", "0");
                opts.set("async_depth", "1");
                encoder.set_max_b_frames(0);
                tracing::info!("QSV HEVC tuning: preset={preset}");
            }
            EncoderBackend::H265Nvenc => {
                encoder.set_bit_rate(config.bitrate_kbps as usize * 1000);
                let preset = config.preset.as_deref().unwrap_or("p1");
                opts.set("preset", preset);
                opts.set("tune", "ull");
                opts.set("rc", "cbr");
                tracing::info!("NVENC HEVC tuning: preset={preset}");
            }
            EncoderBackend::H265Amf | EncoderBackend::H265Vaapi | EncoderBackend::H265V4l2m2m => {
                encoder.set_bit_rate(config.bitrate_kbps as usize * 1000);
                if let Some(preset) = &config.preset {
                    opts.set("preset", preset);
                }
            }
            EncoderBackend::Libx265 => {
                encoder.set_bit_rate(config.bitrate_kbps as usize * 1000);
                let preset = config.preset.as_deref().unwrap_or("fast");
                opts.set("preset", preset);
                let crf = config.crf.map(|c| c.to_string()).unwrap_or_else(|| "23".to_string());
                opts.set("crf", &crf);
                tracing::info!("libx265 tuning: preset={preset} crf={crf}");
            }

            // H.264 tuning
            EncoderBackend::H264Qsv => {
                encoder.set_bit_rate(config.bitrate_kbps as usize * 1000);
                let preset = config.preset.as_deref().unwrap_or("veryfast");
                opts.set("preset", preset);
                opts.set("look_ahead", "0");
                opts.set("async_depth", "1");
                encoder.set_max_b_frames(0);
                tracing::info!("QSV H.264 tuning: preset={preset}");
            }
            EncoderBackend::H264Nvenc => {
                encoder.set_bit_rate(config.bitrate_kbps as usize * 1000);
                let preset = config.preset.as_deref().unwrap_or("p1");
                opts.set("preset", preset);
                opts.set("tune", "ull");
                opts.set("rc", "cbr");
                tracing::info!("NVENC H.264 tuning: preset={preset}");
            }
            EncoderBackend::H264Amf | EncoderBackend::H264Vaapi | EncoderBackend::H264V4l2m2m => {
                encoder.set_bit_rate(config.bitrate_kbps as usize * 1000);
                if let Some(preset) = &config.preset {
                    opts.set("preset", preset);
                }
            }
            EncoderBackend::Libx264 => {
                encoder.set_bit_rate(config.bitrate_kbps as usize * 1000);
                let preset = config.preset.as_deref().unwrap_or("fast");
                opts.set("preset", preset);
                let crf = config.crf.map(|c| c.to_string()).unwrap_or_else(|| "23".to_string());
                opts.set("crf", &crf);
                tracing::info!("libx264 tuning: preset={preset} crf={crf}");
            }

            // VP8 and other backends with generic settings
            _ => {
                encoder.set_bit_rate(config.bitrate_kbps as usize * 1000);
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

        tracing::info!("Video encoder initialized: {backend} @ {}x{} fmt={pixel_fmt:?}",
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

impl VideoEncoder for FfmpegVideoEncoder {
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
        let dst_stride = rgba_frame.stride(0);
        let src_stride = (w * 4) as usize;
        let dst = rgba_frame.data_mut(0);
        for row in 0..h as usize {
            let src_start = row * src_stride;
            let dst_start = row * dst_stride;
            dst[dst_start..dst_start + src_stride]
                .copy_from_slice(&rgba_data[src_start..src_start + src_stride]);
        }

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
        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(500);
        loop {
            match self.encoder.receive_packet(&mut packet) {
                Ok(()) => {
                    let data = packet.data().unwrap_or(&[]).to_vec();
                    if !data.is_empty() {
                        frames.push(EncodedFrame {
                            data,
                            keyframe: packet.is_key(),
                            timestamp_us: 0,
                        });
                    }
                }
                Err(ffmpeg_next::Error::Eof) => break,
                Err(ffmpeg_next::Error::Other { errno: libc::EAGAIN }) => {
                    if std::time::Instant::now() >= deadline {
                        tracing::warn!("flush: EAGAIN deadline exceeded, giving up");
                        break;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(2));
                }
                Err(e) => {
                    tracing::warn!("flush: receive_packet error: {e}");
                    break;
                }
            }
        }
        Ok(frames)
    }
}

/// Encoders to try, in priority order.
///
/// Hardware before software; within each tier, open-source codecs first
/// (AV1 → VP9 → VP8) then patent-encumbered (H.265 → H.264). The tier split
/// matters: it uses whatever GPU is present (e.g. a Volta GV100's HEVC
/// engine) before falling back to a software encoder, which would otherwise
/// always "win" the probe by virtue of always opening successfully.
///
/// Every `EncoderBackend` must appear here exactly once — a variant missing
/// from this list is silently never probed. `tests::every_backend_is_probed`
/// enforces that.
const ENCODER_PRIORITY: &[EncoderBackend] = &[
    // --- Hardware encoders (open-source codecs first) ---
    // AV1 HW
    EncoderBackend::IntelQsv,
    EncoderBackend::NvidiaEnc,
    EncoderBackend::AmdAmf,
    EncoderBackend::AmdVaapi,
    // VP9 HW
    EncoderBackend::Vp9Vaapi,
    EncoderBackend::Vp9V4l2m2m,
    // VP8 HW
    EncoderBackend::Vp8V4l2m2m,
    // H.265/HEVC HW (patent-encumbered)
    EncoderBackend::H265Qsv,
    EncoderBackend::H265Nvenc,
    EncoderBackend::H265Amf,
    EncoderBackend::H265Vaapi,
    EncoderBackend::H265V4l2m2m,
    // H.264 HW (patent-encumbered, universal)
    EncoderBackend::H264Qsv,
    EncoderBackend::H264Nvenc,
    EncoderBackend::H264Amf,
    EncoderBackend::H264Vaapi,
    EncoderBackend::H264V4l2m2m,

    // --- Software encoders (open-source codecs first) ---
    EncoderBackend::SvtAv1,
    EncoderBackend::SvtVp9,   // libvpx-vp9
    EncoderBackend::Vp8Libvpx, // libvpx
    EncoderBackend::Libx265,
    EncoderBackend::Libx264,
];

/// The backends `probe_best_encoder` will try, in order, for a client that
/// advertised `allowed`. An empty slice means "no restriction" — used for
/// older clients that don't announce codec support at all.
///
/// Split out from the probe loop so codec negotiation can be tested without
/// an FFmpeg context or a GPU.
fn candidates(allowed: &[VideoCodec]) -> impl Iterator<Item = EncoderBackend> + '_ {
    ENCODER_PRIORITY
        .iter()
        .copied()
        .filter(move |b| allowed.is_empty() || allowed.contains(&b.codec()))
}

/// Probe available video encoders and return the best one.
/// Priority (open-source first, then patent-encumbered):
/// 1. AV1 (hardware) → AV1 (SVT-AV1 software)
/// 2. VP9 (hardware) → VP9 (SVT-VP9 software)
/// 3. VP8 (hardware) → VP8 (libvpx software)
/// 4. H.265/HEVC (patent-encumbered, last resort)
/// 5. H.264/AVC (patent-encumbered, universal, last of last)
///
/// `allowed` restricts the search to codecs the client advertised it can
/// decode (codec negotiation). An empty slice means "no restriction".
pub fn probe_best_encoder(
    config: &EncoderConfig,
    allowed: &[VideoCodec],
) -> Result<Box<dyn VideoEncoder>, EncoderError> {
    ffmpeg_next::init().map_err(|e| EncoderError::InitFailed(format!("ffmpeg init: {e}")))?;

    for backend in candidates(allowed) {
        tracing::info!("Probing video encoder: {backend}...");
        match FfmpegVideoEncoder::new(backend, config) {
            Ok(enc) => {
                tracing::info!("Selected video encoder: {backend} (codec {})", backend.codec());
                return Ok(Box::new(enc));
            }
            Err(e) => {
                tracing::debug!("  {backend}: {e}");
            }
        }
    }

    Err(EncoderError::NoEncoder)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every `EncoderBackend`, split by whether it runs on the GPU.
    ///
    /// These are hand-maintained, but `all_backends_are_accounted_for` pins
    /// the total, so adding a variant without listing it here fails loudly
    /// rather than silently weakening every test below.
    const HARDWARE: &[EncoderBackend] = &[
        EncoderBackend::IntelQsv,
        EncoderBackend::NvidiaEnc,
        EncoderBackend::AmdAmf,
        EncoderBackend::AmdVaapi,
        EncoderBackend::Vp9Vaapi,
        EncoderBackend::Vp9V4l2m2m,
        EncoderBackend::Vp8V4l2m2m,
        EncoderBackend::H265Qsv,
        EncoderBackend::H265Nvenc,
        EncoderBackend::H265Amf,
        EncoderBackend::H265Vaapi,
        EncoderBackend::H265V4l2m2m,
        EncoderBackend::H264Qsv,
        EncoderBackend::H264Nvenc,
        EncoderBackend::H264Amf,
        EncoderBackend::H264Vaapi,
        EncoderBackend::H264V4l2m2m,
    ];

    const SOFTWARE: &[EncoderBackend] = &[
        EncoderBackend::SvtAv1,
        EncoderBackend::SvtVp9,
        EncoderBackend::Vp8Libvpx,
        EncoderBackend::Libx265,
        EncoderBackend::Libx264,
    ];

    fn all() -> Vec<EncoderBackend> {
        HARDWARE.iter().chain(SOFTWARE).copied().collect()
    }

    /// Guards the two lists above against enum drift. `ENCODER_PRIORITY` is
    /// the one place every variant is already required to appear, so its
    /// length is the reference count.
    #[test]
    fn all_backends_are_accounted_for() {
        assert_eq!(
            all().len(),
            ENCODER_PRIORITY.len(),
            "HARDWARE + SOFTWARE in this test module have drifted from \
             ENCODER_PRIORITY — a backend was probably added to the enum \
             without being classified here",
        );
    }

    /// A backend missing from `ENCODER_PRIORITY` compiles fine and is simply
    /// never probed, so the feature silently does not exist.
    #[test]
    fn every_backend_is_probed() {
        for backend in all() {
            assert!(
                ENCODER_PRIORITY.contains(&backend),
                "{backend:?} is not in ENCODER_PRIORITY, so it will never be probed",
            );
        }
    }

    #[test]
    fn priority_list_has_no_duplicates() {
        let mut seen = Vec::new();
        for backend in ENCODER_PRIORITY {
            assert!(!seen.contains(backend), "{backend:?} appears twice in ENCODER_PRIORITY");
            seen.push(*backend);
        }
    }

    /// The FFmpeg encoder name and the declared wire codec are written out in
    /// two separate 22-arm matches. The compiler catches a *missing* arm; only
    /// a test catches a *wrong* one, e.g. `Vp9Vaapi => "vp8_vaapi"`, which
    /// would hand the client a stream in a codec it did not negotiate.
    ///
    /// This checks codec *family* agreement only. A same-family slip such as
    /// `H265Nvenc => "hevc_qsv"` passes here and is caught by
    /// `display_names_the_ffmpeg_encoder` instead, since the two together pin
    /// the exact string.
    #[test]
    fn ffmpeg_name_agrees_with_declared_codec() {
        for backend in all() {
            let name = backend.codec_name();
            let ok = match backend.codec() {
                VideoCodec::Av1 => name.contains("av1"),
                VideoCodec::Vp9 => name.contains("vp9"),
                // libvpx (no suffix) is VP8; libvpx-vp9 is caught above.
                VideoCodec::Vp8 => name.contains("vp8") || name == "libvpx",
                VideoCodec::H265 => name.contains("hevc") || name.contains("265"),
                VideoCodec::H264 => name.contains("h264") || name.contains("264"),
            };
            assert!(
                ok,
                "{backend:?} declares codec {} but its FFmpeg encoder is {name:?}",
                backend.codec(),
            );
        }
    }

    /// Every backend's `Display` embeds its FFmpeg name in parentheses; that
    /// string is what lands in the server log when a session picks an encoder,
    /// so drift between the two makes logs actively misleading when debugging
    /// a user's "wrong codec" report.
    #[test]
    fn display_names_the_ffmpeg_encoder() {
        for backend in all() {
            let shown = backend.to_string();
            assert!(
                shown.contains(backend.codec_name()),
                "Display for {backend:?} is {shown:?}, which does not mention {:?}",
                backend.codec_name(),
            );
        }
    }

    /// Software encoders always open successfully, so if one were ordered
    /// before a hardware backend it would win every probe and silently
    /// disable GPU encoding everywhere.
    #[test]
    fn hardware_is_probed_before_software() {
        let last_hw = ENCODER_PRIORITY.iter().rposition(|b| HARDWARE.contains(b)).unwrap();
        let first_sw = ENCODER_PRIORITY.iter().position(|b| SOFTWARE.contains(b)).unwrap();
        assert!(
            last_hw < first_sw,
            "software encoder at index {first_sw} precedes hardware encoder at {last_hw}; \
             software always opens, so it would win every probe",
        );
    }

    /// Within each tier the codec order must match `VideoCodec::all_preferred`
    /// — open-source (AV1 → VP9 → VP8) ahead of patent-encumbered
    /// (H.265 → H.264). That ordering is a licensing decision, not an
    /// incidental one.
    #[test]
    fn each_tier_follows_the_documented_codec_preference() {
        let rank = |c: VideoCodec| {
            VideoCodec::all_preferred().iter().position(|p| *p == c).unwrap()
        };
        for (tier, members) in [("hardware", HARDWARE), ("software", SOFTWARE)] {
            let ranks: Vec<_> = ENCODER_PRIORITY
                .iter()
                .filter(|b| members.contains(b))
                .map(|b| rank(b.codec()))
                .collect();
            let mut sorted = ranks.clone();
            sorted.sort_unstable();
            assert_eq!(
                ranks, sorted,
                "{tier} tier departs from VideoCodec::all_preferred order",
            );
        }
    }

    /// Every codec the protocol can negotiate needs at least one encoder,
    /// otherwise a client advertising only that codec gets `NoEncoder`.
    #[test]
    fn every_codec_has_an_encoder() {
        for codec in VideoCodec::all_preferred() {
            assert!(
                ENCODER_PRIORITY.iter().any(|b| b.codec() == codec),
                "no encoder backend produces {codec}",
            );
        }
    }

    #[test]
    fn empty_negotiation_set_allows_everything() {
        assert_eq!(candidates(&[]).count(), ENCODER_PRIORITY.len());
    }

    /// The negotiation filter must never hand back an encoder the client
    /// cannot decode — that produces a session that connects and then shows
    /// a black screen.
    #[test]
    fn negotiation_yields_only_allowed_codecs() {
        let allowed = [VideoCodec::Vp9, VideoCodec::H264];
        let picked: Vec<_> = candidates(&allowed).collect();

        assert!(!picked.is_empty());
        for backend in &picked {
            assert!(
                allowed.contains(&backend.codec()),
                "{backend:?} produces {}, which the client did not advertise",
                backend.codec(),
            );
        }
    }

    /// Filtering must not reshuffle the list: a client advertising everything
    /// has to get exactly the unrestricted order back.
    #[test]
    fn negotiation_preserves_priority_order() {
        let all_codecs = VideoCodec::all_preferred();
        let picked: Vec<_> = candidates(&all_codecs).collect();
        assert_eq!(picked, ENCODER_PRIORITY.to_vec());
    }

    /// A client that advertises a single codec must still get that codec's
    /// full hardware-then-software fallback chain, not just one backend.
    #[test]
    fn single_codec_negotiation_keeps_its_fallback_chain() {
        for codec in VideoCodec::all_preferred() {
            let picked: Vec<_> = candidates(&[codec]).collect();
            assert!(
                picked.iter().all(|b| b.codec() == codec),
                "{codec} negotiation leaked another codec",
            );
            assert!(
                picked.len() > 1,
                "{codec} has only {} backend(s), so a failure has no fallback",
                picked.len(),
            );
        }
    }

    /// FFmpeg packets are an elementary stream (AV1 OBUs / Annex-B), which is
    /// what WebCodecs `EncodedVideoChunk` expects — not IVF/MP4.
    ///
    /// `force_keyframe` is left false: SVT-AV1 rejects "Force key frame"
    /// unless opened in RA CRF/CQP mode. `keyframe_interval: 1` still
    /// produces a GOP-start keyframe.
    #[test]
    fn encoded_packet_is_an_elementary_stream_not_a_container() {
        let config = EncoderConfig {
            width: 320,
            height: 240,
            fps: 30,
            bitrate_kbps: 200,
            keyframe_interval: 1,
            ..Default::default()
        };
        let Ok(mut encoder) = probe_best_encoder(&config, &VideoCodec::all_preferred()) else {
            eprintln!("skipping: no video encoder in this environment");
            return;
        };
        let codec = encoder.backend().codec();
        let rgba = vec![80u8; 320 * 240 * 4];
        let mut packet = None;
        let mut last_err = None;
        for i in 0..12 {
            match encoder.encode_frame(&rgba, i * 33_000, false) {
                Ok(frames) => {
                    if let Some(f) = frames.into_iter().find(|f| !f.data.is_empty()) {
                        packet = Some(f.data);
                        break;
                    }
                }
                Err(e) => last_err = Some(e),
            }
        }
        if packet.is_none() {
            match encoder.flush() {
                Ok(frames) => {
                    packet = frames.into_iter().find(|f| !f.data.is_empty()).map(|f| f.data);
                }
                Err(e) => last_err = Some(e),
            }
        }
        let data = packet.unwrap_or_else(|| {
            panic!("{codec:?} encoder produced no packet: {last_err:?}");
        });
        assert!(data.len() > 2, "packet too small to inspect");
        let fourcc = data.get(0..4);
        assert_ne!(fourcc, Some(&b"DKIF"[..]), "packet is IVF, not an elementary stream");
        assert_ne!(fourcc, Some(&b"ftyp"[..]), "packet is MP4, not an elementary stream");
        assert!(
            termland_protocol::bitstream_matches_codec(codec, &data),
            "{codec:?} packet prefix {:02x?} is not the elementary stream WebCodecs expects",
            &data[..data.len().min(8)],
        );
    }
}