use thiserror::Error;
use termland_protocol::VideoCodec;

#[derive(Debug, Error)]
pub enum DecoderError {
    #[error("decoder init failed: {0}")]
    InitFailed(String),
    #[error("decode failed: {0}")]
    DecodeFailed(String),
    #[error("no frame available")]
    NoFrame,
}

/// Which video decoder backend is in use.
/// Priority order (open-source first):
/// 1. AV1 (hardware) → AV1 (software)
/// 2. VP9 (hardware) → VP9 (software)
/// 3. VP8 (hardware) → VP8 (software)
/// 4. H.265/HEVC (hardware) → H.265 (software)
/// 5. H.264/AVC (hardware) → H.264 (software)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecoderBackend {
    // AV1
    IntelQsv,
    NvidiaCuvid,
    Dav1d,
    
    // VP9
    Vp9Vaapi,
    Vp9V4l2m2m,
    LibvpxVp9,
    
    // VP8
    Vp8V4l2m2m,
    Libvpx,
    
    // H.265/HEVC
    HevcQsv,
    HevcCuvid,
    HevcVaapi,
    HevcV4l2m2m,
    Libx265,
    
    // H.264
    H264Qsv,
    H264Cuvid,
    H264Vaapi,
    H264V4l2m2m,
    Libx264,
}

impl std::fmt::Display for DecoderBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            // AV1
            DecoderBackend::IntelQsv => write!(f, "Intel QSV (av1_qsv)"),
            DecoderBackend::NvidiaCuvid => write!(f, "NVIDIA CUVID (av1_cuvid)"),
            DecoderBackend::Dav1d => write!(f, "dav1d (software)"),
            
            // VP9
            DecoderBackend::Vp9Vaapi => write!(f, "VA-API (vp9_vaapi)"),
            DecoderBackend::Vp9V4l2m2m => write!(f, "v4l2m2m (vp9_v4l2m2m)"),
            DecoderBackend::LibvpxVp9 => write!(f, "libvpx-vp9 (software)"),
            
            // VP8
            DecoderBackend::Vp8V4l2m2m => write!(f, "v4l2m2m (vp8_v4l2m2m)"),
            DecoderBackend::Libvpx => write!(f, "libvpx (vp8, software)"),
            
            // H.265
            DecoderBackend::HevcQsv => write!(f, "Intel QSV (hevc_qsv)"),
            DecoderBackend::HevcCuvid => write!(f, "NVIDIA CUVID (hevc_cuvid)"),
            DecoderBackend::HevcVaapi => write!(f, "VA-API (hevc_vaapi)"),
            DecoderBackend::HevcV4l2m2m => write!(f, "v4l2m2m (hevc_v4l2m2m)"),
            DecoderBackend::Libx265 => write!(f, "libx265 (software)"),
            
            // H.264
            DecoderBackend::H264Qsv => write!(f, "Intel QSV (h264_qsv)"),
            DecoderBackend::H264Cuvid => write!(f, "NVIDIA CUVID (h264_cuvid)"),
            DecoderBackend::H264Vaapi => write!(f, "VA-API (h264_vaapi)"),
            DecoderBackend::H264V4l2m2m => write!(f, "v4l2m2m (h264_v4l2m2m)"),
            DecoderBackend::Libx264 => write!(f, "libx264 (software)"),
        }
    }
}

impl DecoderBackend {
    fn codec_name(&self) -> &'static str {
        match self {
            // AV1
            DecoderBackend::IntelQsv => "av1_qsv",
            DecoderBackend::NvidiaCuvid => "av1_cuvid",
            DecoderBackend::Dav1d => "libdav1d",
            
            // VP9
            DecoderBackend::Vp9Vaapi => "vp9_vaapi",
            DecoderBackend::Vp9V4l2m2m => "vp9_v4l2m2m",
            DecoderBackend::LibvpxVp9 => "libvpx-vp9",
            
            // VP8
            DecoderBackend::Vp8V4l2m2m => "vp8_v4l2m2m",
            DecoderBackend::Libvpx => "libvpx",
            
            // H.265
            DecoderBackend::HevcQsv => "hevc_qsv",
            DecoderBackend::HevcCuvid => "hevc_cuvid",
            DecoderBackend::HevcVaapi => "hevc_vaapi",
            DecoderBackend::HevcV4l2m2m => "hevc_v4l2m2m",
            DecoderBackend::Libx265 => "libx265",
            
            // H.264
            DecoderBackend::H264Qsv => "h264_qsv",
            DecoderBackend::H264Cuvid => "h264_cuvid",
            DecoderBackend::H264Vaapi => "h264_vaapi",
            DecoderBackend::H264V4l2m2m => "h264_v4l2m2m",
            DecoderBackend::Libx264 => "libx264",
        }
    }

    /// The wire codec this backend decodes.
    pub fn codec(&self) -> VideoCodec {
        match self {
            DecoderBackend::IntelQsv
            | DecoderBackend::NvidiaCuvid
            | DecoderBackend::Dav1d => VideoCodec::Av1,

            DecoderBackend::Vp9Vaapi
            | DecoderBackend::Vp9V4l2m2m
            | DecoderBackend::LibvpxVp9 => VideoCodec::Vp9,

            DecoderBackend::Vp8V4l2m2m
            | DecoderBackend::Libvpx => VideoCodec::Vp8,

            DecoderBackend::HevcQsv
            | DecoderBackend::HevcCuvid
            | DecoderBackend::HevcVaapi
            | DecoderBackend::HevcV4l2m2m
            | DecoderBackend::Libx265 => VideoCodec::H265,

            DecoderBackend::H264Qsv
            | DecoderBackend::H264Cuvid
            | DecoderBackend::H264Vaapi
            | DecoderBackend::H264V4l2m2m
            | DecoderBackend::Libx264 => VideoCodec::H264,
        }
    }
}

/// Backends to try, in priority order (used by `new()` auto-detect; with a
/// negotiated codec, `for_codec` filters this to a single codec's backends).
///
/// Priority: hardware before software; within each tier, open-source codecs
/// first then patent-encumbered. Intel QSV decoders are placed *after* the
/// other hardware backends of the same codec: QSV opens successfully even with
/// no Intel GPU present and only fails on the first packet, whereas CUVID /
/// VA-API / V4L2 fail cleanly at init — so trying them first avoids wasting the
/// opening frames on a doomed QSV context (e.g. on an NVIDIA-only box).
const BACKEND_PRIORITY: &[DecoderBackend] = &[
    // --- Hardware decoders (open-source codecs first) ---
    // AV1 HW
    DecoderBackend::NvidiaCuvid,
    DecoderBackend::IntelQsv,
    // VP9 HW
    DecoderBackend::Vp9Vaapi,
    DecoderBackend::Vp9V4l2m2m,
    // VP8 HW
    DecoderBackend::Vp8V4l2m2m,
    // H.265/HEVC HW (patent-encumbered)
    DecoderBackend::HevcCuvid,
    DecoderBackend::HevcVaapi,
    DecoderBackend::HevcV4l2m2m,
    DecoderBackend::HevcQsv,
    // H.264 HW (patent-encumbered)
    DecoderBackend::H264Cuvid,
    DecoderBackend::H264Vaapi,
    DecoderBackend::H264V4l2m2m,
    DecoderBackend::H264Qsv,

    // --- Software decoders (open-source codecs first) ---
    DecoderBackend::Dav1d,
    DecoderBackend::LibvpxVp9,
    DecoderBackend::Libvpx,
    DecoderBackend::Libx265,
    DecoderBackend::Libx264,
];

struct SendScaler(ffmpeg_next::software::scaling::Context);
unsafe impl Send for SendScaler {}
impl std::ops::Deref for SendScaler {
    type Target = ffmpeg_next::software::scaling::Context;
    fn deref(&self) -> &Self::Target { &self.0 }
}
impl std::ops::DerefMut for SendScaler {
    fn deref_mut(&mut self) -> &mut ffmpeg_next::software::scaling::Context { &mut self.0 }
}

/// Video decoder using FFmpeg with automatic codec detection and fallback.
/// Supports AV1, VP9, VP8, H.265, and H.264.
///
/// The decoder auto-detects the codec from the bitstream and falls back to
/// the next backend in the priority list if decoding fails.
pub struct VideoDecoder {
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
    /// When set (via codec negotiation), only backends decoding this codec are
    /// considered — both at init and during fallback. `None` = try any codec.
    codec_filter: Option<VideoCodec>,
}

impl VideoDecoder {
    /// Create a new video decoder, probing for the best available backend
    /// across all codecs. Use this only when the codec is unknown (e.g. an
    /// older server that doesn't announce one). If it later fails to decode,
    /// we transparently fall back to the next backend in the priority list.
    pub fn new() -> Result<Self, DecoderError> {
        ffmpeg_next::init().map_err(|e| DecoderError::InitFailed(format!("ffmpeg: {e}")))?;
        Self::init_from_index(0, Vec::new(), None)
    }

    /// Create a decoder for a specific negotiated codec. Only backends that
    /// decode `codec` are probed, so a VP9 stream never gets fed to an AV1 or
    /// H.264 decoder. Falls back among that codec's backends (HW → SW) if one
    /// fails at runtime.
    pub fn for_codec(codec: VideoCodec) -> Result<Self, DecoderError> {
        ffmpeg_next::init().map_err(|e| DecoderError::InitFailed(format!("ffmpeg: {e}")))?;
        Self::init_from_index(0, Vec::new(), Some(codec))
    }

    fn init_from_index(
        start: usize,
        failed: Vec<DecoderBackend>,
        codec_filter: Option<VideoCodec>,
    ) -> Result<Self, DecoderError> {
        for (idx, backend) in BACKEND_PRIORITY.iter().enumerate().skip(start) {
            if failed.contains(backend) {
                continue;
            }
            if let Some(codec) = codec_filter {
                if backend.codec() != codec {
                    continue;
                }
            }
            tracing::info!("Probing video decoder: {backend}...");
            match Self::open_codec(*backend) {
                Ok(decoder) => {
                    tracing::info!("Selected video decoder: {backend}");
                    return Ok(Self {
                        backend: *backend,
                        decoder,
                        scaler: None,
                        width: 0,
                        height: 0,
                        backend_index: idx,
                        failed_backends: failed,
                        confirmed_working: false,
                        codec_filter,
                    });
                }
                Err(e) => tracing::debug!("  {backend}: {e}"),
            }
        }
        Err(DecoderError::InitFailed(match codec_filter {
            Some(c) => format!("no {c} decoder available"),
            None => "no video decoder available".into(),
        }))
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

    /// The codec the current backend decodes.
    pub fn codec(&self) -> VideoCodec {
        self.backend.codec()
    }

    /// Decode a video packet (AV1, VP9, VP8, H.265, or H.264). Returns (width, height, pixels).
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
            return self.reinit_and_retry(data, format!("send packet: {e}"));
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
                return self.reinit_and_retry(data, format!("receive frame: {e}"));
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

    /// Reinitialize the current backend (same codec) and retry. Used when a
    /// confirmed-working decoder chokes - typically because a keyframe arrived
    /// with new dimensions and the internal parser is still bound to the old
    /// SPS. CUVID in particular throws CUDA_ERROR_UNKNOWN from
    /// cuvidParseVideoData in this case. A fresh codec context re-parses the
    /// incoming keyframe cleanly.
    fn reinit_and_retry(&mut self, data: &[u8], reason: String) -> Result<(u32, u32, Vec<u32>), DecoderError> {
        tracing::warn!("Decoder {} hiccup ({reason}), reinitializing same backend", self.backend);
        let new_decoder = Self::open_codec(self.backend)
            .map_err(|e| DecoderError::DecodeFailed(format!("reinit {}: {e}", self.backend)))?;
        self.decoder = new_decoder;
        self.scaler = None;
        self.width = 0;
        self.height = 0;
        // Keep confirmed_working=true: we know this backend is fine in general.
        // The packet we're about to feed must be a keyframe for this to succeed;
        // if it's not, the decoder will EAGAIN and the next keyframe will work.
        let packet = ffmpeg_next::Packet::copy(data);
        match self.decoder.send_packet(&packet) {
            Ok(()) => {}
            Err(ffmpeg_next::Error::Other { errno: libc::EAGAIN }) => {
                return Err(DecoderError::NoFrame);
            }
            Err(e) => return Err(DecoderError::DecodeFailed(format!("reinit send: {e}"))),
        }
        let mut frame = ffmpeg_next::frame::Video::empty();
        match self.decoder.receive_frame(&mut frame) {
            Ok(()) => {}
            Err(ffmpeg_next::Error::Other { errno: libc::EAGAIN }) => {
                return Err(DecoderError::NoFrame);
            }
            Err(e) => return Err(DecoderError::DecodeFailed(format!("reinit recv: {e}"))),
        }
        let w = frame.width();
        let h = frame.height();
        let fmt = frame.format();
        let scaler = ffmpeg_next::software::scaling::Context::get(
            fmt, w, h,
            ffmpeg_next::format::Pixel::RGBA, w, h,
            ffmpeg_next::software::scaling::Flags::BILINEAR
                | ffmpeg_next::software::scaling::Flags::ACCURATE_RND
                | ffmpeg_next::software::scaling::Flags::FULL_CHR_H_INT,
        ).map_err(|e| DecoderError::DecodeFailed(format!("create scaler: {e}")))?;
        self.scaler = Some(SendScaler(scaler));
        self.width = w;
        self.height = h;
        let mut rgba_frame = ffmpeg_next::frame::Video::new(
            ffmpeg_next::format::Pixel::RGBA, w, h,
        );
        self.scaler.as_mut().unwrap().run(&frame, &mut rgba_frame)
            .map_err(|e| DecoderError::DecodeFailed(format!("scale: {e}")))?;
        let stride = rgba_frame.stride(0);
        let src = rgba_frame.data(0);
        let mut pixels = Vec::with_capacity((w * h) as usize);
        for row in 0..h as usize {
            let row_start = row * stride;
            for col in 0..w as usize {
                let i = row_start + col * 4;
                let r = src[i] as u32;
                let g = src[i + 1] as u32;
                let b = src[i + 2] as u32;
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
        let new_decoder = Self::init_from_index(next_index, failed, self.codec_filter)?;
        *self = new_decoder;
        self.decode(data)
    }
}