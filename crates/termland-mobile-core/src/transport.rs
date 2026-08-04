use std::sync::Arc;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::TcpStream;

use crate::error::{Result, TermlandError};
use crate::types::ServerProfile;

/// Any bidirectional byte stream the protocol can run over. Boxed so the
/// transport is chosen at runtime and the session loop stays generic.
pub trait Io: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Io for T {}

/// How to reach the server. Kept as an enum rather than hardcoding TLS so the
/// in-process SSH variant can be added without touching the session loop: every
/// variant just has to hand back a `Box<dyn Io>`.
///
/// TODO(M4): add `Ssh { opts: SshOptions }` — open an SSH connection with
/// `russh` and request the `termland` subsystem channel, then wrap the channel's
/// reader/writer with `tokio::io::join`. Mobile sandboxes forbid spawning the
/// `ssh` binary the desktop client uses, so this has to be pure Rust. Keys come
/// from the Android Keystore / iOS Keychain via the foreign layer.
pub enum Transport {
    Tcp,
    Tls { accept_invalid_certs: bool },
}

impl Transport {
    pub fn for_profile(profile: &ServerProfile) -> Self {
        if profile.use_tls {
            Transport::Tls { accept_invalid_certs: profile.accept_invalid_certs }
        } else {
            Transport::Tcp
        }
    }

    pub async fn connect(&self, host: &str, port: u16) -> Result<Box<dyn Io>> {
        let addr = format!("{host}:{port}");
        let stream = TcpStream::connect(&addr)
            .await
            .map_err(|e| TermlandError::connect(format!("{addr}: {e}")))?;
        // Interactive remote desktop: coalescing small input packets adds
        // perceptible lag, so Nagle has to go.
        if let Err(e) = stream.set_nodelay(true) {
            tracing::warn!("set_nodelay failed: {e}");
        }

        match self {
            Transport::Tcp => {
                tracing::info!("Connected to {addr}");
                Ok(Box::new(stream))
            }
            Transport::Tls { accept_invalid_certs } => {
                tracing::info!("Connected to {addr} (TLS)");
                let config = tls_config(*accept_invalid_certs)?;
                let connector = tokio_rustls::TlsConnector::from(Arc::new(config));
                let domain = rustls::pki_types::ServerName::try_from(host.to_string())
                    .map_err(|e| TermlandError::tls(format!("invalid server name '{host}': {e}")))?;
                let tls = connector
                    .connect(domain, stream)
                    .await
                    .map_err(|e| TermlandError::tls(format!("handshake failed: {e}")))?;
                tracing::info!("TLS handshake complete");
                Ok(Box::new(tls))
            }
        }
    }
}

/// The crypto provider is pinned explicitly instead of relying on the
/// process-default: this crate links into a workspace where another crate
/// enables aws-lc-rs, and feature unification would otherwise decide for us.
fn provider() -> Arc<rustls::crypto::CryptoProvider> {
    Arc::new(rustls::crypto::ring::default_provider())
}

fn tls_config(accept_invalid_certs: bool) -> Result<rustls::ClientConfig> {
    let builder = rustls::ClientConfig::builder_with_provider(provider())
        .with_safe_default_protocol_versions()
        .map_err(|e| TermlandError::tls(format!("unsupported TLS versions: {e}")))?;

    if accept_invalid_certs {
        tracing::warn!("TLS certificate validation disabled for this connection");
        return Ok(builder
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(AcceptAnyCert(provider())))
            .with_no_client_auth());
    }

    let mut roots = rustls::RootCertStore::empty();
    // Android/iOS may hand back nothing here; that is not fatal by itself, the
    // handshake just fails later with a clearer error than a panic.
    let native = rustls_native_certs::load_native_certs();
    for e in &native.errors {
        tracing::warn!("loading native root certificates: {e}");
    }
    for cert in native.certs {
        let _ = roots.add(cert);
    }
    if roots.is_empty() {
        tracing::warn!("no root certificates available; TLS verification will fail");
    }
    Ok(builder.with_root_certificates(roots).with_no_client_auth())
}

/// Verifier for `accept_invalid_certs`. Mirrors the desktop client's
/// `--accept-invalid-certs`: needed because Termland servers ship a self-signed
/// cert by default and there is no CA to pin against on a LAN.
#[derive(Debug)]
struct AcceptAnyCert(Arc<rustls::crypto::CryptoProvider>);

impl rustls::client::danger::ServerCertVerifier for AcceptAnyCert {
    fn verify_server_cert(
        &self,
        _: &rustls::pki_types::CertificateDer<'_>,
        _: &[rustls::pki_types::CertificateDer<'_>],
        _: &rustls::pki_types::ServerName<'_>,
        _: &[u8],
        _: rustls::pki_types::UnixTime,
    ) -> std::result::Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }
    fn verify_tls12_signature(
        &self,
        _: &[u8],
        _: &rustls::pki_types::CertificateDer<'_>,
        _: &rustls::DigitallySignedStruct,
    ) -> std::result::Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }
    fn verify_tls13_signature(
        &self,
        _: &[u8],
        _: &rustls::pki_types::CertificateDer<'_>,
        _: &rustls::DigitallySignedStruct,
    ) -> std::result::Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }
    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.0.signature_verification_algorithms.supported_schemes()
    }
}
