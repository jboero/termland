//! Termland's browser protocol codec, compiled to WebAssembly.
//!
//! # Why wasm rather than a JavaScript protocol library
//!
//! Reimplementing the 7-byte `TL` header and serde's externally-tagged CBOR
//! in TypeScript is a second source of truth. Cross-language fixtures only
//! tell you *after* the two have already diverged — which is exactly the hole
//! a hand-rolled encoder had until this crate took over encode/decode.
//!
//! `termland-protocol` compiles to wasm, so the browser runs the same
//! `TermlandCodec`, the same `Message` enum and the same serde derives the
//! server does. A protocol change is a recompile, not a port.
//!
//! What is left for JavaScript is the part that genuinely is browser API
//! surface — obtaining a `WebTransport`, handing frames to `VideoDecoder`,
//! painting a canvas, capturing input.

use bytes::{Buf, BufMut, BytesMut};
use js_sys::{Array, BigInt, Reflect, Uint8Array};
use serde::Serialize;
use termland_protocol::{
    parse_video_header, video_header_bytes, FrameType, Message, TermlandCodec, VideoCodec,
    FRAME_MAGIC, MAX_PAYLOAD_SIZE, MAX_VIDEO_FRAME_BYTES, VIDEO_HEADER_LEN,
};
use tokio_util::codec::Encoder;
use wasm_bindgen::prelude::*;

const HEADER_SIZE: usize = 7;

/// Install the panic hook. Called automatically when the module loads.
#[wasm_bindgen(start)]
pub fn start() {
    console_error_panic_hook::set_once();
}

#[wasm_bindgen(js_name = frameMagic)]
pub fn frame_magic() -> Uint8Array {
    Uint8Array::from(&FRAME_MAGIC[..])
}

#[wasm_bindgen(js_name = maxPayloadSize)]
pub fn max_payload_size() -> u32 {
    MAX_PAYLOAD_SIZE
}

#[wasm_bindgen(js_name = videoHeaderLen)]
pub fn video_header_len() -> usize {
    VIDEO_HEADER_LEN
}

/// Encode a `Message` (externally tagged, as serde produces it) to a `TL` frame.
#[wasm_bindgen(js_name = encodeWire)]
pub fn encode_wire(msg: JsValue) -> Result<Uint8Array, JsValue> {
    let msg: Message = serde_wasm_bindgen::from_value(msg).map_err(js_err)?;
    let bytes = encode_wire_msg(&msg).map_err(js_err)?;
    Ok(Uint8Array::from(bytes.as_slice()))
}

/// Encode a `Message` to CBOR payload bytes (no `TL` frame).
#[wasm_bindgen(js_name = encodePayload)]
pub fn encode_payload(msg: JsValue) -> Result<Uint8Array, JsValue> {
    let msg: Message = serde_wasm_bindgen::from_value(msg).map_err(js_err)?;
    let bytes = msg.encode().map_err(|e| js_err(e.to_string()))?;
    Ok(Uint8Array::from(bytes.as_slice()))
}

/// Wrap raw CBOR in a 7-byte `TL` frame. Used by tests; production traffic
/// goes through `encode_wire`.
#[wasm_bindgen(js_name = encodeFrame)]
pub fn encode_frame(msg_id: u8, payload: &[u8]) -> Result<Uint8Array, JsValue> {
    let bytes = encode_frame_bytes(msg_id, payload).map_err(js_err)?;
    Ok(Uint8Array::from(bytes.as_slice()))
}

/// Decode a CBOR payload to an externally tagged `Message` object.
///
/// Unrecognized enum tags become `{ type: "Unknown", tag }` so a newer server
/// can add messages without the tab throwing on the control stream.
#[wasm_bindgen(js_name = decodeMessage)]
pub fn decode_message(payload: &[u8]) -> Result<JsValue, JsValue> {
    match decode_payload(payload) {
        Decoded::Msg(msg) => to_js(&msg),
        Decoded::Unknown { tag } => {
            let obj = js_sys::Object::new();
            Reflect::set(&obj, &"type".into(), &JsValue::from_str("Unknown"))?;
            Reflect::set(&obj, &"tag".into(), &JsValue::from_str(&tag))?;
            Ok(obj.into())
        }
    }
}

/// Incremental 7-byte Termland framing. `push` returns complete CBOR payloads.
#[wasm_bindgen]
pub struct FrameDecoder {
    buf: BytesMut,
}

#[wasm_bindgen]
impl FrameDecoder {
    #[wasm_bindgen(constructor)]
    pub fn new() -> FrameDecoder {
        FrameDecoder {
            buf: BytesMut::new(),
        }
    }

    #[wasm_bindgen]
    pub fn push(&mut self, chunk: &[u8]) -> Result<Array, JsValue> {
        self.buf.extend_from_slice(chunk);
        let out = Array::new();
        loop {
            match try_extract_frame(&mut self.buf).map_err(js_err)? {
                None => break,
                Some((_id, payload)) => {
                    out.push(&Uint8Array::from(payload.as_slice()));
                }
            }
        }
        Ok(out)
    }
}

/// Parse an 18-byte Q2 header. `null` for an unknown codec tag.
/// Throws on a short buffer or an implausible `data_len`.
#[wasm_bindgen(js_name = parseVideoHeader)]
pub fn parse_video_header_js(header: &[u8]) -> Result<JsValue, JsValue> {
    if header.len() < VIDEO_HEADER_LEN {
        return Err(js_err(format!(
            "video header too short ({})",
            header.len()
        )));
    }
    let mut arr = [0u8; VIDEO_HEADER_LEN];
    arr.copy_from_slice(&header[..VIDEO_HEADER_LEN]);
    let Some(parsed) = parse_video_header(&arr) else {
        return Err(js_err(format!("unknown codec tag {}", header[0])));
    };
    if parsed.data_len > MAX_VIDEO_FRAME_BYTES {
        return Err(js_err(format!(
            "implausible frame size {}",
            parsed.data_len
        )));
    }
    video_header_to_js(parsed)
}

/// Encode an 18-byte Q2 header. `timestamp_us` is a BigInt so values above
/// `Number.MAX_SAFE_INTEGER` still round-trip in tests.
#[wasm_bindgen(js_name = encodeVideoHeader)]
pub fn encode_video_header_js(
    codec: &str,
    keyframe: bool,
    width: u16,
    height: u16,
    timestamp_us: BigInt,
    data_len: u32,
) -> Result<Uint8Array, JsValue> {
    let codec = codec_from_name(codec).ok_or_else(|| js_err(format!("unknown codec {codec}")))?;
    let ts = bigint_to_u64(&timestamp_us)?;
    let frame_type = if keyframe {
        FrameType::Keyframe
    } else {
        FrameType::Inter
    };
    let bytes = video_header_bytes(codec, frame_type, width, height, ts, data_len);
    Ok(Uint8Array::from(&bytes[..]))
}

#[derive(Debug)]
enum Decoded {
    Msg(Message),
    Unknown { tag: String },
}

fn encode_wire_msg(msg: &Message) -> Result<Vec<u8>, String> {
    let mut codec = TermlandCodec;
    let mut buf = BytesMut::new();
    codec
        .encode(msg.clone(), &mut buf)
        .map_err(|e| e.to_string())?;
    Ok(buf.to_vec())
}

fn encode_frame_bytes(msg_id: u8, payload: &[u8]) -> Result<Vec<u8>, String> {
    if payload.len() as u32 > MAX_PAYLOAD_SIZE {
        return Err(format!(
            "payload too large: {} bytes (max {MAX_PAYLOAD_SIZE})",
            payload.len()
        ));
    }
    let mut out = BytesMut::with_capacity(HEADER_SIZE + payload.len());
    out.extend_from_slice(&FRAME_MAGIC);
    out.put_u8(msg_id);
    out.put_u32_le(payload.len() as u32);
    out.extend_from_slice(payload);
    Ok(out.to_vec())
}

fn try_extract_frame(src: &mut BytesMut) -> Result<Option<(u8, Vec<u8>)>, String> {
    if src.len() < HEADER_SIZE {
        return Ok(None);
    }
    if src[0] != FRAME_MAGIC[0] || src[1] != FRAME_MAGIC[1] {
        return Err("invalid magic bytes".into());
    }
    let msg_id = src[2];
    let payload_len = u32::from_le_bytes([src[3], src[4], src[5], src[6]]);
    if payload_len > MAX_PAYLOAD_SIZE {
        return Err(format!(
            "payload too large: {payload_len} bytes (max {MAX_PAYLOAD_SIZE})"
        ));
    }
    let total = HEADER_SIZE + payload_len as usize;
    if src.len() < total {
        src.reserve(total - src.len());
        return Ok(None);
    }
    src.advance(HEADER_SIZE);
    let payload = src.split_to(payload_len as usize).to_vec();
    Ok(Some((msg_id, payload)))
}

fn decode_payload(data: &[u8]) -> Decoded {
    match Message::decode(data) {
        Ok(m) => Decoded::Msg(m),
        Err(_) => Decoded::Unknown {
            tag: peek_cbor_tag(data).unwrap_or_else(|| "undecodable".into()),
        },
    }
}

fn peek_cbor_tag(data: &[u8]) -> Option<String> {
    let v: ciborium::Value = ciborium::from_reader(data).ok()?;
    match v {
        ciborium::Value::Map(pairs) => pairs.into_iter().next().and_then(|(k, _)| match k {
            ciborium::Value::Text(s) => Some(s),
            _ => None,
        }),
        _ => None,
    }
}

fn to_js<T: Serialize>(value: &T) -> Result<JsValue, JsValue> {
    let serializer = serde_wasm_bindgen::Serializer::new().serialize_maps_as_objects(true);
    value.serialize(&serializer).map_err(js_err)
}

fn video_header_to_js(h: termland_protocol::q2::VideoHeader) -> Result<JsValue, JsValue> {
    let obj = js_sys::Object::new();
    Reflect::set(
        &obj,
        &"codec".into(),
        &JsValue::from_str(codec_name(h.codec)),
    )?;
    Reflect::set(&obj, &"keyframe".into(), &JsValue::from_bool(h.keyframe))?;
    Reflect::set(&obj, &"width".into(), &JsValue::from(h.width))?;
    Reflect::set(&obj, &"height".into(), &JsValue::from(h.height))?;
    let ts = BigInt::from(h.timestamp_us);
    Reflect::set(&obj, &"timestamp_us".into(), &ts)?;
    Reflect::set(&obj, &"data_len".into(), &JsValue::from(h.data_len))?;
    Ok(obj.into())
}

fn codec_name(codec: VideoCodec) -> &'static str {
    match codec {
        VideoCodec::Av1 => "Av1",
        VideoCodec::Vp9 => "Vp9",
        VideoCodec::Vp8 => "Vp8",
        VideoCodec::H265 => "H265",
        VideoCodec::H264 => "H264",
    }
}

fn codec_from_name(name: &str) -> Option<VideoCodec> {
    match name {
        "Av1" => Some(VideoCodec::Av1),
        "Vp9" => Some(VideoCodec::Vp9),
        "Vp8" => Some(VideoCodec::Vp8),
        "H265" => Some(VideoCodec::H265),
        "H264" => Some(VideoCodec::H264),
        _ => None,
    }
}

fn bigint_to_u64(v: &BigInt) -> Result<u64, JsValue> {
    let s = v
        .to_string(10)
        .map_err(|e| JsValue::from(e))?
        .as_string()
        .ok_or_else(|| js_err("timestamp BigInt toString failed"))?;
    s.parse::<u64>()
        .map_err(|e| js_err(format!("timestamp_us: {e}")))
}

fn js_err(e: impl ToString) -> JsValue {
    JsValue::from_str(&e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use termland_protocol::{Hello, MouseMove, PROTOCOL_VERSION};
    use tokio_util::codec::Decoder;

    #[test]
    fn hello_wire_round_trips_through_the_server_codec() {
        let msg = Message::Hello(Hello {
            protocol_version: PROTOCOL_VERSION,
            client_name: "fixture-client".into(),
        });
        let wire = encode_wire_msg(&msg).expect("encode");
        assert_eq!(&wire[..2], &FRAME_MAGIC);
        let mut buf = BytesMut::from(wire.as_slice());
        let decoded = TermlandCodec.decode(&mut buf).unwrap().unwrap();
        match decoded {
            Message::Hello(h) => {
                assert_eq!(h.protocol_version, PROTOCOL_VERSION);
                assert_eq!(h.client_name, "fixture-client");
            }
            other => panic!("expected Hello, got {other:?}"),
        }
    }

    #[test]
    fn payload_bytes_match_message_encode() {
        let msg = Message::Hello(Hello {
            protocol_version: PROTOCOL_VERSION,
            client_name: "fixture-client".into(),
        });
        let payload = msg.encode().unwrap();
        match decode_payload(&payload) {
            Decoded::Msg(Message::Hello(h)) => assert_eq!(h.client_name, "fixture-client"),
            other => panic!("unexpected {other:?}"),
        }
        let wire = encode_wire_msg(&msg).unwrap();
        assert_eq!(&wire[HEADER_SIZE..], payload.as_slice());
    }

    #[test]
    fn unknown_cbor_tag_is_not_a_hard_error() {
        let mut buf = Vec::new();
        let value = ciborium::Value::Map(vec![(
            ciborium::Value::Text("NotARealMessage".into()),
            ciborium::Value::Map(vec![]),
        )]);
        ciborium::into_writer(&value, &mut buf).unwrap();
        match decode_payload(&buf) {
            Decoded::Unknown { tag } => assert_eq!(tag, "NotARealMessage"),
            Decoded::Msg(m) => panic!("expected Unknown, got {m:?}"),
        }
    }

    #[test]
    fn mouse_move_payload_round_trips() {
        let msg = Message::MouseMove(MouseMove {
            x: 100.5,
            y: 200.25,
            absolute: true,
        });
        let payload = msg.encode().unwrap();
        match decode_payload(&payload) {
            Decoded::Msg(Message::MouseMove(m)) => {
                assert!((m.x - 100.5).abs() < 1e-6);
                assert!(m.absolute);
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn partial_header_is_not_yet_a_frame() {
        let mut buf = BytesMut::from(&b"TL\x01"[..]);
        assert!(try_extract_frame(&mut buf).unwrap().is_none());
        assert_eq!(&buf[..], b"TL\x01");
    }

    #[test]
    fn invalid_magic_is_rejected() {
        let mut buf = BytesMut::from(&b"XX\x01\x00\x00\x00\x00"[..]);
        match try_extract_frame(&mut buf) {
            Err(e) => assert!(e.contains("invalid magic"), "{e}"),
            Ok(v) => panic!("expected error, got {v:?}"),
        }
    }

    #[test]
    fn payload_over_16_mib_is_rejected_from_the_header_alone() {
        let mut buf = BytesMut::new();
        buf.extend_from_slice(&FRAME_MAGIC);
        buf.put_u8(0x01);
        buf.put_u32_le(MAX_PAYLOAD_SIZE + 1);
        match try_extract_frame(&mut buf) {
            Err(e) => assert!(e.contains("too large"), "{e}"),
            Ok(v) => panic!("expected error, got {v:?}"),
        }
    }

    #[test]
    fn split_frame_across_two_pushes() {
        let payload = vec![9, 8, 7];
        let full = encode_frame_bytes(0x0b, &payload).unwrap();
        let mut buf = BytesMut::new();
        buf.extend_from_slice(&full[..5]);
        assert!(try_extract_frame(&mut buf).unwrap().is_none());
        buf.extend_from_slice(&full[5..]);
        let (id, got) = try_extract_frame(&mut buf).unwrap().unwrap();
        assert_eq!(id, 0x0b);
        assert_eq!(got, payload);
    }

    #[test]
    fn q2_header_round_trips() {
        let bytes = video_header_bytes(
            VideoCodec::H264,
            FrameType::Keyframe,
            0x0102,
            0x0304,
            0x0102030405060708,
            0xAABBCCDD,
        );
        let parsed = parse_video_header(&bytes).unwrap();
        assert_eq!(parsed.codec, VideoCodec::H264);
        assert!(parsed.keyframe);
        assert_eq!(parsed.width, 0x0102);
        assert_eq!(parsed.data_len, 0xAABBCCDD);
    }
}
