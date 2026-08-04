use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use tokio::io::{self, AsyncRead, AsyncWrite, ReadBuf};
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
pub enum Transport {
    Tcp,
    Tls { accept_invalid_certs: bool },
    /// Opens an SSH connection with `russh` and requests the `termland`
    /// subsystem channel — the in-process equivalent of the desktop client's
    /// `ssh -s host termland`. Exists because mobile app sandboxes forbid
    /// spawning the `ssh` binary. Password-only for now; see
    /// `SshHostKeyPolicy` for the host-key posture and the module-level
    /// followup note on key-based auth.
    Ssh { username: String, password: String },
}

impl Transport {
    pub fn for_profile(profile: &ServerProfile) -> Self {
        if profile.use_ssh {
            Transport::Ssh {
                username: profile.username.clone().unwrap_or_default(),
                password: profile.password.clone().unwrap_or_default(),
            }
        } else if profile.use_tls {
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
            Transport::Ssh { username, password } => {
                tracing::info!("Connected to {addr}, starting SSH handshake");
                let config = Arc::new(russh::client::Config::default());
                let mut handle = russh::client::connect_stream(config, stream, AcceptAnyHostKey)
                    .await
                    .map_err(|e| TermlandError::connect(format!("SSH handshake with {addr} failed: {e}")))?;

                let auth = handle
                    .authenticate_password(username.clone(), password.clone())
                    .await
                    .map_err(|e| TermlandError::auth(format!("SSH authentication failed: {e}")))?;
                if !auth.success() {
                    return Err(TermlandError::auth("SSH server rejected the password"));
                }
                tracing::info!("SSH authenticated as '{username}'");

                // Mirrors the desktop client's `ssh -s host termland`: sshd
                // execs the termland binary with stdio wired to this channel,
                // so no extra TCP port has to be reachable on the server.
                let mut channel = handle
                    .channel_open_session()
                    .await
                    .map_err(|e| TermlandError::connect(format!("failed to open SSH channel: {e}")))?;
                channel
                    .request_subsystem(true, "termland")
                    .await
                    .map_err(|e| {
                        TermlandError::connect(format!("failed to request 'termland' subsystem: {e}"))
                    })?;

                // `request_subsystem(want_reply: true, ..)` gets acked with a
                // Success/Failure channel message before any Data arrives;
                // wait for it explicitly rather than handing the channel to
                // `into_stream()` immediately; a rejected subsystem must
                // surface as a connect error, not a stream that silently
                // never produces bytes.
                loop {
                    match channel.wait().await {
                        Some(russh::ChannelMsg::Success) => break,
                        Some(russh::ChannelMsg::Failure) => {
                            return Err(TermlandError::connect(
                                "server rejected the 'termland' subsystem request",
                            ));
                        }
                        Some(_) => continue,
                        None => {
                            return Err(TermlandError::connect(
                                "SSH channel closed before the 'termland' subsystem request completed",
                            ));
                        }
                    }
                }

                tracing::info!("SSH subsystem 'termland' ready");
                // The channel's stream alone isn't enough: it only stays
                // readable/writable as long as the SSH session's background
                // task keeps running, which is tied to `handle`'s lifetime
                // (dropping it closes the connection). Bundle both together.
                Ok(Box::new(SshIo { stream: channel.into_stream(), _handle: handle }))
            }
        }
    }
}

/// Host key acceptance policy for the embedded SSH transport.
///
/// Accepts every host key unconditionally and just logs the fingerprint —
/// the SSH-transport analogue of `Transport::Tls`'s `accept_invalid_certs`:
/// there is no CA to check against and this crate has no on-device
/// `known_hosts` file to pin to (yet).
///
/// TODO(followup, security): this is TOFU without the "trust" part. A real
/// implementation should persist the host key fingerprint on first connect
/// (Keystore/Keychain-backed, alongside the rest of the profile) and refuse a
/// silently-changed key on reconnect, matching what the desktop client's
/// system `ssh` binary already does via `~/.ssh/known_hosts`. Do not read
/// this as an endorsement of skipping host-key verification — it is a stopgap
/// so the transport works at all, not a security stance.
struct AcceptAnyHostKey;

impl russh::client::Handler for AcceptAnyHostKey {
    type Error = russh::Error;

    async fn check_server_key(&mut self, key: &russh::keys::PublicKey) -> std::result::Result<bool, Self::Error> {
        tracing::warn!(
            "SSH host key not verified (fingerprint {}): TOFU/pinning not implemented yet",
            key.fingerprint(russh::keys::HashAlg::Sha256)
        );
        Ok(true)
    }
}

/// Adapts a `russh` subsystem channel into the plain `AsyncRead + AsyncWrite`
/// the session loop expects, while keeping the SSH connection itself
/// (`Handle`) alive for as long as the stream is held. `russh::ChannelStream`
/// alone is sufficient for the byte-shuffling; `_handle` is never read, only
/// kept from being dropped.
struct SshIo {
    stream: russh::ChannelStream<russh::client::Msg>,
    _handle: russh::client::Handle<AcceptAnyHostKey>,
}

impl AsyncRead for SshIo {
    fn poll_read(self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &mut ReadBuf<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().stream).poll_read(cx, buf)
    }
}

impl AsyncWrite for SshIo {
    fn poll_write(self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &[u8]) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.get_mut().stream).poll_write(cx, buf)
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().stream).poll_flush(cx)
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().stream).poll_shutdown(cx)
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
