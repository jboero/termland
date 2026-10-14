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
    /// Encodes through VA-API, from frames uploaded to the GPU.
    fn is_vaapi(&self) -> bool {
        matches!(
            self,
            EncoderBackend::AmdVaapi
                | EncoderBackend::Vp9Vaapi
                | EncoderBackend::H265Vaapi
                | EncoderBackend::H264Vaapi
        )
    }

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

/// How much the encoder may spend on the video.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RateTarget {
    /// The session's quality, 1-100. Each backend turns it into its own rate
    /// control: a bitrate scaled for the codec, or for SVT-AV1 a CRF.
    Quality(u8),
    /// A bitrate the client asked for explicitly, in kbps.
    Kbps(u32),
}

impl RateTarget {
    /// The bitrate for a backend whose rate control is bitrate-based.
    ///
    /// AV1 needs far less than the older codecs for the same picture, and a
    /// remote desktop is mostly still images, so quality 100 is 1.5 Mbps for
    /// AV1 against 15 Mbps for the rest.
    pub fn kbps(self, codec: VideoCodec) -> u32 {
        match self {
            RateTarget::Kbps(kbps) => kbps,
            RateTarget::Quality(q) => {
                let q = u32::from(q.clamp(1, 100));
                match codec {
                    VideoCodec::Av1 => (q * 15).max(MIN_AV1_KBPS),
                    _ => (q * 150).max(500),
                }
            }
        }
    }
}

/// Lowest bitrate a quality setting maps to for AV1.
const MIN_AV1_KBPS: u32 = 50;

/// A CRF for a quality, on an encoder's own scale: `best` at quality 100 to
/// `worst` at 1. The scales below put quality 75, the default, at the CRF
/// each encoder used before quality applied to it (35 for SVT-AV1 and
/// libvpx-vp9, 23 for x264/x265).
fn crf_for_quality(quality: u8, best: u8, worst: u8) -> u8 {
    let q = f64::from(quality.clamp(1, 100));
    (f64::from(worst) - (q - 1.0) * f64::from(worst - best) / 99.0).round() as u8
}

/// Rate control for an encoder that rates by CRF (the software ones): an
/// explicit CRF override, else a CRF from the quality, else, for an explicit
/// bitrate, constant bitrate. A CRF encoder ignores a plain bitrate, which is
/// why quality used to have no effect on these. Returns a description to log.
fn crf_rate_control(
    config: &EncoderConfig,
    encoder: &mut ffmpeg_next::codec::encoder::video::Video,
    opts: &mut ffmpeg_next::Dictionary,
    best: u8,
    worst: u8,
) -> String {
    match (config.crf, config.rate) {
        (Some(crf), _) => {
            opts.set("crf", &crf.to_string());
            format!("crf={crf}")
        }
        (None, RateTarget::Quality(q)) => {
            let crf = crf_for_quality(q, best, worst);
            opts.set("crf", &crf.to_string());
            format!("crf={crf} (quality {q})")
        }
        (None, RateTarget::Kbps(k)) => {
            // maxrate == bitrate is constant bitrate; x264/x265 need a buffer
            // size for maxrate to apply. One second's worth.
            encoder.set_bit_rate(k as usize * 1000);
            opts.set("maxrate", &format!("{k}k"));
            opts.set("bufsize", &format!("{k}k"));
            format!("cbr {k} kbps")
        }
    }
}

#[derive(Debug, Clone)]
pub struct EncoderConfig {
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub rate: RateTarget,
    pub keyframe_interval: u32,
    /// Override encoder preset. None = backend default.
    /// SVT-AV1: "0".."13", QSV: "veryfast".."veryslow", NVENC: "p1".."p7".
    pub preset: Option<String>,
    /// Override constant rate factor (SVT-AV1 only), taking precedence over
    /// `rate`. None = derived from `rate`.
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
            rate: RateTarget::Quality(75),
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
/// The GPU frame pool a VA-API encoder encodes from. FFmpeg's VA-API
/// encoders accept only VA surfaces (AV_PIX_FMT_VAAPI), never frames in
/// system memory, so each converted NV12 frame is uploaded into one.
struct VaapiFrames {
    device: *mut ffmpeg_next::ffi::AVBufferRef,
    frames: *mut ffmpeg_next::ffi::AVBufferRef,
}

// The buffers are reference-counted FFmpeg objects owned by one encoder and
// only used from whichever thread holds that encoder.
unsafe impl Send for VaapiFrames {}

impl VaapiFrames {
    fn new(width: u32, height: u32) -> Result<Self, String> {
        use ffmpeg_next::ffi::*;
        unsafe {
            // The default VA display: the first DRM render node.
            let mut device = std::ptr::null_mut();
            let err = av_hwdevice_ctx_create(
                &mut device,
                AVHWDeviceType::AV_HWDEVICE_TYPE_VAAPI,
                std::ptr::null(),
                std::ptr::null_mut(),
                0,
            );
            if err < 0 {
                return Err(format!("no VA-API device ({})", ffmpeg_next::Error::from(err)));
            }
            let mut frames = av_hwframe_ctx_alloc(device);
            if frames.is_null() {
                av_buffer_unref(&mut device);
                return Err("allocating VA-API frames".into());
            }
            let ctx = (*frames).data as *mut AVHWFramesContext;
            (*ctx).format = AVPixelFormat::AV_PIX_FMT_VAAPI;
            (*ctx).sw_format = AVPixelFormat::AV_PIX_FMT_NV12;
            (*ctx).width = width as i32;
            (*ctx).height = height as i32;
            (*ctx).initial_pool_size = 8;
            let err = av_hwframe_ctx_init(frames);
            if err < 0 {
                av_buffer_unref(&mut frames);
                av_buffer_unref(&mut device);
                return Err(format!("VA-API frames ({})", ffmpeg_next::Error::from(err)));
            }
            Ok(Self { device, frames })
        }
    }

    /// Upload a frame in system memory (NV12) to a VA surface from the pool,
    /// carrying over its timestamp and picture type.
    fn upload(&self, frame: &ffmpeg_next::frame::Video) -> Result<ffmpeg_next::frame::Video, String> {
        use ffmpeg_next::ffi::*;
        let mut surface = ffmpeg_next::frame::Video::empty();
        unsafe {
            let dst = surface.as_mut_ptr();
            let err = av_hwframe_get_buffer(self.frames, dst, 0);
            if err < 0 {
                return Err(format!("VA surface ({})", ffmpeg_next::Error::from(err)));
            }
            let err = av_hwframe_transfer_data(dst, frame.as_ptr(), 0);
            if err < 0 {
                return Err(format!("upload to VA surface ({})", ffmpeg_next::Error::from(err)));
            }
            (*dst).pts = (*frame.as_ptr()).pts;
            (*dst).pict_type = (*frame.as_ptr()).pict_type;
        }
        Ok(surface)
    }
}

impl Drop for VaapiFrames {
    fn drop(&mut self) {
        unsafe {
            ffmpeg_next::ffi::av_buffer_unref(&mut self.frames);
            ffmpeg_next::ffi::av_buffer_unref(&mut self.device);
        }
    }
}

pub struct FfmpegVideoEncoder {
    backend: EncoderBackend,
    encoder: ffmpeg_next::encoder::Video,
    converter: SendScaler,
    pixel_fmt: ffmpeg_next::format::Pixel,
    frame_index: i64,
    config: EncoderConfig,
    // Declared after `encoder`, so the encoder is dropped (and lets go of its
    // own reference to the frame pool) first.
    vaapi: Option<VaapiFrames>,
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

        // A VA-API encoder encodes from VA surfaces: give it a pool of them,
        // into which encode_frame uploads each converted NV12 frame.
        let vaapi = if backend.is_vaapi() {
            let frames = VaapiFrames::new(config.width, config.height)
                .map_err(|e| EncoderError::InitFailed(format!("{backend}: {e}")))?;
            unsafe {
                let ctx = encoder.as_mut_ptr();
                (*ctx).pix_fmt = ffmpeg_next::ffi::AVPixelFormat::AV_PIX_FMT_VAAPI;
                (*ctx).hw_frames_ctx = ffmpeg_next::ffi::av_buffer_ref(frames.frames);
            }
            Some(frames)
        } else {
            None
        };

        // For the backends with bitrate-based rate control.
        let kbps = config.rate.kbps(backend.codec());

        // Backend-specific tuning
        let mut opts = ffmpeg_next::Dictionary::new();
        match backend {
            // AV1 tuning
            EncoderBackend::SvtAv1 => {
                let preset = config.preset.as_deref().unwrap_or("10");
                opts.set("preset", preset);

                let rc = crf_rate_control(config, &mut encoder, &mut opts, 25, 63);

                let mut svt_params = String::from("pred-struct=1");
                if let Some(extra) = &config.extra_svt_params {
                    if !extra.is_empty() {
                        svt_params.push(':');
                        svt_params.push_str(extra);
                    }
                }
                opts.set("svtav1-params", &svt_params);
                encoder.set_max_b_frames(0);

                tracing::info!("SVT-AV1 tuning: preset={preset} {rc} svtav1-params={svt_params}");
            }
            EncoderBackend::IntelQsv => {
                encoder.set_bit_rate(kbps as usize * 1000);
                let preset = config.preset.as_deref().unwrap_or("veryfast");
                opts.set("preset", preset);
                opts.set("look_ahead", "0");
                opts.set("async_depth", "1");
                // As for NVENC: otherwise a forced keyframe is not an IDR frame.
                opts.set("forced_idr", "1");
                opts.set("low_delay_brc", "1");
                encoder.set_max_b_frames(0);
                tracing::info!("QSV tuning: preset={preset}");
            }
            EncoderBackend::NvidiaEnc => {
                encoder.set_bit_rate(kbps as usize * 1000);
                let preset = config.preset.as_deref().unwrap_or("p1");
                opts.set("preset", preset);
                opts.set("tune", "ull");
                opts.set("rc", "cbr");
                // Without this a forced keyframe comes out as a plain intra
                // frame, which a decoder cannot start from.
                opts.set("forced-idr", "1");
                tracing::info!("NVENC tuning: preset={preset}");
            }
            EncoderBackend::AmdAmf | EncoderBackend::AmdVaapi => {
                encoder.set_bit_rate(kbps as usize * 1000);
                if let Some(preset) = &config.preset {
                    opts.set("preset", preset);
                }
            }

            // VP9 tuning (SVT-VP9)
            EncoderBackend::SvtVp9 => {
                let preset = config.preset.as_deref().unwrap_or("8");
                opts.set("preset", preset);

                let rc = crf_rate_control(config, &mut encoder, &mut opts, 25, 63);
                encoder.set_max_b_frames(0);

                tracing::info!("SVT-VP9 tuning: preset={preset} {rc}");
            }

            // H.265/HEVC tuning
            EncoderBackend::H265Qsv => {
                encoder.set_bit_rate(kbps as usize * 1000);
                let preset = config.preset.as_deref().unwrap_or("veryfast");
                opts.set("preset", preset);
                opts.set("look_ahead", "0");
                opts.set("async_depth", "1");
                // As for NVENC: otherwise a forced keyframe is not an IDR frame.
                opts.set("forced_idr", "1");
                encoder.set_max_b_frames(0);
                tracing::info!("QSV HEVC tuning: preset={preset}");
            }
            EncoderBackend::H265Nvenc => {
                encoder.set_bit_rate(kbps as usize * 1000);
                let preset = config.preset.as_deref().unwrap_or("p1");
                opts.set("preset", preset);
                opts.set("tune", "ull");
                opts.set("rc", "cbr");
                // Without this a forced keyframe comes out as a plain intra
                // frame, which a decoder cannot start from.
                opts.set("forced-idr", "1");
                tracing::info!("NVENC HEVC tuning: preset={preset}");
            }
            EncoderBackend::H265Amf | EncoderBackend::H265Vaapi | EncoderBackend::H265V4l2m2m => {
                encoder.set_bit_rate(kbps as usize * 1000);
                if let Some(preset) = &config.preset {
                    opts.set("preset", preset);
                }
            }
            EncoderBackend::Libx265 => {
                let preset = config.preset.as_deref().unwrap_or("fast");
                opts.set("preset", preset);
                let rc = crf_rate_control(config, &mut encoder, &mut opts, 14, 51);
                // As for NVENC: otherwise a forced keyframe is a non-IDR intra frame.
                opts.set("forced-idr", "1");
                tracing::info!("libx265 tuning: preset={preset} {rc}");
            }

            // H.264 tuning
            EncoderBackend::H264Qsv => {
                encoder.set_bit_rate(kbps as usize * 1000);
                let preset = config.preset.as_deref().unwrap_or("veryfast");
                opts.set("preset", preset);
                opts.set("look_ahead", "0");
                opts.set("async_depth", "1");
                // As for NVENC: otherwise a forced keyframe is not an IDR frame.
                opts.set("forced_idr", "1");
                encoder.set_max_b_frames(0);
                tracing::info!("QSV H.264 tuning: preset={preset}");
            }
            EncoderBackend::H264Nvenc => {
                encoder.set_bit_rate(kbps as usize * 1000);
                let preset = config.preset.as_deref().unwrap_or("p1");
                opts.set("preset", preset);
                opts.set("tune", "ull");
                opts.set("rc", "cbr");
                // Without this a forced keyframe comes out as a plain intra
                // frame, which a decoder cannot start from.
                opts.set("forced-idr", "1");
                tracing::info!("NVENC H.264 tuning: preset={preset}");
            }
            EncoderBackend::H264Amf | EncoderBackend::H264Vaapi | EncoderBackend::H264V4l2m2m => {
                encoder.set_bit_rate(kbps as usize * 1000);
                if let Some(preset) = &config.preset {
                    opts.set("preset", preset);
                }
            }
            EncoderBackend::Libx264 => {
                let preset = config.preset.as_deref().unwrap_or("fast");
                opts.set("preset", preset);
                let rc = crf_rate_control(config, &mut encoder, &mut opts, 14, 51);
                tracing::info!("libx264 tuning: preset={preset} {rc}");
            }

            // VP8 and other backends with generic settings
            _ => {
                encoder.set_bit_rate(kbps as usize * 1000);
            }
        }

        let encoder = encoder.open_with(opts).map_err(|e| {
            EncoderError::InitFailed(format!("{backend}: open encoder: {e}"))
        })?;
        // The CRF-based backends log their rate control above; the rest use `kbps`.
        if !matches!(
            backend,
            EncoderBackend::SvtAv1 | EncoderBackend::SvtVp9 | EncoderBackend::Libx264 | EncoderBackend::Libx265
        ) {
            tracing::info!("{backend}: target bitrate {kbps} kbps");
        }

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
            vaapi,
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

        // Send frame to encoder (a VA-API one takes it as a VA surface)
        let sent = match &self.vaapi {
            Some(vaapi) => {
                let surface = vaapi.upload(&yuv_frame).map_err(EncoderError::EncodeFailed)?;
                self.encoder.send_frame(&surface)
            }
            None => self.encoder.send_frame(&yuv_frame),
        };
        sent.map_err(|e| EncoderError::EncodeFailed(format!("send frame: {e}")))?;

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
        match crate::quiet_ffmpeg(|| FfmpegVideoEncoder::new(backend, config)) {
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

    /// A forced keyframe must come out as one the decoder can start from.
    ///
    /// The server forces one after a resize, and anything that later asks for
    /// a keyframe on demand (a client attaching mid-stream) depends on it too.
    /// NVENC, left to its defaults, honours the request with a plain intra
    /// frame that is not a keyframe; `forced-idr` fixes that. The GOP is
    /// longer than the run, so any keyframe after the first was forced.
    ///
    /// Runs every backend this machine can open and skips the rest, so on a
    /// CI runner without a GPU it covers the software encoders only.
    #[test]
    fn forced_keyframe_is_a_real_keyframe() {
        let (w, h) = (320u32, 240u32);
        let config = EncoderConfig {
            width: w,
            height: h,
            fps: 30,
            rate: RateTarget::Kbps(1000),
            keyframe_interval: 300,
            ..Default::default()
        };
        let mut tested = 0;
        for &backend in ENCODER_PRIORITY {
            let Ok(mut encoder) = FfmpegVideoEncoder::new(backend, &config) else {
                continue;
            };
            let mut keyframes = 0;
            for i in 0..12u64 {
                // Vary the content so no encoder can treat frames as repeats.
                let rgba = vec![(i * 16) as u8; (w * h * 4) as usize];
                let packets = encoder
                    .encode_frame(&rgba, i * 33_000, i == 0 || i == 6)
                    .unwrap_or_else(|e| panic!("{backend}: frame {i}: {e}"));
                keyframes += packets.iter().filter(|p| p.keyframe).count();
            }
            let flushed = encoder
                .flush()
                .unwrap_or_else(|e| panic!("{backend}: flush: {e}"));
            keyframes += flushed.iter().filter(|p| p.keyframe).count();
            assert_eq!(keyframes, 2, "{backend}: the keyframe forced at frame 6 was not a keyframe");
            eprintln!("tested {backend}");
            tested += 1;
        }
        if tested == 0 {
            eprintln!("skipping: no video encoder in this environment");
        }
    }

    /// Encode a second of busy, moving content and return the bytes per second
    /// produced, or None if the backend does not open here.
    fn bytes_per_sec(backend: EncoderBackend, rate: RateTarget) -> Option<f64> {
        let (w, h) = (320u32, 240u32);
        let config = EncoderConfig { width: w, height: h, fps: 30, rate, keyframe_interval: 300, ..Default::default() };
        let mut encoder = FfmpegVideoEncoder::new(backend, &config).ok()?;
        // Noise, shifted each frame: costly enough that the rate control,
        // not the content, decides the size.
        let mut seed: u32 = 1;
        let noise: Vec<u8> = (0..w * h * 4)
            .map(|_| {
                seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12345);
                (seed >> 24) as u8
            })
            .collect();
        let mut bytes = 0usize;
        for i in 0..30u64 {
            let mut rgba = noise.clone();
            rgba.rotate_left(i as usize * 28);
            bytes += encoder.encode_frame(&rgba, i * 33_333, false).ok()?.iter().map(|p| p.data.len()).sum::<usize>();
        }
        bytes += encoder.flush().ok()?.iter().map(|p| p.data.len()).sum::<usize>();
        Some(bytes as f64)
    }

    /// Quality and an explicit bitrate must both change what every encoder
    /// produces. The CRF-based software encoders (SVT-AV1, libvpx-vp9,
    /// libx264, libx265) used to ignore both and always use a fixed CRF.
    #[test]
    fn quality_and_bitrate_change_the_output() {
        let mut tested = 0;
        for &backend in ENCODER_PRIORITY {
            let Some(q10) = bytes_per_sec(backend, RateTarget::Quality(10)) else { continue };
            let q90 = bytes_per_sec(backend, RateTarget::Quality(90)).unwrap();
            let low = bytes_per_sec(backend, RateTarget::Kbps(300)).unwrap();
            let high = bytes_per_sec(backend, RateTarget::Kbps(3000)).unwrap();
            assert!(q10 * 2.0 < q90, "{backend}: quality 10 gave {q10} B/s, quality 90 {q90} B/s");
            assert!(low * 2.0 < high, "{backend}: 300 kbps gave {low} B/s, 3000 kbps {high} B/s");
            eprintln!("tested {backend}: q10 {q10:.0} q90 {q90:.0} 300k {low:.0} 3000k {high:.0} B/s");
            tested += 1;
        }
        if tested == 0 {
            eprintln!("skipping: no video encoder in this environment");
        }
    }

    #[test]
    fn quality_maps_to_a_bitrate_scaled_for_the_codec() {
        assert_eq!(RateTarget::Quality(100).kbps(VideoCodec::Av1), 1500);
        assert_eq!(RateTarget::Quality(75).kbps(VideoCodec::Av1), 1125);
        assert_eq!(RateTarget::Quality(1).kbps(VideoCodec::Av1), MIN_AV1_KBPS);
        assert_eq!(RateTarget::Quality(75).kbps(VideoCodec::H264), 11_250);
        assert_eq!(RateTarget::Quality(1).kbps(VideoCodec::H264), 500);
        assert_eq!(RateTarget::Kbps(42).kbps(VideoCodec::Av1), 42, "an explicit bitrate is used as given");
    }

    #[test]
    fn quality_maps_to_a_crf_with_the_old_default_at_75() {
        assert_eq!(crf_for_quality(100, 25, 63), 25);
        assert_eq!(crf_for_quality(75, 25, 63), 35);
        assert_eq!(crf_for_quality(1, 25, 63), 63);
        assert_eq!(crf_for_quality(75, 14, 51), 23);
        assert_eq!(crf_for_quality(0, 14, 51), 51, "out-of-range quality is clamped");
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
            rate: RateTarget::Kbps(200),
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
