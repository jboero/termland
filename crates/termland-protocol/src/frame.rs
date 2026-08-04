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
    fn roundtrip_text_input() {
        // Mixed BMP, astral-plane (emoji) and CJK content, since the whole point
        // of this message is codepoints the scancode path cannot express.
        let text = "héllo 世界 🎉";
        let msg = Message::TextInput(crate::TextInput { text: text.into() });

        let mut codec = TermlandCodec;
        let mut buf = BytesMut::new();

        Encoder::encode(&mut codec, msg, &mut buf).unwrap();
        assert_eq!(buf[2], crate::MessageId::TextInput as u8);

        let decoded = codec.decode(&mut buf).unwrap().unwrap();
        match decoded {
            Message::TextInput(ti) => assert_eq!(ti.text, text),
            other => panic!("expected TextInput, got {:?}", other),
        }
    }

    #[test]
    fn roundtrip_video_frame() {
        let msg = Message::VideoFrame(crate::VideoFrame {
            timestamp_us: 12345,
            frame_type: crate::FrameType::Keyframe,
            width: 1920,
            height: 1080,
            codec: Some(crate::VideoCodec::Av1),
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
    fn roundtrip_cursor_update() {
        // Non-trivial hotspot/dimensions and a non-empty image, since the
        // whole point of this message is carrying an actual cursor bitmap
        // (not just position) from server to client.
        let msg = Message::CursorUpdate(crate::CursorUpdate {
            x: 640,
            y: 360,
            hotspot_x: 3,
            hotspot_y: 1,
            width: 16,
            height: 16,
            visible: true,
            image_rgba: [0u8, 128, 255, 255].repeat(16 * 16),
        });

        let mut codec = TermlandCodec;
        let mut buf = BytesMut::new();

        Encoder::encode(&mut codec, msg, &mut buf).unwrap();
        assert_eq!(buf[2], crate::MessageId::CursorUpdate as u8);

        let decoded = codec.decode(&mut buf).unwrap().unwrap();
        match decoded {
            Message::CursorUpdate(cu) => {
                assert_eq!(cu.x, 640);
                assert_eq!(cu.y, 360);
                assert_eq!(cu.hotspot_x, 3);
                assert_eq!(cu.hotspot_y, 1);
                assert_eq!(cu.width, 16);
                assert_eq!(cu.height, 16);
                assert!(cu.visible);
                assert_eq!(cu.image_rgba.len(), 16 * 16 * 4);
            }
            other => panic!("expected CursorUpdate, got {:?}", other),
        }
    }

    #[test]
    fn roundtrip_file_transfer() {
        // Two files with a mix of plain and unicode/space-containing names,
        // since name-handling (not just byte-blob transport) is the whole
        // point of this message over the older single-blob ClipboardPayload.
        let msg = Message::FileTransferSend(crate::FileTransferPayload {
            files: vec![
                crate::FileEntry {
                    name: "report.pdf".into(),
                    data: vec![0x25, 0x50, 0x44, 0x46], // "%PDF"
                },
                crate::FileEntry {
                    name: "üñïçødé notes.txt".into(),
                    data: b"hello world".to_vec(),
                },
            ],
        });

        let mut codec = TermlandCodec;
        let mut buf = BytesMut::new();

        Encoder::encode(&mut codec, msg, &mut buf).unwrap();
        assert_eq!(buf[2], crate::MessageId::FileTransferSend as u8);

        let decoded = codec.decode(&mut buf).unwrap().unwrap();
        match decoded {
            Message::FileTransferSend(ft) => {
                assert_eq!(ft.files.len(), 2);
                assert_eq!(ft.files[0].name, "report.pdf");
                assert_eq!(ft.files[0].data, vec![0x25, 0x50, 0x44, 0x46]);
                assert_eq!(ft.files[1].name, "üñïçødé notes.txt");
                assert_eq!(ft.files[1].data, b"hello world".to_vec());
            }
            other => panic!("expected FileTransferSend, got {:?}", other),
        }
    }

    #[test]
    fn roundtrip_file_transfer_data_server_to_client() {
        // Same payload shape, but the server->client message id
        // (FileTransferData) - verifies both directions get distinct ids
        // and both decode correctly, mirroring ClipboardData/ClipboardSend.
        let msg = Message::FileTransferData(crate::FileTransferPayload {
            files: vec![crate::FileEntry { name: "a.txt".into(), data: vec![1, 2, 3] }],
        });

        let mut codec = TermlandCodec;
        let mut buf = BytesMut::new();

        Encoder::encode(&mut codec, msg, &mut buf).unwrap();
        assert_eq!(buf[2], crate::MessageId::FileTransferData as u8);

        let decoded = codec.decode(&mut buf).unwrap().unwrap();
        match decoded {
            Message::FileTransferData(ft) => {
                assert_eq!(ft.files.len(), 1);
                assert_eq!(ft.files[0].name, "a.txt");
                assert_eq!(ft.files[0].data, vec![1, 2, 3]);
            }
            other => panic!("expected FileTransferData, got {:?}", other),
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
