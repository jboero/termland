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
    /// The QUIC transport (`docs/quic-transport.md`), dialed with `quinn`
    /// instead of a plain/TLS `TcpStream`. Control messages
    /// (Hello/auth/`Session*`/input/clipboard) run over one bidirectional
    /// stream exactly as they do over TCP/TLS/SSH; since Q2, video and audio
    /// run on their own QUIC objects instead of sharing that stream — see
    /// `session::run_session`'s QUIC reader tasks and
    /// `crates/termland-server/src/quic.rs`'s module doc comment for the
    /// full plane layout. Exists for the same reason the design doc pulled
    /// QUIC forward to accompany the mobile milestone rather than treating
    /// it as a desktop-only stretch goal: connection migration and fast
    /// reconnect matter most on a phone roaming between Wi-Fi and cellular,
    /// and Q2's split planes mean a lossy cellular link no longer stalls
    /// input/control behind a slow video packet. `accept_invalid_certs`
    /// mirrors `Transport::Tls`'s field of the same name/meaning — QUIC
    /// always requires TLS 1.3, so the same self-signed-cert-on-a-LAN
    /// tradeoff applies.
    Quic { accept_invalid_certs: bool },
}

/// ALPN identifier for the termland QUIC transport. Must match the server's
/// (`crates/termland-server/src/quic.rs`) exactly — QUIC's handshake uses
/// ALPN to select a protocol and refuses to proceed on a mismatch.
const QUIC_ALPN: &[u8] = b"termland/1";

impl Transport {
    pub fn for_profile(profile: &ServerProfile) -> Self {
        if profile.use_ssh {
            Transport::Ssh {
                username: profile.username.clone().unwrap_or_default(),
                password: profile.password.clone().unwrap_or_default(),
            }
        } else if profile.use_quic {
            // Takes priority over use_tls: QUIC replaces the TCP+TLS socket
            // outright rather than layering on it, so a leftover TLS flag
            // from switching modes in the UI must not fight it.
            Transport::Quic { accept_invalid_certs: profile.accept_invalid_certs }
        } else if profile.use_tls {
            Transport::Tls { accept_invalid_certs: profile.accept_invalid_certs }
        } else {
            Transport::Tcp
        }
    }

    pub async fn connect(&self, host: &str, port: u16) -> Result<Connected> {
        // QUIC is UDP-based, not a TCP socket wrapped in something else like
        // Tls/Ssh below are — it needs its own connection path entirely, so
        // it branches off before the shared `TcpStream::connect` the other
        // three variants all build on.
        if let Transport::Quic { accept_invalid_certs } = self {
            return connect_quic(host, port, *accept_invalid_certs).await;
        }

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
                Ok(Connected { io: Box::new(stream), quic_connection: None })
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
                Ok(Connected { io: Box::new(tls), quic_connection: None })
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
                Ok(Connected { io: Box::new(SshIo { stream: channel.into_stream(), _handle: handle }), quic_connection: None })
            }
            Transport::Quic { .. } => {
                unreachable!("Transport::Quic returns early above, before the TCP connect")
            }
        }
    }
}

/// What `Transport::connect` hands back: the control-plane byte stream every
/// transport provides, plus (QUIC only) the extra `Connection` handle Q2
/// needs for its split video/audio planes.
pub(crate) struct Connected {
    pub(crate) io: Box<dyn Io>,
    /// `Some` only for `Transport::Quic` — every other transport carries
    /// video/audio inline on `io` via the ordinary `Message` framing (Q2
    /// only exists for QUIC), so this stays `None` for Tcp/Tls/Ssh.
    ///
    /// Deliberately just the raw `Connection`, not an already-`accept_uni()`'d
    /// video stream: accepting it here, before this function even returns to
    /// let `connect_and_handshake` send `Hello`, would block waiting for a
    /// uni stream the server doesn't open until well after
    /// Hello/auth/SessionCreate have already round-tripped over the control
    /// bidi stream (see `crates/termland-server/src/quic.rs`'s `run_session`
    /// comment for exactly when). `session::run_session` accepts it lazily,
    /// in a spawned background task, once streaming actually starts.
    pub(crate) quic_connection: Option<quinn::Connection>,
}

/// Dials a QUIC connection with `quinn`, completes the TLS 1.3 handshake,
/// and opens the control bidirectional stream (unchanged from Q1's
/// contract — mirrors `crates/termland-server/src/quic.rs`'s `accept_bi()`
/// on the server side). Does NOT accept the Q2 video uni stream or start
/// reading datagrams here — see `Connected::quic_connection`'s doc comment
/// for why that has to wait.
async fn connect_quic(host: &str, port: u16, accept_invalid_certs: bool) -> Result<Connected> {
    let addr_str = format!("{host}:{port}");
    let addr = tokio::net::lookup_host(&addr_str)
        .await
        .map_err(|e| TermlandError::connect(format!("resolving {addr_str}: {e}")))?
        .next()
        .ok_or_else(|| TermlandError::connect(format!("no addresses found for {addr_str}")))?;

    // Bind an ephemeral local UDP socket; the server address/port is chosen
    // per-connection via `endpoint.connect`, not at bind time.
    let local_bind = if addr.is_ipv6() { "[::]:0" } else { "0.0.0.0:0" };
    let mut endpoint = quinn::Endpoint::client(local_bind.parse().unwrap())
        .map_err(|e| TermlandError::connect(format!("failed to open local QUIC/UDP socket: {e}")))?;
    endpoint.set_default_client_config(quic_client_config(accept_invalid_certs)?);

    let connecting = endpoint
        .connect(addr, host)
        .map_err(|e| TermlandError::connect(format!("{addr_str}: {e}")))?;
    let connection = connecting
        .await
        .map_err(|e| TermlandError::connect(format!("QUIC handshake with {addr_str} failed: {e}")))?;
    tracing::info!("Connected to {addr_str} (QUIC), handshake complete");

    // The control bidi stream: opened immediately — the server is waiting
    // on `accept_bi()` for it. Unchanged from Q1's contract.
    let (send, recv) = connection
        .open_bi()
        .await
        .map_err(|e| TermlandError::connect(format!("failed to open QUIC stream: {e}")))?;

    Ok(Connected {
        io: Box::new(QuicIo {
            io: tokio::io::join(recv, send),
            _connection: connection.clone(),
            _endpoint: endpoint,
        }),
        quic_connection: Some(connection),
    })
}

/// Builds quinn's `ClientConfig` by reusing `tls_config` (the same
/// accept-invalid-certs-or-native-roots logic `Transport::Tls` already uses)
/// and layering QUIC's mandatory ALPN + the `QuicClientConfig` wrapper on top.
fn quic_client_config(accept_invalid_certs: bool) -> Result<quinn::ClientConfig> {
    let mut crypto = tls_config(accept_invalid_certs)?;
    crypto.alpn_protocols = vec![QUIC_ALPN.to_vec()];

    let quic_crypto = quinn::crypto::rustls::QuicClientConfig::try_from(crypto)
        .map_err(|e| TermlandError::tls(format!("rustls config not usable for QUIC: {e}")))?;

    Ok(quinn::ClientConfig::new(Arc::new(quic_crypto)))
}

/// Adapts a QUIC bidi stream's separate `RecvStream`/`SendStream` halves into
/// the plain `AsyncRead + AsyncWrite` the session loop expects (via
/// `tokio::io::join`, the same combinator `run_subsystem` uses for
/// stdin/stdout on the server side), while keeping the `Connection` and
/// `Endpoint` alive for as long as the stream is held — same rationale as
/// `SshIo` bundling in its SSH `Handle` below: dropping either would tear
/// down the connection out from under the stream.
struct QuicIo {
    io: tokio::io::Join<quinn::RecvStream, quinn::SendStream>,
    _connection: quinn::Connection,
    _endpoint: quinn::Endpoint,
}

impl AsyncRead for QuicIo {
    fn poll_read(self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &mut ReadBuf<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().io).poll_read(cx, buf)
    }
}

impl AsyncWrite for QuicIo {
    fn poll_write(self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &[u8]) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.get_mut().io).poll_write(cx, buf)
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().io).poll_flush(cx)
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().io).poll_shutdown(cx)
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
