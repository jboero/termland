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

    // Priority: hardware before software; within each tier, open-source codecs
    // first (AV1 → VP9 → VP8) then patent-encumbered (H.265 → H.264). This uses
    // whatever GPU is present (e.g. a Volta GV100's HEVC engine) before falling
    // back to a software encoder that would otherwise always "win" the probe.
    let backends = [
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

    for backend in &backends {
        // Skip codecs the client can't decode (empty `allowed` = no restriction).
        if !allowed.is_empty() && !allowed.contains(&backend.codec()) {
            continue;
        }
        tracing::info!("Probing video encoder: {backend}...");
        match FfmpegVideoEncoder::new(*backend, config) {
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