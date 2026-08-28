//! Q2 video plane: read the server's uni stream, decode with WebCodecs, paint
//! to a canvas.
//!
//! The header parse is `termland_protocol::q2` — the same code the server used
//! to write those bytes, not a second reading of the spec.

use std::rc::Rc;

use js_sys::{Object, Reflect, Uint8Array};
use termland_protocol::{parse_video_header, VideoCodec, VIDEO_HEADER_LEN};
use wasm_bindgen::closure::Closure;
use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_futures::JsFuture;
use web_sys::{
    CanvasRenderingContext2d, EncodedVideoChunk, EncodedVideoChunkInit, EncodedVideoChunkType,
    HtmlCanvasElement, ReadableStreamDefaultReader, VideoDecoder, VideoDecoderConfig,
    VideoDecoderInit, VideoFrame,
};

/// One decoded Q2 frame off the wire.
pub struct Q2Frame {
    pub codec: VideoCodec,
    pub keyframe: bool,
    pub width: u16,
    pub height: u16,
    pub timestamp_us: u64,
    pub data: Vec<u8>,
}

/// WebCodecs `VideoDecoderConfig.codec` strings to try, per codec, in
/// descending preference. A bare family name ("av01") is not a valid config
/// string — WebCodecs wants a full profile/level/depth triplet.
pub fn candidate_strings(codec: VideoCodec) -> &'static [&'static str] {
    match codec {
        VideoCodec::Av1 => &["av01.0.04M.08", "av01.0.08M.08", "av01.0.13M.08"],
        VideoCodec::Vp9 => &["vp09.00.10.08", "vp09.00.40.08", "vp09.00.51.08"],
        VideoCodec::Vp8 => &["vp8"],
        VideoCodec::H264 => &["avc1.42E01E", "avc1.4D401F", "avc1.64001F"],
        VideoCodec::H265 => &["hvc1.1.6.L93.B0", "hev1.1.6.L93.B0"],
    }
}

/// Ask WebCodecs which codecs this browser can really decode.
///
/// Advertising a codec the browser cannot decode would have the server encode
/// a stream that paints nothing, so the probe runs before the session request
/// and its result becomes `supported_codecs` on the wire. Order follows
/// `VideoCodec::all_preferred`.
pub async fn probe_supported_codecs() -> Vec<(VideoCodec, String)> {
    let mut supported = Vec::new();
    for codec in VideoCodec::all_preferred() {
        for candidate in candidate_strings(codec) {
            let config = VideoDecoderConfig::new(candidate);
            config.set_coded_width(640);
            config.set_coded_height(360);
            let Ok(promise) = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                VideoDecoder::is_config_supported(&config)
            })) else {
                continue;
            };
            let Ok(result) = JsFuture::from(promise).await else {
                continue;
            };
            let ok = Reflect::get(&result, &JsValue::from_str("supported"))
                .ok()
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            if ok {
                supported.push((codec, (*candidate).to_string()));
                break;
            }
        }
    }
    supported
}

/// Pick the WebCodecs string for a codec, preferring one the probe confirmed.
pub fn codec_string(codec: VideoCodec, probed: &[(VideoCodec, String)]) -> String {
    probed
        .iter()
        .find(|(c, _)| *c == codec)
        .map(|(_, s)| s.clone())
        .unwrap_or_else(|| candidate_strings(codec)[0].to_string())
}

/// WebCodecs decoder wired to a canvas.
pub struct VideoPipeline {
    canvas: HtmlCanvasElement,
    ctx: Rc<CanvasRenderingContext2d>,
    decoder: Option<VideoDecoder>,
    waiting_keyframe: bool,
    /// The decoder callbacks must outlive `configure`; dropping them would
    /// free the JS shims while WebCodecs still holds them.
    _callbacks: Option<(Closure<dyn FnMut(JsValue)>, Closure<dyn FnMut(JsValue)>)>,
}

impl VideoPipeline {
    pub fn new(canvas: HtmlCanvasElement) -> Result<Self, JsValue> {
        let ctx: CanvasRenderingContext2d = canvas
            .get_context("2d")?
            .ok_or_else(|| JsValue::from_str("canvas has no 2d context"))?
            .unchecked_into();
        Ok(Self {
            canvas,
            ctx: Rc::new(ctx),
            decoder: None,
            waiting_keyframe: true,
            _callbacks: None,
        })
    }

    /// (Re)build the decoder for a codec and size.
    ///
    /// Frames are painted straight from the decoder's output callback rather
    /// than being parked for `requestAnimationFrame`. rAF stops firing in a
    /// background tab, which is what stranded the TypeScript client's paint
    /// loop until it grew explicit recovery; with no latch to get stuck there
    /// is nothing to unstick.
    pub fn configure(&mut self, codec: VideoCodec, width: u16, height: u16, probed: &[(VideoCodec, String)]) -> Result<(), JsValue> {
        self.close_decoder();
        self.waiting_keyframe = true;

        let ctx = self.ctx.clone();
        let canvas = self.canvas.clone();
        let output = Closure::<dyn FnMut(JsValue)>::new(move |frame: JsValue| {
            let frame: VideoFrame = frame.unchecked_into();
            let w = frame.display_width();
            let h = frame.display_height();
            if canvas.width() != w {
                canvas.set_width(w);
            }
            if canvas.height() != h {
                canvas.set_height(h);
            }
            let _ = ctx.draw_image_with_video_frame(&frame, 0.0, 0.0);
            // WebCodecs hands out frames from a bounded pool; not closing one
            // stalls the decoder after a handful of frames.
            frame.close();
        });
        let error = Closure::<dyn FnMut(JsValue)>::new(move |e: JsValue| {
            web_sys::console::error_2(&JsValue::from_str("VideoDecoder error:"), &e);
        });

        let init = VideoDecoderInit::new(error.as_ref().unchecked_ref(), output.as_ref().unchecked_ref());
        let decoder = VideoDecoder::new(&init)?;
        let config = VideoDecoderConfig::new(&codec_string(codec, probed));
        config.set_coded_width(width as u32);
        config.set_coded_height(height as u32);
        config.set_optimize_for_latency(true);
        decoder.configure(&config)?;

        self.decoder = Some(decoder);
        self._callbacks = Some((output, error));
        Ok(())
    }

    /// Feed one frame. Inter frames before the first keyframe are dropped:
    /// decoding them would produce garbage or an error, not a picture.
    pub fn push(&mut self, frame: &Q2Frame) -> Result<(), JsValue> {
        let Some(decoder) = self.decoder.as_ref() else {
            return Ok(());
        };
        if self.waiting_keyframe && !frame.keyframe {
            return Ok(());
        }
        if frame.keyframe {
            self.waiting_keyframe = false;
        }
        // A decoder killed by a tab freeze reports "closed"; rebuilding it is
        // the caller's job, but decoding into it would throw.
        if decoder.state() == web_sys::CodecState::Closed {
            self.waiting_keyframe = true;
            return Ok(());
        }
        let data = Uint8Array::from(frame.data.as_slice());
        let kind = if frame.keyframe {
            EncodedVideoChunkType::Key
        } else {
            EncodedVideoChunkType::Delta
        };
        // web-sys binds `timestamp` as i32, but WebCodecs specifies `long
        // long` and this carries microseconds: i32 would wrap after roughly
        // 35 minutes of session uptime. Set it as f64 instead, which is what
        // a JS caller passing a Number does anyway.
        let init = EncodedVideoChunkInit::new(data.unchecked_ref(), 0, kind);
        init.set_timestamp_f64(frame.timestamp_us as f64);
        let chunk = EncodedVideoChunk::new(&init)?;
        decoder.decode(&chunk)
    }

    /// True when the decoder is gone or was torn down by the browser, so the
    /// session loop knows to reconfigure before the next frame.
    pub fn needs_reconfigure(&self) -> bool {
        match self.decoder.as_ref() {
            None => true,
            Some(d) => d.state() == web_sys::CodecState::Closed,
        }
    }

    fn close_decoder(&mut self) {
        if let Some(d) = self.decoder.take() {
            if d.state() != web_sys::CodecState::Closed {
                let _ = d.close();
            }
        }
        self._callbacks = None;
    }
}

impl Drop for VideoPipeline {
    fn drop(&mut self) {
        self.close_decoder();
    }
}

/// Pump complete Q2 frames off one WebTransport uni stream.
pub struct Q2Reader {
    reader: ReadableStreamDefaultReader,
    buf: Vec<u8>,
}

impl Q2Reader {
    pub fn new(stream: web_sys::ReadableStream) -> Self {
        Self {
            reader: stream.get_reader().unchecked_into(),
            buf: Vec::new(),
        }
    }

    /// Next frame, or `Ok(None)` at end of stream.
    pub async fn next(&mut self) -> Result<Option<Q2Frame>, JsValue> {
        let Some(header) = self.read_exact(VIDEO_HEADER_LEN).await? else {
            return Ok(None);
        };
        let header: [u8; VIDEO_HEADER_LEN] = header.try_into().expect("read_exact returned n bytes");
        let parsed = parse_video_header(&header)
            .ok_or_else(|| JsValue::from_str("Q2 header has an unknown codec tag"))?;
        if parsed.data_len > termland_protocol::MAX_VIDEO_FRAME_BYTES {
            return Err(JsValue::from_str(&format!(
                "Q2 frame claims {} bytes, over the {} cap",
                parsed.data_len,
                termland_protocol::MAX_VIDEO_FRAME_BYTES
            )));
        }
        let Some(data) = self.read_exact(parsed.data_len as usize).await? else {
            return Ok(None);
        };
        Ok(Some(Q2Frame {
            codec: parsed.codec,
            keyframe: parsed.keyframe,
            width: parsed.width,
            height: parsed.height,
            timestamp_us: parsed.timestamp_us,
            data,
        }))
    }

    /// Exactly `n` bytes, or `None` if the stream ends first.
    ///
    /// Leftovers are carried in `self.buf` and drained from the front once per
    /// call, rather than reallocating per chunk.
    async fn read_exact(&mut self, n: usize) -> Result<Option<Vec<u8>>, JsValue> {
        while self.buf.len() < n {
            let result = JsFuture::from(self.reader.read()).await?;
            let result: Object = result.unchecked_into();
            if Reflect::get(&result, &JsValue::from_str("done"))?
                .as_bool()
                .unwrap_or(false)
            {
                return Ok(None);
            }
            let chunk: Uint8Array =
                Reflect::get(&result, &JsValue::from_str("value"))?.unchecked_into();
            let start = self.buf.len();
            self.buf.resize(start + chunk.length() as usize, 0);
            chunk.copy_to(&mut self.buf[start..]);
        }
        Ok(Some(self.buf.drain(..n).collect()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use termland_protocol::{video_header_bytes, FrameType};

    #[test]
    fn every_codec_has_a_full_webcodecs_config_string() {
        for codec in VideoCodec::all_preferred() {
            let strings = candidate_strings(codec);
            assert!(!strings.is_empty(), "{codec:?} has no candidate");
            for s in strings {
                assert!(!s.is_empty());
                // "av01"/"vp09"/"avc1"/"hvc1" alone are rejected by
                // isConfigSupported; only VP8 is legitimately a bare name.
                if codec != VideoCodec::Vp8 {
                    assert!(s.contains('.'), "{codec:?}: {s} is not a full config string");
                }
            }
        }
    }

    #[test]
    fn codec_string_prefers_what_the_probe_confirmed() {
        let probed = vec![(VideoCodec::Av1, "av01.0.08M.08".to_string())];
        assert_eq!(codec_string(VideoCodec::Av1, &probed), "av01.0.08M.08");
        // Not probed: fall back to the first candidate rather than nothing.
        assert_eq!(codec_string(VideoCodec::Vp9, &probed), "vp09.00.10.08");
    }

    /// The reader must agree with the server's encoder byte-for-byte; both
    /// sides are `termland_protocol::q2`, and this pins that they stay wired
    /// to each other.
    #[test]
    fn header_written_by_the_server_parses_back() {
        let header = video_header_bytes(VideoCodec::Av1, FrameType::Keyframe, 1280, 720, 999, 4096);
        let parsed = parse_video_header(&header).expect("parse");
        assert_eq!(parsed.codec, VideoCodec::Av1);
        assert!(parsed.keyframe);
        assert_eq!(parsed.width, 1280);
        assert_eq!(parsed.height, 720);
        assert_eq!(parsed.timestamp_us, 999);
        assert_eq!(parsed.data_len, 4096);
    }
}
