//! WebTransport plumbing: open a session, own the control stream, and turn it
//! into `Message` in and `Message` out.
//!
//! The browser's WebTransport object is reached through `web-sys`, which needs
//! `--cfg=web_sys_unstable_apis` (see `.cargo/config.toml`). Everything above
//! this module works in `Message`, never in bytes.

use bytes::BytesMut;
use js_sys::{Object, Reflect, Uint8Array};
use termland_protocol::frame::CodecError;
use termland_protocol::TermlandCodec;
use termland_protocol::Message;
use tokio_util::codec::Decoder;
use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_futures::JsFuture;
use web_sys::{
    ReadableStreamDefaultReader, WebTransport, WebTransportBidirectionalStream, WebTransportHash,
    WebTransportOptions, WritableStreamDefaultWriter,
};

/// Parse the dotted-hex SHA-256 the server logs for its development
/// certificate. Accepts `aa:bb:..`, `aa-bb-..`, spaces, or none of those.
///
/// Returns `None` rather than a partial digest: a 31-byte hash is not a
/// near-miss, it is a different certificate.
pub fn decode_cert_hash(s: &str) -> Option<Vec<u8>> {
    let cleaned: String = s
        .chars()
        .filter(|c| !c.is_whitespace() && *c != ':' && *c != '-')
        .collect();
    if cleaned.len() != 64 || !cleaned.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    (0..32)
        .map(|i| u8::from_str_radix(&cleaned[i * 2..i * 2 + 2], 16).ok())
        .collect()
}

/// Open a WebTransport session, pinning the certificate hash when one is given.
///
/// A hash is only accepted by browsers for certificates valid two weeks or
/// less, which is why the server mints a separate short-lived one rather than
/// reusing its ordinary keypair.
pub async fn open(url: &str, cert_hash_hex: Option<&str>) -> Result<WebTransport, JsValue> {
    let transport = match cert_hash_hex.map(str::trim).filter(|s| !s.is_empty()) {
        None => WebTransport::new(url)?,
        Some(hex) => {
            let bytes = decode_cert_hash(hex).ok_or_else(|| {
                JsValue::from_str("certificate hash must be 32 bytes of hex (SHA-256)")
            })?;
            let hash = WebTransportHash::new();
            hash.set_algorithm("sha-256");
            hash.set_value_u8_array(&Uint8Array::from(bytes.as_slice()));
            let opts = WebTransportOptions::new();
            opts.set_server_certificate_hashes(&[hash]);
            WebTransport::new_with_options(url, &opts)?
        }
    };
    JsFuture::from(transport.ready()).await?;
    Ok(transport)
}

/// Write half of the control stream.
///
/// Cloneable, so input handlers can send without reaching the read loop. A
/// `WritableStream` queues `write()` calls in the order they are made, so
/// concurrent senders cannot interleave halfway through a frame.
#[derive(Clone)]
pub struct Outbox {
    writer: WritableStreamDefaultWriter,
}

impl Outbox {
    /// Queue a message. Errors are logged rather than returned: every caller
    /// is a DOM event handler with nowhere to propagate to, and a failed write
    /// means the transport is already gone, which the read loop will see.
    pub fn send(&self, msg: &Message) {
        match encode_wire(msg) {
            Err(e) => web_sys::console::error_1(&JsValue::from_str(&format!(
                "dropping {:?}: {e}",
                msg.message_id()
            ))),
            Ok(bytes) => {
                let promise = self.writer.write_with_chunk(&Uint8Array::from(bytes.as_slice()));
                let id = msg.message_id();
                wasm_bindgen_futures::spawn_local(async move {
                    if let Err(e) = JsFuture::from(promise).await {
                        web_sys::console::warn_2(
                            &JsValue::from_str(&format!("control write failed ({id:?}):")),
                            &e,
                        );
                    }
                });
            }
        }
    }
}

/// Read half of the control stream.
pub struct Inbox {
    reader: ReadableStreamDefaultReader,
    codec: TermlandCodec,
    buf: BytesMut,
}

/// Open the client's control stream and split it into the two halves.
pub async fn control_stream(transport: &WebTransport) -> Result<(Outbox, Inbox), JsValue> {
    let stream: WebTransportBidirectionalStream =
        JsFuture::from(transport.create_bidirectional_stream())
            .await?
            .unchecked_into();
    let outbox = Outbox {
        writer: stream.writable().get_writer()?,
    };
    let inbox = Inbox {
        reader: stream.readable().get_reader().unchecked_into(),
        codec: TermlandCodec,
        buf: BytesMut::new(),
    };
    Ok((outbox, inbox))
}

impl Inbox {
    /// Next complete message, or `Ok(None)` once the peer finishes the stream.
    ///
    /// Decoding is `TermlandCodec` — the same decoder the server runs — so a
    /// split frame, a bad magic and the 16 MiB payload cap all behave here
    /// exactly as they do on the other end.
    pub async fn next(&mut self) -> Result<Option<Message>, JsValue> {
        loop {
            match self.codec.decode(&mut self.buf) {
                Ok(Some(msg)) => return Ok(Some(msg)),
                Ok(None) => {}
                Err(e) => return Err(JsValue::from_str(&codec_error(&e))),
            }
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
    }
}

fn codec_error(e: &CodecError) -> String {
    format!("control stream: {e}")
}

/// `[magic "TL"][msg id][len u32 LE][CBOR]`, matching `TermlandCodec`'s
/// encoder on the server. Written by hand rather than driven through the
/// `Encoder` impl so this does not need a `Framed` sink over a JS stream.
fn encode_wire(msg: &Message) -> Result<Vec<u8>, String> {
    let payload = msg.encode().map_err(|e| e.to_string())?;
    if payload.len() > termland_protocol::MAX_PAYLOAD_SIZE as usize {
        return Err(format!("payload too large: {} bytes", payload.len()));
    }
    let mut out = Vec::with_capacity(7 + payload.len());
    out.extend_from_slice(&termland_protocol::FRAME_MAGIC);
    out.push(msg.message_id() as u8);
    out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    out.extend_from_slice(&payload);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use termland_protocol::{Ping, PROTOCOL_VERSION, Hello};

    #[test]
    fn cert_hash_accepts_the_servers_dotted_hex() {
        let dotted = (0..32)
            .map(|i| format!("{:02x}", i))
            .collect::<Vec<_>>()
            .join(":");
        let parsed = decode_cert_hash(&dotted).expect("dotted hex");
        assert_eq!(parsed, (0u8..32).collect::<Vec<_>>());
    }

    #[test]
    fn cert_hash_accepts_bare_and_dashed_forms() {
        let bare = "00".repeat(32);
        assert_eq!(decode_cert_hash(&bare), Some(vec![0u8; 32]));
        let dashed = vec!["ff"; 32].join("-");
        assert_eq!(decode_cert_hash(&dashed), Some(vec![0xffu8; 32]));
    }

    #[test]
    fn a_short_or_invalid_hash_is_rejected_outright() {
        assert_eq!(decode_cert_hash(""), None);
        assert_eq!(decode_cert_hash(&"00".repeat(31)), None, "31 bytes");
        assert_eq!(decode_cert_hash(&"00".repeat(33)), None, "33 bytes");
        assert_eq!(decode_cert_hash(&"zz".repeat(32)), None, "not hex");
    }

    #[test]
    fn wire_frame_matches_the_servers_layout() {
        let msg = Message::Ping(Ping { timestamp_us: 42 });
        let bytes = encode_wire(&msg).unwrap();
        assert_eq!(&bytes[0..2], b"TL");
        assert_eq!(bytes[2], msg.message_id() as u8);
        let len = u32::from_le_bytes(bytes[3..7].try_into().unwrap()) as usize;
        assert_eq!(len, bytes.len() - 7);
        assert_eq!(Message::decode(&bytes[7..]).unwrap().message_id(), msg.message_id());
    }

    /// What `ControlStream::next` does, minus the JS stream: the same codec
    /// must reassemble a frame delivered in arbitrary pieces.
    #[test]
    fn codec_reassembles_a_frame_split_across_chunks() {
        let msg = Message::Hello(Hello {
            protocol_version: PROTOCOL_VERSION,
            client_name: "termland-wasm".into(),
        });
        let bytes = encode_wire(&msg).unwrap();
        let mut codec = TermlandCodec;
        let mut buf = BytesMut::new();
        for chunk in bytes.chunks(3) {
            buf.extend_from_slice(chunk);
            if let Some(got) = codec.decode(&mut buf).unwrap() {
                assert_eq!(got.message_id(), msg.message_id());
                assert!(buf.is_empty(), "decoder left bytes behind");
                return;
            }
        }
        panic!("frame never completed");
    }
}
