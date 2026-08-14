//! Live QUIC handshake test for Q1 (docs/quic-transport.md).
//!
//! This is the one piece of the QUIC transport that can't be verified by
//! reading the code: a real ALPN/TLS1.3 handshake, a real bidi stream open,
//! and a real `Message::Hello` / `Message::HelloAck` round trip over it,
//! using the same `quinn` client machinery the mobile core uses and the same
//! `TermlandCodec` framing the desktop client/server already use over TCP.
//!
//! Spawns the actual `termland-server` binary (via `CARGO_BIN_EXE_...`, so
//! this is the real compiled artifact, not an in-process shortcut) with
//! `--quic` and talks to it exactly like a client would.

use std::process::{Child, Command, Stdio};
use std::sync::Arc;

use futures::{SinkExt, StreamExt};
use tokio_util::codec::Framed;

use termland_protocol::{Hello, Message, TermlandCodec, PROTOCOL_VERSION};

/// Kills the child server process on drop so a failing assertion (which
/// unwinds before reaching an explicit `kill`) doesn't leak the process.
struct ChildGuard(Child);
impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Accepts any server certificate — this test's self-signed dev cert isn't
/// in any trust store. Mirrors `AcceptAnyCert` in
/// `termland-mobile-core`/`termland-client`, kept test-local rather than
/// shared since it's a handful of lines and this is the only place a test
/// needs it.
#[derive(Debug)]
struct AcceptAnyCert;

impl rustls::client::danger::ServerCertVerifier for AcceptAnyCert {
    fn verify_server_cert(
        &self,
        _: &rustls::pki_types::CertificateDer<'_>,
        _: &[rustls::pki_types::CertificateDer<'_>],
        _: &rustls::pki_types::ServerName<'_>,
        _: &[u8],
        _: rustls::pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }
    fn verify_tls12_signature(
        &self,
        _: &[u8],
        _: &rustls::pki_types::CertificateDer<'_>,
        _: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }
    fn verify_tls13_signature(
        &self,
        _: &[u8],
        _: &rustls::pki_types::CertificateDer<'_>,
        _: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }
    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        rustls::crypto::aws_lc_rs::default_provider()
            .signature_verification_algorithms
            .supported_schemes()
    }
}

/// Must match `crates/termland-server/src/quic.rs`'s ALPN exactly.
const ALPN: &[u8] = b"termland/1";

#[tokio::test]
async fn quic_hello_hello_ack_round_trip() {
    let port: u16 = 27867;

    // An explicit --port matters: without it the spawned server falls back
    // to the default 7867, which collides with any termland-server the
    // developer already has running — a normal state to be in — and the
    // bind failure kills the child before QUIC ever starts, surfacing as an
    // unrelated-looking handshake timeout.
    let bin = env!("CARGO_BIN_EXE_termland-server");
    eprintln!("[test] spawning {bin} --quic --quic-port {port}");
    let child = Command::new(bin)
        .args(["--bind", "127.0.0.1", "--port", "27866",
               "--quic", "--quic-port", &port.to_string()])
        .env("RUST_LOG", "info")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to spawn termland-server");
    let mut guard = ChildGuard(child);

    // Give the endpoint a moment to bind + start accepting. There's no
    // "ready" signal to poll for cross-process short of parsing log lines,
    // so a short fixed wait is the pragmatic choice here.
    tokio::time::sleep(std::time::Duration::from_millis(800)).await;

    // --- Build a quinn client with the dangerous-but-honest self-signed
    // --- posture (matches Transport::Quic's accept_invalid_certs=true path
    // --- in termland-mobile-core).
    let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
    let mut crypto = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .expect("protocol versions")
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(AcceptAnyCert))
        .with_no_client_auth();
    crypto.alpn_protocols = vec![ALPN.to_vec()];

    let quic_crypto = quinn::crypto::rustls::QuicClientConfig::try_from(crypto)
        .expect("rustls config usable for QUIC");
    let client_config = quinn::ClientConfig::new(Arc::new(quic_crypto));

    let mut endpoint = quinn::Endpoint::client("127.0.0.1:0".parse().unwrap())
        .expect("bind local QUIC socket");
    endpoint.set_default_client_config(client_config);

    eprintln!("[test] connecting to 127.0.0.1:{port} over QUIC");
    let connecting = endpoint
        .connect(format!("127.0.0.1:{port}").parse().unwrap(), "localhost")
        .expect("start QUIC connect");
    let connection = tokio::time::timeout(std::time::Duration::from_secs(10), connecting)
        .await
        .expect("QUIC handshake timed out")
        .expect("QUIC handshake failed");
    eprintln!(
        "[test] QUIC handshake COMPLETE — connection established to {}",
        connection.remote_address()
    );

    let (send, recv) = connection.open_bi().await.expect("open bidi stream");
    eprintln!("[test] bidi stream opened");
    let mut framed = Framed::new(tokio::io::join(recv, send), TermlandCodec);

    framed
        .send(Message::Hello(Hello {
            protocol_version: PROTOCOL_VERSION,
            client_name: "quic-handshake-test".into(),
        }))
        .await
        .expect("send Hello");
    eprintln!("[test] sent Message::Hello");

    let msg = tokio::time::timeout(std::time::Duration::from_secs(10), framed.next())
        .await
        .expect("timed out waiting for HelloAck")
        .expect("connection closed before HelloAck")
        .expect("decode error waiting for HelloAck");

    let ack = match msg {
        Message::HelloAck(ack) => ack,
        other => panic!("expected HelloAck, got {other:?}"),
    };
    eprintln!("[test] received Message::HelloAck: {ack:?}");

    assert_eq!(ack.protocol_version, PROTOCOL_VERSION);
    assert_eq!(ack.server_name, "termland-server");
    assert!(!ack.auth_required);

    eprintln!("[test] QUIC Q1 live handshake + Hello/HelloAck round trip: PASS");

    endpoint.close(0u32.into(), b"test done");
    let _ = guard.0.kill();
    let _ = guard.0.wait();

    // Surface server-side logs for debugging if anything above panicked
    // before this point (drop already ran, but stdout was piped so it's
    // gone either way) — nothing further to do; ChildGuard's Drop already
    // guarantees cleanup on early-return/panic paths too.
    let _ = ack;
}
