//! QUIC transport — Q1 only.
//!
//! Per `docs/quic-transport.md`'s phasing, Q1 treats QUIC purely as a
//! drop-in *byte* transport: the entire existing protocol (control messages,
//! video, audio, input, clipboard, cursor — everything `handle_session`
//! already does) runs unmodified over a single bidirectional QUIC stream per
//! connection. That's the whole point of the change: `handle_session` is
//! already generic over `T: AsyncRead + AsyncWrite + Unpin`, so a QUIC
//! stream just needs to satisfy that bound and nothing downstream has to
//! know QUIC exists. Splitting video/audio onto their own
//! streams/datagrams for head-of-line-blocking-free delivery is Q2 — out of
//! scope here, and deliberately not attempted.
//!
//! What Q1 buys anyway, for free, just from swapping the byte transport:
//! connection migration (the QUIC connection ID survives the client's IP
//! changing, e.g. Wi-Fi to cellular), faster reconnect than a fresh TCP+TLS
//! handshake, and no head-of-line blocking against *other* QUIC connections
//! sharing the same UDP socket.
//!
//! Client contract: immediately after the QUIC/TLS handshake completes, the
//! client opens exactly one bidirectional stream and speaks the same CBOR
//! `TermlandCodec` framing over it that it would over a fresh TCP
//! connection. This listener calls `accept_bi()` exactly once per
//! connection; if a client opens additional streams they are simply never
//! read (there is nothing for them to carry yet — that's reserved for Q2).

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
/// self-signed cert in `~/.config/termland/` just like the TCP+TLS acceptor
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

/// Complete the QUIC handshake, open the one Q1 bidi stream, and hand it to
/// the transport-agnostic `handle_session` unchanged.
async fn handle_quic_connection(incoming: quinn::Incoming, require_auth: bool) -> Result<()> {
    let connection = incoming.await.context("QUIC handshake failed")?;
    let remote = connection.remote_address();
    tracing::info!("QUIC handshake complete for {remote}");

    // Q1 contract: the client opens exactly one bidi stream right after the
    // handshake, and that stream carries the entire session.
    let (send, recv) = connection
        .accept_bi()
        .await
        .context("client did not open a bidirectional stream")?;

    // quinn splits a bidi stream into separate read/write halves; combine
    // them back into one AsyncRead+AsyncWrite the same way `run_subsystem`
    // already combines stdin/stdout for the SSH-subsystem entry point.
    let io = tokio::io::join(recv, send);
    crate::transport::handle_session(io, require_auth).await
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
