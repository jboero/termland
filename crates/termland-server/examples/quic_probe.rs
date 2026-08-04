//! Manual, ad-hoc probe for the QUIC listener: connects to a running
//! `termland-server --quic` instance and does one real Hello/HelloAck round
//! trip. Not part of the automated suite (see `tests/quic_handshake.rs` for
//! that) — this is for driving an already-running server by hand, e.g.
//! `termland-server --quic --quic-port 17867 &` then
//! `cargo run --release --example quic_probe -- 127.0.0.1 17867`.

use std::sync::Arc;

use futures::{SinkExt, StreamExt};
use tokio_util::codec::Framed;

use termland_protocol::{Hello, Message, TermlandCodec, PROTOCOL_VERSION};

const ALPN: &[u8] = b"termland/1";

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

#[tokio::main]
async fn main() {
    let mut args = std::env::args().skip(1);
    let host = args.next().unwrap_or_else(|| "127.0.0.1".into());
    let port: u16 = args.next().unwrap_or_else(|| "17867".into()).parse().expect("port");

    println!("[probe] connecting to {host}:{port} over QUIC (ALPN={:?})", String::from_utf8_lossy(ALPN));

    let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
    let mut crypto = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .expect("protocol versions")
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(AcceptAnyCert))
        .with_no_client_auth();
    crypto.alpn_protocols = vec![ALPN.to_vec()];

    let quic_crypto = quinn::crypto::rustls::QuicClientConfig::try_from(crypto).expect("rustls->quic config");
    let client_config = quinn::ClientConfig::new(Arc::new(quic_crypto));

    let mut endpoint = quinn::Endpoint::client("0.0.0.0:0".parse().unwrap()).expect("bind local UDP socket");
    endpoint.set_default_client_config(client_config);

    let addr: std::net::SocketAddr = format!("{host}:{port}").parse().expect("parse addr");
    let connecting = endpoint.connect(addr, "localhost").expect("start connect");
    let connection = connecting.await.expect("QUIC handshake failed");
    println!("[probe] QUIC handshake COMPLETE, remote={}", connection.remote_address());

    let (send, recv) = connection.open_bi().await.expect("open bidi stream");
    println!("[probe] bidi stream opened");
    let mut framed = Framed::new(tokio::io::join(recv, send), TermlandCodec);

    framed
        .send(Message::Hello(Hello { protocol_version: PROTOCOL_VERSION, client_name: "quic-probe".into() }))
        .await
        .expect("send Hello");
    println!("[probe] sent Message::Hello {{ protocol_version: {PROTOCOL_VERSION}, client_name: \"quic-probe\" }}");

    match framed.next().await {
        Some(Ok(Message::HelloAck(ack))) => {
            println!("[probe] received Message::HelloAck: {ack:?}");
            println!("[probe] RESULT: PASS - live QUIC handshake + Hello/HelloAck round trip succeeded");
        }
        Some(Ok(other)) => {
            println!("[probe] RESULT: FAIL - expected HelloAck, got {other:?}");
            std::process::exit(1);
        }
        Some(Err(e)) => {
            println!("[probe] RESULT: FAIL - decode error: {e}");
            std::process::exit(1);
        }
        None => {
            println!("[probe] RESULT: FAIL - connection closed before HelloAck");
            std::process::exit(1);
        }
    }

    endpoint.close(0u32.into(), b"probe done");
}
