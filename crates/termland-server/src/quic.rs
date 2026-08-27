//! QUIC transport — Q2 (split video/audio planes).
//!
//! Per `docs/quic-transport.md`'s phasing, Q1 treated QUIC purely as a
//! drop-in *byte* transport: the entire protocol (control messages, video,
//! audio, input, clipboard, cursor) ran unmodified over a single
//! bidirectional QUIC stream. Q2 — implemented here — moves video and audio
//! off that shared stream onto their own QUIC objects, so a lost/slow video
//! packet no longer head-of-line-blocks control/input messages behind it
//! (which is the whole reason QUIC was worth adopting over TCP in the first
//! place; Q1 alone didn't actually buy HOL-blocking freedom *within* one
//! connection, only *across* connections).
//!
//! There is exactly one QUIC client in existence (`termland-mobile-core`) and
//! no third party depends on Q1's single-stream wire contract, so Q2
//! *replaces* it outright rather than versioning or negotiating between the
//! two — `--quic` now means Q2, full stop.
//!
//! ## Plane layout
//!
//! - **Control** (Hello/auth/`Session*`/resize/cursor mode/clipboard/input —
//!   everything except video and audio): unchanged. One client-opened bidi
//!   stream, the same CBOR `TermlandCodec` framing `handle_session` already
//!   speaks over TCP/TLS/SSH. This is what lets Q2 reuse `handle_session`'s
//!   entire control/session lifecycle unmodified.
//! - **Video**: one server-opened **uni** stream, reliable (ordinary QUIC
//!   stream delivery — not datagrams). Framed with a lightweight *fixed*
//!   binary header (see `video_header_bytes`) rather than a CBOR `Message`,
//!   because this stream only ever carries one shape of message — one frame
//!   after another — so there's nothing for a tagged/flexible envelope to
//!   buy here, only per-frame overhead at 30fps. Deliberately reliable, not
//!   datagram-fragmented: splitting frames across datagrams so a lost
//!   fragment only costs one frame (Moonlight-style FEC/pacing) is real added
//!   complexity around loss-tolerant keyframe/interframe reconstruction that
//!   `docs/quic-transport.md` explicitly earmarks as a later refinement, not
//!   this change.
//! - **Audio**: QUIC **datagrams**, one complete Opus chunk per datagram.
//!   Safe without fragmentation logic: a 20ms@48kHz-stereo Opus frame is
//!   roughly 100-200 bytes, comfortably under any real-world QUIC datagram
//!   MTU (~1200 bytes after QUIC/UDP/IP overhead) — but the send path still
//!   checks the connection's actual negotiated `max_datagram_size` and
//!   drops+logs rather than risk truncating/corrupting a chunk that
//!   somehow doesn't fit, or panicking on a peer that disabled datagrams.
//!
//! What plain QUIC (Q1's contribution) still buys underneath all of this:
//! connection migration (the QUIC connection ID survives the client's IP
//! changing, e.g. Wi-Fi to cellular), faster reconnect than a fresh TCP+TLS
//! handshake, and no head-of-line blocking against *other* QUIC connections
//! sharing the same UDP socket.
//!
//! Client contract: immediately after the QUIC/TLS handshake completes, the
//! client opens the control bidi stream (`open_bi()`) exactly as in Q1, then
//! separately `accept_uni()`s the server-opened video stream (opened lazily
//! by `run_session` — see its doc comment for why not eagerly here) and
//! starts reading datagrams for audio. This listener calls `accept_bi()`
//! exactly once per connection for the control stream; the video uni stream
//! is opened from the server side, not accepted from the client.

use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result};

/// ALPN identifier for the termland QUIC transport. QUIC requires ALPN — it's
/// how the handshake picks a protocol and defends against version-downgrade
/// attacks — so this has to be set on both client and server even though Q1
/// only ever runs one thing over the connection.
const ALPN: &[u8] = b"termland/1";

const MAX_CONCURRENT_SESSIONS: usize = 32;

/// Run the server as a QUIC listener (UDP). Mirrors `run_tcp_listener`'s
/// structure — semaphore-bounded concurrent sessions, same
/// `MAX_CONCURRENT_SESSIONS`, same connection/rejection logging — but the
/// accept loop is two-level: `endpoint.accept()` yields an incoming
/// connection attempt before the handshake completes, so a slow or hostile
/// handshake on one connection can't stall accepting the next. The handshake
/// itself, and the single `accept_bi()` call per Q1's contract above, happen
/// inside the spawned per-connection task.
///
/// QUIC always requires TLS 1.3 (there is no plaintext QUIC), so this reuses
/// the exact same cert-loading path as `--tls` (`tls::load_or_generate_cert`
/// via `tls::build_rustls_server_config`) rather than a separate
/// implementation: pass `--tls-cert`/`--tls-key`, or let it auto-generate a
/// self-signed cert in `/etc/pki/termland/` just like the TCP+TLS acceptor
/// does.
pub async fn run_quic_listener(
    bind: &str,
    port: u16,
    cert_path: Option<&Path>,
    key_path: Option<&Path>,
    require_auth: bool,
) -> Result<()> {
    let server_config = build_quic_server_config(cert_path, key_path)?;
    let addr: SocketAddr = format!("{bind}:{port}")
        .parse()
        .with_context(|| format!("invalid QUIC bind address {bind}:{port}"))?;
    let endpoint = quinn::Endpoint::server(server_config, addr)
        .with_context(|| format!("failed to bind QUIC/UDP {addr}"))?;

    tracing::info!("Listening on {addr} (QUIC/UDP, max {MAX_CONCURRENT_SESSIONS} sessions)");

    let semaphore = Arc::new(tokio::sync::Semaphore::new(MAX_CONCURRENT_SESSIONS));

    while let Some(incoming) = endpoint.accept().await {
        let remote = incoming.remote_address();

        let permit = match semaphore.clone().try_acquire_owned() {
            Ok(p) => p,
            Err(_) => {
                tracing::warn!("Rejected QUIC connection from {remote}: max sessions reached");
                incoming.refuse();
                continue;
            }
        };

        tracing::info!(
            "QUIC connection attempt from {remote} ({} active)",
            MAX_CONCURRENT_SESSIONS - semaphore.available_permits()
        );
        let auth = require_auth;
        tokio::spawn(async move {
            if let Err(e) = handle_quic_connection(incoming, auth).await {
                tracing::error!("QUIC session error for {remote}: {e}");
            }
            drop(permit);
        });
    }

    Ok(())
}

/// Complete the QUIC handshake, open the control bidi stream, and hand both
/// it and the raw `Connection` to the transport-agnostic `handle_session`.
///
/// Deliberately does NOT open the video uni stream here: `handle_session`
/// doesn't yet know whether this connection will even reach a live session
/// (Hello/auth/SessionCreate all still have to happen first, and any of them
/// can fail or the client can just be listing/closing sessions and never
/// stream at all). `run_session` opens the video stream itself, lazily,
/// right when the encoder has produced its first frame — see that call
/// site's doc comment. This function's job is just to get the `Connection`
/// handle down to where that decision gets made.
async fn handle_quic_connection(incoming: quinn::Incoming, require_auth: bool) -> Result<()> {
    let connection = incoming.await.context("QUIC handshake failed")?;
    let remote = connection.remote_address();
    tracing::info!("QUIC handshake complete for {remote}");

    // Client contract: the client opens exactly one bidi stream right after
    // the handshake, and that stream carries the control plane for the
    // entire session (unchanged from Q1).
    let (send, recv) = connection
        .accept_bi()
        .await
        .context("client did not open a bidirectional stream")?;

    // quinn splits a bidi stream into separate read/write halves; combine
    // them back into one AsyncRead+AsyncWrite the same way `run_subsystem`
    // already combines stdin/stdout for the SSH-subsystem entry point.
    let io = tokio::io::join(recv, send);
    crate::transport::handle_session(
        io,
        require_auth,
        crate::media::MediaConnection::Quic(connection),
    )
    .await
}

// Q2 header encode/decode lives in `termland-protocol::q2` so the TypeScript
// client, the wasm spike, and this listener cannot drift. Tests below import
// those helpers from the protocol crate directly.

#[cfg(test)]
mod header_tests {
    use termland_protocol::{
        audio_header_bytes, video_header_bytes, AUDIO_HEADER_LEN, FrameType, VIDEO_HEADER_LEN,
        VideoCodec,
    };

    /// Mirrors the client-side decoder this header must match: unpack the 18
    /// bytes back into fields the same way `termland-mobile-core`'s
    /// `quic_video_reader` does, and check they round-trip. Kept local to
    /// this test (not exposed as production code) since the server itself
    /// never needs to decode a header it always originates.
    fn decode_video_header(buf: &[u8; VIDEO_HEADER_LEN]) -> (u8, u8, u16, u16, u64, u32) {
        let codec = buf[0];
        let frame_type = buf[1];
        let width = u16::from_le_bytes([buf[2], buf[3]]);
        let height = u16::from_le_bytes([buf[4], buf[5]]);
        let timestamp_us = u64::from_le_bytes(buf[6..14].try_into().unwrap());
        let data_len = u32::from_le_bytes(buf[14..18].try_into().unwrap());
        (codec, frame_type, width, height, timestamp_us, data_len)
    }

    #[test]
    fn video_header_round_trips_all_codecs_and_frame_types() {
        for (codec, tag) in [
            (VideoCodec::Av1, 0u8),
            (VideoCodec::Vp9, 1),
            (VideoCodec::Vp8, 2),
            (VideoCodec::H265, 3),
            (VideoCodec::H264, 4),
        ] {
            for (frame_type, ft_tag) in [(FrameType::Inter, 0u8), (FrameType::Keyframe, 1u8)] {
                let header = video_header_bytes(codec, frame_type, 1920, 1080, 123_456_789_012, 65536);
                assert_eq!(header.len(), VIDEO_HEADER_LEN);
                let (c, ft, w, h, ts, len) = decode_video_header(&header);
                assert_eq!(c, tag, "codec tag for {codec:?}");
                assert_eq!(ft, ft_tag, "frame_type tag for {frame_type:?}");
                assert_eq!(w, 1920);
                assert_eq!(h, 1080);
                assert_eq!(ts, 123_456_789_012);
                assert_eq!(len, 65536);
            }
        }
    }

    #[test]
    fn video_header_byte_offsets_are_exact() {
        // Pin the exact byte layout, not just round-trip-through-our-own-decoder:
        // a future refactor of `video_header_bytes` that still round-trips
        // through a matching decode function could still silently break wire
        // compatibility with the client if both sides drifted together.
        let header = video_header_bytes(VideoCodec::H264, FrameType::Keyframe, 0x0102, 0x0304, 0x0102030405060708, 0xAABBCCDD);
        assert_eq!(header[0], 4); // H264 tag
        assert_eq!(header[1], 1); // Keyframe tag
        assert_eq!(&header[2..4], &0x0102u16.to_le_bytes());
        assert_eq!(&header[4..6], &0x0304u16.to_le_bytes());
        assert_eq!(&header[6..14], &0x0102030405060708u64.to_le_bytes());
        assert_eq!(&header[14..18], &0xAABBCCDDu32.to_le_bytes());
    }

    #[test]
    fn audio_header_layout_is_exact() {
        let header = audio_header_bytes(48000, 2);
        assert_eq!(header.len(), AUDIO_HEADER_LEN);
        assert_eq!(&header[0..4], &48000u32.to_le_bytes());
        assert_eq!(header[4], 2);
    }
}

/// Build quinn's `ServerConfig` around an rustls `ServerConfig` sharing cert
/// loading with the TCP+TLS acceptor (`tls::build_rustls_server_config`).
fn build_quic_server_config(
    cert_path: Option<&Path>,
    key_path: Option<&Path>,
) -> Result<quinn::ServerConfig> {
    let mut crypto = crate::tls::build_rustls_server_config(cert_path, key_path)?;
    crypto.alpn_protocols = vec![ALPN.to_vec()];

    let quic_crypto = quinn::crypto::rustls::QuicServerConfig::try_from(crypto)
        .context("rustls config is not usable for QUIC (needs TLS 1.3)")?;

    Ok(quinn::ServerConfig::with_crypto(Arc::new(quic_crypto)))
}
