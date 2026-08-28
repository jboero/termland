//! Termland browser client, in Rust.
//!
//! A parallel implementation to `web/src` (TypeScript): same server, same
//! protocol, same WebTransport listener. The difference is where the logic
//! lives — the framing, the session state machine, the keymap and the video
//! pump are Rust compiled to wasm, sharing `termland-protocol` with the
//! server instead of restating it.
//!
//! What is *not* different: the browser APIs. WebTransport, WebCodecs, canvas
//! and DOM events are reached through wasm-bindgen, which generates
//! JavaScript glue for every one of those calls. This removes the
//! hand-written TypeScript layer, not the JavaScript one.

mod transport;
mod input;
mod session;
mod video;

pub use session::WebClient;
pub use transport::decode_cert_hash;
