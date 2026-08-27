//! Termland's browser client core, compiled to WebAssembly.
//!
//! # Why wasm rather than a JavaScript protocol library
//!
//! The obvious way to build a browser client is to reimplement the wire format
//! in JavaScript — the 7-byte `TL` frame header, serde's externally-tagged
//! CBOR, the codec negotiation rules — and then keep that reimplementation in
//! step with the Rust one forever. That is a second source of truth, and the
//! usual mitigation (cross-language fixtures pinning the bytes) only tells you
//! *after* they have already diverged.
//!
//! This crate takes the other route: `termland-protocol` compiles to wasm, so
//! the browser runs the same `TermlandCodec`, the same `Message` enum and the
//! same serde derives the server does. There is nothing to keep in step. A
//! protocol change is a recompile, not a port.
//!
//! What is left for JavaScript is the part that genuinely is browser API
//! surface — obtaining a `WebTransport`, handing frames to `VideoDecoder`,
//! painting a canvas — and that is a thin shell over this core.
//!
//! # Scope
//!
//! This crate is the original Hello-only wasm spike. The product browser
//! client is TypeScript in `web/` (see `docs/webtransport.md`). `connect()`
//! still completes Hello/HelloAck against `--webtransport`.

use bytes::BytesMut;
use futures::StreamExt;
use js_sys::Uint8Array;
use termland_protocol::{Hello, Message, TermlandCodec, PROTOCOL_VERSION};
use tokio_util_codec_shim::{Decoder, Encoder};
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::JsFuture;
use web_sys::{WebTransport, WebTransportBidirectionalStream, WebTransportHash, WebTransportOptions};

/// `tokio-util`'s `Decoder`/`Encoder` traits, re-exported by the protocol
/// crate. They are plain buffer transforms with no async runtime behind them,
/// which is why they work unchanged in a browser.
mod tokio_util_codec_shim {
    pub use tokio_util::codec::{Decoder, Encoder};
}

/// What the server told us about itself, handed back to JavaScript.
#[wasm_bindgen]
pub struct ServerGreeting {
    protocol_version: u32,
    server_name: String,
    session_id: String,
    auth_required: bool,
}

#[wasm_bindgen]
impl ServerGreeting {
    #[wasm_bindgen(getter)]
    pub fn protocol_version(&self) -> u32 {
        self.protocol_version
    }
    #[wasm_bindgen(getter)]
    pub fn server_name(&self) -> String {
        self.server_name.clone()
    }
    #[wasm_bindgen(getter)]
    pub fn session_id(&self) -> String {
        self.session_id.clone()
    }
    #[wasm_bindgen(getter)]
    pub fn auth_required(&self) -> bool {
        self.auth_required
    }
}

/// Connect to a Termland WebTransport listener and complete the handshake.
///
/// `cert_hash_hex`, when given, is the SHA-256 digest of the server's
/// certificate as printed by `termland-server --webtransport`. Browsers accept
/// it only for certificates valid two weeks or less, which is why the server
/// mints a short-lived one for this path rather than reusing its ordinary
/// long-lived self-signed certificate. Omit it when the server presents a
/// normally trusted certificate.
/// Install the panic hook. Called automatically when the module loads.
#[wasm_bindgen(start)]
pub fn start() {
    console_error_panic_hook::set_once();
}

#[wasm_bindgen]
pub async fn connect(url: &str, cert_hash_hex: Option<String>) -> Result<ServerGreeting, JsValue> {
    let step = |m: &str| web_sys::console::log_1(&JsValue::from_str(&format!("[termland-web] {m}")));

    step("opening transport");
    let transport = open_transport(url, cert_hash_hex.as_deref())?;
    step("awaiting ready");
    JsFuture::from(transport.ready()).await?;
    step("ready resolved");

    // Same contract every other Termland transport uses: the client opens one
    // bidirectional stream and it carries the control plane.
    let stream: WebTransportBidirectionalStream =
        JsFuture::from(transport.create_bidirectional_stream())
            .await?
            .into();
    step("bidi stream created");

    let writable = stream.writable();
    let writer = writable.get_writer()?;

    // Encode with the server's own codec, not a hand-rolled framing.
    let mut codec = TermlandCodec;
    let mut out = BytesMut::new();
    codec
        .encode(
            Message::Hello(Hello {
                protocol_version: PROTOCOL_VERSION,
                client_name: "termland-web".into(),
            }),
            &mut out,
        )
        .map_err(|e| JsValue::from_str(&format!("encoding Hello: {e}")))?;

    let chunk = Uint8Array::from(&out[..]);
    step("writing Hello");
    JsFuture::from(writer.write_with_chunk(&chunk)).await?;
    step("Hello written");

    step("reading HelloAck");
    let greeting = read_hello_ack(&stream).await?;
    step("HelloAck decoded");
    Ok(greeting)
}

/// Build the `WebTransport`, optionally pinning a certificate hash.
fn open_transport(url: &str, cert_hash_hex: Option<&str>) -> Result<WebTransport, JsValue> {
    let Some(hex) = cert_hash_hex else {
        return WebTransport::new(url);
    };

    let bytes = decode_hex(hex)
        .ok_or_else(|| JsValue::from_str("certificate hash is not valid hex"))?;
    if bytes.len() != 32 {
        return Err(JsValue::from_str("certificate hash must be 32 bytes (SHA-256)"));
    }

    let hash = WebTransportHash::new();
    hash.set_algorithm("sha-256");
    hash.set_value(&Uint8Array::from(&bytes[..]).into());

    let options = WebTransportOptions::new();
    options.set_server_certificate_hashes(&[hash]);
    WebTransport::new_with_options(url, &options)
}

/// Read from the control stream until a complete frame decodes.
///
/// The loop is here because a stream read returns whatever bytes have arrived,
/// not whole protocol frames; `TermlandCodec` is what knows where a frame ends,
/// exactly as on the server.
async fn read_hello_ack(stream: &WebTransportBidirectionalStream) -> Result<ServerGreeting, JsValue> {
    let readable = wasm_streams::ReadableStream::from_raw(stream.readable().unchecked_into());
    let mut reader = readable.into_stream();

    let mut codec = TermlandCodec;
    let mut buf = BytesMut::new();

    while let Some(chunk) = reader.next().await {
        let chunk: Uint8Array = chunk?.into();
        buf.extend_from_slice(&chunk.to_vec());

        match codec.decode(&mut buf) {
            Ok(Some(Message::HelloAck(ack))) => {
                return Ok(ServerGreeting {
                    protocol_version: ack.protocol_version,
                    server_name: ack.server_name,
                    session_id: ack.session_id,
                    auth_required: ack.auth_required,
                });
            }
            // A different message before HelloAck means the peer is not
            // speaking this protocol; say so rather than silently waiting.
            Ok(Some(other)) => {
                return Err(JsValue::from_str(&format!(
                    "expected HelloAck, got {:?}",
                    other.message_id()
                )));
            }
            Ok(None) => continue, // partial frame, keep reading
            Err(e) => return Err(JsValue::from_str(&format!("decoding HelloAck: {e}"))),
        }
    }

    Err(JsValue::from_str("stream closed before HelloAck arrived"))
}

/// Parse a hex string, tolerating the `aa:bb:cc` grouping some tools print.
fn decode_hex(s: &str) -> Option<Vec<u8>> {
    let cleaned: String = s
        .chars()
        .filter(|c| !c.is_whitespace() && *c != ':' && *c != '-')
        .collect();
    if cleaned.len() % 2 != 0 {
        return None;
    }
    (0..cleaned.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&cleaned[i..i + 2], 16).ok())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The hash is copied out of a terminal by a human, so the formats tools
    /// actually print have to work — colon-grouped included.
    #[test]
    fn hex_parsing_accepts_the_formats_people_paste() {
        assert_eq!(decode_hex("00ff10"), Some(vec![0x00, 0xff, 0x10]));
        assert_eq!(decode_hex("00:ff:10"), Some(vec![0x00, 0xff, 0x10]));
        assert_eq!(decode_hex("00 ff 10"), Some(vec![0x00, 0xff, 0x10]));
        assert_eq!(decode_hex("00-ff-10"), Some(vec![0x00, 0xff, 0x10]));
    }

    #[test]
    fn malformed_hex_is_rejected_rather_than_truncated() {
        assert_eq!(decode_hex("abc"), None, "odd length");
        assert_eq!(decode_hex("zz"), None, "not hex");
    }

    /// Guards the claim this crate exists to make: the browser encodes with
    /// the server's codec, so a frame built here is one the server decodes.
    #[test]
    fn hello_round_trips_through_the_shared_codec() {
        let mut codec = TermlandCodec;
        let mut buf = BytesMut::new();
        codec
            .encode(
                Message::Hello(Hello {
                    protocol_version: PROTOCOL_VERSION,
                    client_name: "termland-web".into(),
                }),
                &mut buf,
            )
            .expect("encode");

        match codec.decode(&mut buf).expect("decode") {
            Some(Message::Hello(h)) => {
                assert_eq!(h.protocol_version, PROTOCOL_VERSION);
                assert_eq!(h.client_name, "termland-web");
            }
            other => panic!("expected Hello, got {other:?}"),
        }
    }
}
