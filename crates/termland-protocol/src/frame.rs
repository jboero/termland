use bytes::{Buf, BufMut, BytesMut};
use tokio_util::codec::{Decoder, Encoder};

use crate::messages::{DecodeError, EncodeError, FRAME_MAGIC, MAX_PAYLOAD_SIZE, Message};

/// Header: 2 bytes magic + 1 byte msg_id + 4 bytes length = 7 bytes
const HEADER_SIZE: usize = 7;

/// Tokio codec for termland wire protocol.
///
/// Wire format:
/// ```text
/// [Magic "TL" 2B][MsgID 1B][Payload Length 4B LE][CBOR payload]
/// ```
pub struct TermlandCodec;

#[derive(Debug, thiserror::Error)]
pub enum CodecError {
    #[error("invalid magic bytes")]
    InvalidMagic,
    #[error("payload too large: {0} bytes (max {MAX_PAYLOAD_SIZE})")]
    PayloadTooLarge(u32),
    #[error("encode error: {0}")]
    Encode(#[from] EncodeError),
    #[error("decode error: {0}")]
    Decode(#[from] DecodeError),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

impl Decoder for TermlandCodec {
    type Item = Message;
    type Error = CodecError;

    fn decode(&mut self, src: &mut BytesMut) -> Result<Option<Self::Item>, Self::Error> {
        if src.len() < HEADER_SIZE {
            return Ok(None);
        }

        // Peek at header without consuming
        let magic = [src[0], src[1]];
        if magic != FRAME_MAGIC {
            return Err(CodecError::InvalidMagic);
        }

        // src[2] is message_id - we don't use it for decoding since CBOR has the enum tag,
        // but it's useful for logging/debugging without deserializing
        let _msg_id = src[2];

        let payload_len = u32::from_le_bytes([src[3], src[4], src[5], src[6]]);
        if payload_len > MAX_PAYLOAD_SIZE {
            return Err(CodecError::PayloadTooLarge(payload_len));
        }

        let total_len = HEADER_SIZE + payload_len as usize;
        if src.len() < total_len {
            // Reserve space for the full frame so tokio reads enough
            src.reserve(total_len - src.len());
            return Ok(None);
        }

        // Consume the header
        src.advance(HEADER_SIZE);

        // Consume the payload
        let payload = src.split_to(payload_len as usize);

        let msg = Message::decode(&payload)?;
        Ok(Some(msg))
    }
}

impl Encoder<Message> for TermlandCodec {
    type Error = CodecError;

    fn encode(&mut self, item: Message, dst: &mut BytesMut) -> Result<(), Self::Error> {
        let msg_id = item.message_id() as u8;
        let payload = item.encode()?;
        let payload_len = payload.len() as u32;

        if payload_len > MAX_PAYLOAD_SIZE {
            return Err(CodecError::PayloadTooLarge(payload_len));
        }

        dst.reserve(HEADER_SIZE + payload.len());
        dst.put_slice(&FRAME_MAGIC);
        dst.put_u8(msg_id);
        dst.put_u32_le(payload_len);
        dst.put_slice(&payload);

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_hello() {
        let msg = Message::Hello(crate::Hello {
            protocol_version: 1,
            client_name: "test-client".into(),
        });

        let mut codec = TermlandCodec;
        let mut buf = BytesMut::new();

        Encoder::encode(&mut codec, msg.clone(), &mut buf).unwrap();

        let decoded = codec.decode(&mut buf).unwrap().unwrap();
        match decoded {
            Message::Hello(h) => {
                assert_eq!(h.protocol_version, 1);
                assert_eq!(h.client_name, "test-client");
            }
            other => panic!("expected Hello, got {:?}", other),
        }
    }

    #[test]
    fn roundtrip_video_frame() {
        let msg = Message::VideoFrame(crate::VideoFrame {
            timestamp_us: 12345,
            frame_type: crate::FrameType::Keyframe,
            width: 1920,
            height: 1080,
            data: vec![0xDE, 0xAD, 0xBE, 0xEF],
        });

        let mut codec = TermlandCodec;
        let mut buf = BytesMut::new();

        Encoder::encode(&mut codec, msg, &mut buf).unwrap();

        let decoded = codec.decode(&mut buf).unwrap().unwrap();
        match decoded {
            Message::VideoFrame(vf) => {
                assert_eq!(vf.timestamp_us, 12345);
                assert_eq!(vf.width, 1920);
                assert_eq!(vf.data, vec![0xDE, 0xAD, 0xBE, 0xEF]);
            }
            other => panic!("expected VideoFrame, got {:?}", other),
        }
    }

    #[test]
    fn partial_read() {
        let msg = Message::Ping(crate::Ping { timestamp_us: 42 });

        let mut codec = TermlandCodec;
        let mut full_buf = BytesMut::new();
        Encoder::encode(&mut codec, msg, &mut full_buf).unwrap();

        // Feed only half the bytes
        let half = full_buf.len() / 2;
        let mut partial = BytesMut::from(&full_buf[..half]);

        assert!(codec.decode(&mut partial).unwrap().is_none());

        // Feed the rest
        partial.extend_from_slice(&full_buf[half..]);
        let decoded = codec.decode(&mut partial).unwrap().unwrap();
        match decoded {
            Message::Ping(p) => assert_eq!(p.timestamp_us, 42),
            other => panic!("expected Ping, got {:?}", other),
        }
    }
}
