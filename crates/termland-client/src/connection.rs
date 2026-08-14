use anyhow::{Context, Result};
use futures::{SinkExt, StreamExt};
use std::sync::Arc;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::TcpStream;
use tokio::process::Command;
use tokio::sync::mpsc;
use tokio_util::codec::Framed;
use termland_protocol::*;

/// Ready-to-blit pixel buffer from decode thread to display thread.
pub enum ServerEvent {
    SessionReady(SessionReady),
    Frame { width: u32, height: u32, pixels: Vec<u32> },
    /// Periodic data rate update in bytes/sec (measured over the last ~1s).
    DataRate { bytes_per_sec: u64 },
    /// The server's compositor cursor bitmap changed. Only sent while we're
    /// in client-side-cursor mode (see `SetCursorInFrame`); tells the display
    /// layer what shape to actually draw instead of the generic placeholder.
    /// Carries no position - see the note on `RemoteCursor` in display.rs for
    /// why position stays purely locally-tracked.
    CursorUpdate(CursorUpdate),
    #[allow(dead_code)]
    Pong(Pong),
    /// The server explicitly ended the session: it sent us a `SessionEnd`
    /// message (its compositor exited, or someone closed the session via
    /// `--close`/another client). This is final - the session is genuinely
    /// gone, so the display layer should show `reason` and exit rather than
    /// retry. Contrast with `Reconnecting`: a `SessionEnd` we *receive* is
    /// always server-initiated, since our own clean-exit path
    /// (`ClientCommand::Disconnect`) sends one but never turns it back into
    /// this event for ourselves.
    SessionEnded { reason: String },
    /// The connection dropped unexpectedly - a protocol decode error, or a
    /// bare EOF with no `SessionEnd` from the server first. No one told us
    /// the session is over, so per this project's persistent-session design
    /// (see termland-server's `SessionOutcome::Detached`) the server-side
    /// compositor is very likely still alive. `connection.rs` is retrying
    /// with backoff in the background and will reattach to the same
    /// session; `attempt` is the 1-based attempt about to be made. The
    /// display layer should show a "Reconnecting..." status and keep
    /// showing the last frame rather than exiting.
    Reconnecting { attempt: u32 },
}

/// Opus packet for the audio playback thread.
struct AudioJob {
    data: Vec<u8>,
}

#[allow(dead_code)]
pub enum ClientCommand {
    KeyEvent(termland_protocol::input::KeyEvent),
    MouseMove(termland_protocol::input::MouseMove),
    MouseButton(termland_protocol::input::MouseButton),
    MouseScroll(termland_protocol::input::MouseScroll),
    Resize(u32, u32),
    /// Toggle whether the server includes its cursor in the video stream.
    /// `true` = server-side cursor (in frame), `false` = client draws own cursor.
    SetCursorInFrame(bool),
    /// Send clipboard content to the remote session.
    ClipboardSend { mime_type: String, data: Vec<u8> },
    /// Send files copied on the local clipboard (a `text/uri-list`) to the
    /// remote session, so a paste there gets the real files. See
    /// `termland_protocol::FileTransferPayload`'s doc comment for the MVP
    /// size-cap/no-chunking limitation.
    FileTransferSend(FileTransferPayload),
    Disconnect,
}

/// RGBA bytes to softbuffer pixels.
fn rgba_to_pixels(rgba: &[u8]) -> Vec<u32> {
    rgba.chunks_exact(4)
        .map(|c| (c[0] as u32) << 16 | (c[1] as u32) << 8 | c[2] as u32)
        .collect()
}

/// Parameters for initiating a new termland session.
#[derive(Clone)]
pub struct ConnectParams {
    pub mode: SessionMode,
    pub width: u32,
    pub height: u32,
    pub quality: u8,
    pub audio: bool,
    pub ssh_opts: Vec<String>,
    pub tls: bool,
    pub accept_invalid_certs: bool,
    pub username: Option<String>,
    pub password: Option<String>,
    pub desktop_shell: Option<String>,
    pub encoder_preset: Option<String>,
    pub encoder_crf: Option<u8>,
    pub encoder_extra_params: Option<String>,
    /// Force a specific codec. When set, the client advertises only this codec
    /// so the server must encode with it; otherwise all codecs are offered.
    pub codec: Option<termland_protocol::VideoCodec>,
    /// Resume an existing session by id instead of creating a new one.
    pub attach: Option<String>,
    /// Auto-retry with backoff and reattach to the same session on an
    /// unexpected connection drop (see `ServerEvent::Reconnecting`). On by
    /// default; `--no-reconnect` turns this off for scripted/tested use
    /// where retrying forever against a dead server isn't wanted.
    pub reconnect: bool,
}

/// Any bidirectional byte stream we can run the protocol over (SSH pipe, TCP,
/// or TLS). Boxed so the transport can be chosen at runtime and reused by both
/// the streaming path and one-shot control ops.
pub trait Io: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Io for T {}

/// Open the transport to the server: SSH subsystem, plain TCP, or TLS.
async fn open_io(server: &str, ssh: bool, params: &ConnectParams) -> Result<Box<dyn Io>> {
    if ssh {
        let mut ssh_args: Vec<String> = params.ssh_opts.clone();
        ssh_args.extend(["-s".into(), server.to_string(), "termland".into()]);
        let child = Command::new("ssh")
            .args(&ssh_args)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::inherit())
            .spawn().context("failed to spawn ssh")?;
        Ok(Box::new(tokio::io::join(child.stdout.unwrap(), child.stdin.unwrap())))
    } else if params.tls {
        let stream = TcpStream::connect(server).await
            .context(format!("failed to connect to {server}"))?;
        tracing::info!("Connected to {server} (TLS)");

        let mut root_store = rustls::RootCertStore::empty();
        for cert in rustls_native_certs::load_native_certs().expect("load native certs") {
            let _ = root_store.add(cert);
        }

        let config = if params.accept_invalid_certs {
            rustls::ClientConfig::builder()
                .dangerous()
                .with_custom_certificate_verifier(Arc::new(AcceptAnyCert))
                .with_no_client_auth()
        } else {
            rustls::ClientConfig::builder()
                .with_root_certificates(root_store)
                .with_no_client_auth()
        };

        let connector = tokio_rustls::TlsConnector::from(Arc::new(config));
        let host = server.split(':').next().unwrap_or("localhost");
        let domain = rustls::pki_types::ServerName::try_from(host.to_string())
            .context("invalid server name for TLS")?;
        let tls_stream = connector.connect(domain, stream).await
            .context("TLS handshake failed")?;
        tracing::info!("TLS handshake complete");
        Ok(Box::new(tls_stream))
    } else {
        let stream = TcpStream::connect(server).await
            .context(format!("failed to connect to {server}"))?;
        tracing::info!("Connected to {server}");
        Ok(Box::new(stream))
    }
}

/// One-shot session-management operations (no GUI window).
pub enum ControlOp {
    List,
    Close(String),
}

/// Fetch the list of resumable sessions (used by the tray manager).
pub async fn fetch_sessions(
    server: &str, ssh: bool, params: &ConnectParams,
) -> Result<Vec<termland_protocol::SessionInfo>> {
    let io = open_io(server, ssh, params).await?;
    let mut framed = Framed::new(io, TermlandCodec);
    handshake(&mut framed, params).await?;
    framed.send(Message::SessionList(SessionList {})).await.context("send SessionList")?;
    let resp = framed.next().await.context("closed")?.context("decode")?;
    match resp {
        Message::SessionListResult(r) => Ok(r.sessions),
        other => anyhow::bail!("expected SessionListResult, got {:?}", other.message_id()),
    }
}

/// Run a one-shot control op against the server and print the result.
pub async fn run_control(server: &str, ssh: bool, params: ConnectParams, op: ControlOp) -> Result<()> {
    let io = open_io(server, ssh, &params).await?;
    let mut framed = Framed::new(io, TermlandCodec);
    handshake(&mut framed, &params).await?;

    match op {
        ControlOp::List => {
            framed.send(Message::SessionList(SessionList {})).await.context("send SessionList")?;
            let resp = framed.next().await.context("closed")?.context("decode")?;
            match resp {
                Message::SessionListResult(r) => {
                    if r.sessions.is_empty() {
                        println!("No resumable sessions.");
                    } else {
                        println!("{:<16} {:<16} {:<11} {:>8}", "SESSION ID", "MODE", "SIZE", "AGE");
                        for s in &r.sessions {
                            println!(
                                "{:<16} {:<16} {:<11} {:>8}",
                                s.session_id, s.mode,
                                format!("{}x{}", s.width, s.height),
                                format_age(s.age_secs),
                            );
                        }
                        println!("\nResume with:  termland-client --attach <SESSION ID> …");
                    }
                }
                other => anyhow::bail!("expected SessionListResult, got {:?}", other.message_id()),
            }
        }
        ControlOp::Close(id) => {
            framed.send(Message::SessionClose(SessionClose { session_id: id.clone() }))
                .await.context("send SessionClose")?;
            // Server replies with a SessionEnd ack; ignore its exact contents.
            let _ = framed.next().await;
            println!("Closed session {id}");
        }
    }
    Ok(())
}

fn format_age(secs: u64) -> String {
    if secs < 60 { format!("{secs}s") }
    else if secs < 3600 { format!("{}m", secs / 60) }
    else if secs < 86400 { format!("{}h", secs / 3600) }
    else { format!("{}d", secs / 86400) }
}

pub async fn connect(
    server: &str, ssh: bool, params: ConnectParams,
) -> Result<(mpsc::UnboundedReceiver<ServerEvent>, mpsc::UnboundedSender<ClientCommand>)> {
    let (server_tx, server_rx) = mpsc::unbounded_channel();
    let (client_tx, client_rx) = mpsc::unbounded_channel();

    let io = open_io(server, ssh, &params).await?;
    let server = server.to_string();
    tokio::spawn(async move {
        run_with_reconnect(server, ssh, params, io, server_tx, client_rx).await;
    });
    Ok((server_rx, client_tx))
}

/// Capped exponential backoff for reconnect attempts: 1s, 2s, 4s, 8s, then
/// held at 30s for every attempt after. `attempt` is the 1-based attempt
/// about to be made (so `backoff_delay(1) == 1s`, the delay *before* the
/// first retry, matching a human's expectation of "try again shortly").
fn backoff_delay(attempt: u32) -> std::time::Duration {
    let shift = attempt.saturating_sub(1).min(5); // 2^5 = 32, already past the 30s cap
    let secs = 1u64 << shift;
    std::time::Duration::from_secs(secs.min(30))
}

/// Owns the connect -> stream -> (on unexpected drop) retry-with-backoff
/// loop for the lifetime of the client session. Runs entirely in the
/// background task spawned by `connect()`, so neither `connect()`'s
/// signature nor how `display.rs` calls it needs to change - the display
/// layer only ever sees the `ServerEvent`s this loop emits.
///
/// The crux of this whole function is the distinction `session_loop`
/// reports back via `LoopExit`: a `Message::SessionEnd` we *receive* means
/// the server itself ended the session (its compositor exited, or someone
/// closed it via `--close`) - that's final, never retry. Anything else that
/// ends the stream unexpectedly (a decode error, or a bare EOF with no
/// `SessionEnd` first) is just the network connection dying; the
/// server-side compositor keeps running across an ordinary disconnect (see
/// termland-server's `SessionOutcome::Detached`), so reattaching to the
/// same session id is very likely to succeed.
async fn run_with_reconnect(
    server: String, ssh: bool, mut params: ConnectParams, mut io: Box<dyn Io>,
    server_tx: mpsc::UnboundedSender<ServerEvent>,
    mut client_rx: mpsc::UnboundedReceiver<ClientCommand>,
) {
    // Session id from the last SessionReady we saw, so a reconnect resumes
    // THIS session instead of accidentally creating a new one.
    let mut session_id: Option<String> = None;

    loop {
        let outcome = session_loop(io, params.clone(), server_tx.clone(), &mut client_rx, &mut session_id).await;

        match outcome {
            Ok(LoopExit::ClientQuit) => return,
            Ok(LoopExit::SessionEnded { reason }) => {
                tracing::info!("Session ended by server: {reason}");
                let _ = server_tx.send(ServerEvent::SessionEnded { reason });
                return;
            }
            Ok(LoopExit::ConnectionLost) => {
                tracing::warn!("Connection lost unexpectedly");
            }
            Err(e) => {
                tracing::warn!("Connection error: {e:#}");
            }
        }

        if !params.reconnect {
            let _ = server_tx.send(ServerEvent::SessionEnded {
                reason: "connection lost (reconnect disabled)".into(),
            });
            return;
        }

        if let Some(id) = &session_id {
            params.attach = Some(id.clone());
        }

        // Drop input queued while we were connected: it's stale by the time
        // we reconnect (keystrokes/mouse moves aimed at a session that
        // wasn't there to receive them), and replaying it on reattach could
        // e.g. leave a key looking stuck down.
        while client_rx.try_recv().is_ok() {}

        let mut attempt: u32 = 0;
        io = loop {
            attempt += 1;
            let _ = server_tx.send(ServerEvent::Reconnecting { attempt });
            let delay = backoff_delay(attempt);
            tracing::info!("Reconnecting to {server} in {delay:?} (attempt {attempt})...");
            tokio::time::sleep(delay).await;

            match open_io(&server, ssh, &params).await {
                Ok(io) => {
                    tracing::info!("Reconnected on attempt {attempt}");
                    break io;
                }
                Err(e) => tracing::warn!("Reconnect attempt {attempt} failed: {e:#}"),
            }
        };
    }
}

/// Dummy certificate verifier for --accept-invalid-certs (self-signed servers).
#[derive(Debug)]
struct AcceptAnyCert;

impl rustls::client::danger::ServerCertVerifier for AcceptAnyCert {
    fn verify_server_cert(
        &self, _: &rustls::pki_types::CertificateDer<'_>, _: &[rustls::pki_types::CertificateDer<'_>],
        _: &rustls::pki_types::ServerName<'_>, _: &[u8], _: rustls::pki_types::UnixTime,
    ) -> std::result::Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }
    fn verify_tls12_signature(
        &self, _: &[u8], _: &rustls::pki_types::CertificateDer<'_>,
        _: &rustls::DigitallySignedStruct,
    ) -> std::result::Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }
    fn verify_tls13_signature(
        &self, _: &[u8], _: &rustls::pki_types::CertificateDer<'_>,
        _: &rustls::DigitallySignedStruct,
    ) -> std::result::Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }
    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        rustls::crypto::aws_lc_rs::default_provider()
            .signature_verification_algorithms.supported_schemes()
    }
}

/// Encoded video packet for the decode thread.
struct DecodeJob {
    data: Vec<u8>,
    /// Codec this packet is encoded with, if the server tagged it. Lets the
    /// decode thread switch decoders if the server ever changes codec mid-stream.
    codec: Option<VideoCodec>,
}

/// Hello/HelloAck + optional PAM auth. Shared by the streaming path and the
/// one-shot control ops.
async fn handshake<T: AsyncRead + AsyncWrite + Unpin>(
    framed: &mut Framed<T, TermlandCodec>,
    params: &ConnectParams,
) -> Result<()> {
    framed.send(Message::Hello(Hello { protocol_version: PROTOCOL_VERSION, client_name: "termland-client".into() })).await?;
    let msg = framed.next().await.context("closed")?.context("decode")?;
    let auth_required = match &msg {
        Message::HelloAck(ha) => {
            tracing::info!("Server: {} (v{}, session {})", ha.server_name, ha.protocol_version, ha.session_id);
            ha.auth_required
        }
        other => anyhow::bail!("expected HelloAck, got {:?}", other.message_id()),
    };

    if auth_required {
        let msg = framed.next().await.context("closed")?.context("decode")?;
        match msg {
            Message::AuthRequest(ar) => {
                tracing::info!("Server requires authentication (methods: {:?})", ar.methods);
            }
            other => anyhow::bail!("expected AuthRequest, got {:?}", other.message_id()),
        }

        let username = params.username.clone().unwrap_or_else(whoami::username);
        let password = params.password.clone()
            .or_else(|| std::env::var("TERMLAND_PASSWORD").ok())
            .unwrap_or_default();
        if password.is_empty() {
            tracing::warn!("Server requires auth but no --password provided");
        }

        framed.send(Message::AuthResponse(AuthResponse {
            username: username.clone(),
            credential: password,
        })).await.context("send AuthResponse")?;

        let result = framed.next().await.context("closed")?.context("decode")?;
        match result {
            Message::AuthResult(ar) if ar.success => tracing::info!("Authenticated as '{username}'"),
            Message::AuthResult(ar) => anyhow::bail!("Authentication failed: {}", ar.message),
            other => anyhow::bail!("expected AuthResult, got {:?}", other.message_id()),
        }
    }
    Ok(())
}

/// Why `session_loop` returned - lets the caller (`run_with_reconnect`)
/// decide whether to retry. See its doc comment for the reasoning behind
/// the distinction.
enum LoopExit {
    /// `ClientCommand::Disconnect` (the user quit / closed the window). Our
    /// own clean exit - never retry.
    ClientQuit,
    /// The server sent us `Message::SessionEnd`: it decided the session is
    /// over. Never retry - carries the server's reason for logging/UI.
    SessionEnded { reason: String },
    /// The stream ended with no `SessionEnd` first - a decode error or a
    /// bare EOF. The connection just dropped; retry.
    ConnectionLost,
}

async fn session_loop<T: AsyncRead + AsyncWrite + Unpin>(
    io: T, params: ConnectParams,
    server_tx: mpsc::UnboundedSender<ServerEvent>,
    client_rx: &mut mpsc::UnboundedReceiver<ClientCommand>,
    session_id: &mut Option<String>,
) -> Result<LoopExit> {
    let mut framed = Framed::new(io, TermlandCodec);
    handshake(&mut framed, &params).await?;

    // If a codec was forced, advertise only it so the server must use it;
    // otherwise advertise all (our ffmpeg decoder handles any) in preference
    // order so the server picks the best its hardware can encode.
    let supported_codecs = match params.codec {
        Some(c) => vec![c],
        None => termland_protocol::VideoCodec::all_preferred(),
    };
    // Everything this build can decode. Not user-selectable: unlike video,
    // there is no reason to pin audio to one codec from the command line.
    let supported_audio_codecs = termland_protocol::AudioCodec::all_preferred();
    match &params.attach {
        Some(id) => {
            tracing::info!("Attaching to session {id}");
            framed.send(Message::SessionAttach(SessionAttach {
                session_id: id.clone(),
                audio: params.audio,
                quality: params.quality,
                encoder_preset: params.encoder_preset,
                encoder_crf: params.encoder_crf,
                encoder_extra_params: params.encoder_extra_params,
                supported_codecs,
                supported_audio_codecs,
            })).await?;
        }
        None => {
            framed.send(Message::SessionCreate(SessionCreate {
                mode: params.mode,
                width: params.width,
                height: params.height,
                audio: params.audio,
                quality: params.quality,
                desktop_shell: params.desktop_shell,
                encoder_preset: params.encoder_preset,
                encoder_crf: params.encoder_crf,
                encoder_extra_params: params.encoder_extra_params,
                supported_codecs,
                supported_audio_codecs,
            })).await?;
        }
    }
    let msg = framed.next().await.context("closed")?.context("decode")?;
    let negotiated_codec = match &msg {
        Message::SessionReady(sr) => {
            match sr.codec {
                Some(c) => tracing::info!("Session ready: {}x{} codec={c}", sr.width, sr.height),
                None => tracing::info!("Session ready: {}x{} (codec unannounced, will auto-detect)", sr.width, sr.height),
            }
            if !sr.session_id.is_empty() {
                *session_id = Some(sr.session_id.clone());
            }
            let _ = server_tx.send(ServerEvent::SessionReady(sr.clone()));
            sr.codec
        }
        other => anyhow::bail!("expected SessionReady, got {:?}", other.message_id()),
    };

    // Spawn a dedicated decode thread so it doesn't block the network task.
    // It builds the decoder for the negotiated codec (or auto-detects if the
    // server didn't announce one).
    let (decode_tx, decode_rx) = std::sync::mpsc::channel::<DecodeJob>();
    let display_tx = server_tx.clone();
    let _decode_thread = std::thread::spawn(move || {
        decode_thread(decode_rx, display_tx, negotiated_codec);
    });

    // Spawn audio playback thread if audio was requested
    let audio_tx = if params.audio {
        let (atx, arx) = std::sync::mpsc::channel::<AudioJob>();
        std::thread::spawn(move || {
            audio_playback_thread(arx);
        });
        Some(atx)
    } else {
        None
    };

    // Data rate tracking: byte counter + last-reported instant.
    let mut bytes_since_report: u64 = 0;
    let mut last_report = std::time::Instant::now();
    let report_interval = std::time::Duration::from_millis(1000);

    loop {
        // Drain input commands first - must not be starved by video
        loop {
            match client_rx.try_recv() {
                Ok(ClientCommand::Disconnect) => {
                    let _ = framed.send(Message::SessionEnd(SessionEnd { reason: "client disconnect".into() })).await;
                    return Ok(LoopExit::ClientQuit);
                }
                Ok(ClientCommand::Resize(w, h)) => { let _ = framed.send(Message::SessionResize(SessionResize { width: w, height: h })).await; }
                Ok(ClientCommand::KeyEvent(ke)) => { let _ = framed.send(Message::KeyEvent(ke)).await; }
                Ok(ClientCommand::MouseMove(mm)) => { let _ = framed.send(Message::MouseMove(mm)).await; }
                Ok(ClientCommand::MouseButton(mb)) => { let _ = framed.send(Message::MouseButton(mb)).await; }
                Ok(ClientCommand::MouseScroll(ms)) => { let _ = framed.send(Message::MouseScroll(ms)).await; }
                Ok(ClientCommand::SetCursorInFrame(yes)) => {
                    let _ = framed.send(Message::CursorMode(CursorModeMsg {
                        include_cursor_in_frame: yes,
                    })).await;
                }
                Ok(ClientCommand::ClipboardSend { mime_type, data }) => {
                    let _ = framed.send(Message::ClipboardSend(ClipboardPayload {
                        mime_type,
                        data,
                    })).await;
                }
                Ok(ClientCommand::FileTransferSend(payload)) => {
                    let _ = framed.send(Message::FileTransferSend(payload)).await;
                }
                Err(_) => break,
            }
        }

        // Periodic data rate report (once/sec).
        let now = std::time::Instant::now();
        if now.duration_since(last_report) >= report_interval {
            let elapsed = now.duration_since(last_report).as_secs_f64().max(0.001);
            let bps = (bytes_since_report as f64 / elapsed) as u64;
            let _ = server_tx.send(ServerEvent::DataRate { bytes_per_sec: bps });
            bytes_since_report = 0;
            last_report = now;
        }

        tokio::select! {
            incoming = framed.next() => {
                match incoming {
                    Some(Ok(Message::StillFrame(sf))) => {
                        bytes_since_report += sf.data.len() as u64;
                        let pixels = rgba_to_pixels(&sf.data);
                        let _ = server_tx.send(ServerEvent::Frame { width: sf.width, height: sf.height, pixels });
                    }
                    Some(Ok(Message::VideoFrame(vf))) => {
                        if vf.data.is_empty() { continue; }
                        bytes_since_report += vf.data.len() as u64;
                        let _ = decode_tx.send(DecodeJob { data: vf.data, codec: vf.codec });
                    }
                    Some(Ok(Message::AudioChunk(ac))) => {
                        bytes_since_report += ac.data.len() as u64;
                        if let Some(ref atx) = audio_tx {
                            let _ = atx.send(AudioJob { data: ac.data });
                        }
                    }
                    Some(Ok(Message::CursorUpdate(cu))) => {
                        bytes_since_report += cu.image_rgba.len() as u64;
                        let _ = server_tx.send(ServerEvent::CursorUpdate(cu));
                    }
                    Some(Ok(Message::ClipboardData(cp))) => {
                        tracing::debug!("Clipboard received ({} bytes)", cp.data.len());
                        use std::io::Write;
                        if let Ok(mut child) = std::process::Command::new("wl-copy")
                            .args(["--type", if cp.mime_type.is_empty() { "text/plain" } else { &cp.mime_type }])
                            .stdin(std::process::Stdio::piped())
                            .stdout(std::process::Stdio::null())
                            .stderr(std::process::Stdio::null())
                            .spawn()
                        {
                            if let Some(mut stdin) = child.stdin.take() {
                                let _ = stdin.write_all(&cp.data);
                            }
                            let _ = child.wait();
                        }
                    }
                    Some(Ok(Message::FileTransferData(payload))) => {
                        // Server sent us files copied on its (remote) clipboard -
                        // write them locally and point our own clipboard at them
                        // so a Ctrl+V in a local app pastes the real files.
                        std::thread::spawn(move || receive_file_transfer(payload));
                    }
                    Some(Ok(Message::SessionEnd(se))) => {
                        // Received FROM the server (our own clean exit takes
                        // the ClientCommand::Disconnect path above and never
                        // loops back around to read this) - the server
                        // decided the session is over. Final, don't retry.
                        return Ok(LoopExit::SessionEnded { reason: se.reason });
                    }
                    Some(Ok(_)) => {}
                    Some(Err(e)) => {
                        tracing::error!("Protocol: {e}");
                        return Ok(LoopExit::ConnectionLost);
                    }
                    None => {
                        tracing::info!("Connection closed (EOF)");
                        return Ok(LoopExit::ConnectionLost);
                    }
                }
            }
            _ = tokio::time::sleep(tokio::time::Duration::from_millis(100)) => {}
        }
    }
}

/// Audio playback thread: decodes Opus packets and writes PCM to cpal output.
fn audio_playback_thread(rx: std::sync::mpsc::Receiver<AudioJob>) {
    use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

    let host = cpal::default_host();
    let device = match host.default_output_device() {
        Some(d) => d,
        None => {
            tracing::warn!("No audio output device found — audio disabled");
            return;
        }
    };

    let config = cpal::StreamConfig {
        channels: termland_codec::audio::CHANNELS as u16,
        sample_rate: cpal::SampleRate(termland_codec::audio::SAMPLE_RATE),
        buffer_size: cpal::BufferSize::Default,
    };

    let mut opus_dec = match termland_codec::OpusDecoder::new() {
        Ok(d) => d,
        Err(e) => {
            tracing::error!("Opus decoder init: {e}");
            return;
        }
    };

    // Ring buffer: decoded PCM samples waiting for cpal to consume.
    let ring = std::sync::Arc::new(std::sync::Mutex::new(std::collections::VecDeque::<i16>::with_capacity(48000)));
    let ring_write = ring.clone();

    let stream = match device.build_output_stream(
        &config,
        move |out: &mut [i16], _: &cpal::OutputCallbackInfo| {
            let mut ring = ring.lock().unwrap();
            for sample in out.iter_mut() {
                *sample = ring.pop_front().unwrap_or(0);
            }
        },
        |e| tracing::warn!("Audio stream error: {e}"),
        None,
    ) {
        Ok(s) => s,
        Err(e) => {
            tracing::error!("Failed to build audio stream: {e}");
            return;
        }
    };

    if let Err(e) = stream.play() {
        tracing::error!("Failed to start audio playback: {e}");
        return;
    }

    tracing::info!("Audio playback started");

    while let Ok(job) = rx.recv() {
        match opus_dec.decode(&job.data) {
            Ok(pcm) => {
                let mut ring = ring_write.lock().unwrap();
                // Cap buffer at ~500ms to prevent latency buildup while
                // allowing enough headroom to absorb jitter.
                let max_buffered = (termland_codec::audio::SAMPLE_RATE as usize) * (termland_codec::audio::CHANNELS as usize);
                if ring.len() > max_buffered {
                    let drain = ring.len() - max_buffered / 2;
                    ring.drain(..drain);
                }
                ring.extend(pcm.iter());
            }
            Err(e) => tracing::warn!("Opus decode: {e}"),
        }
    }

    tracing::info!("Audio playback thread exiting");
}

/// Dedicated decode thread - video decoder (AV1/VP9/VP8/H.265/H.264) + YUV→pixel conversion.
/// Runs independently of the network task so input is never blocked.
fn decode_thread(
    rx: std::sync::mpsc::Receiver<DecodeJob>,
    display_tx: mpsc::UnboundedSender<ServerEvent>,
    negotiated_codec: Option<VideoCodec>,
) {
    // Build the decoder for the codec the server negotiated. If the server
    // didn't announce one (older server), auto-detect across all codecs.
    let build = |codec: Option<VideoCodec>| match codec {
        Some(c) => termland_codec::VideoDecoder::for_codec(c),
        None => termland_codec::VideoDecoder::new(),
    };
    let mut decoder = match build(negotiated_codec) {
        Ok(d) => d,
        Err(e) => {
            tracing::error!("video decoder init: {e}");
            return;
        }
    };

    let mut count: u64 = 0;

    while let Ok(job) = rx.recv() {
        // If the server switched codec mid-stream (e.g. encoder reinit after a
        // resize landed on a different backend), rebuild the decoder to match.
        if let Some(c) = job.codec {
            if c != decoder.codec() {
                tracing::info!("Stream codec changed to {c}, rebuilding decoder");
                match termland_codec::VideoDecoder::for_codec(c) {
                    Ok(d) => decoder = d,
                    Err(e) => { tracing::error!("rebuild decoder for {c}: {e}"); continue; }
                }
            }
        }
        match decoder.decode(&job.data) {
            Ok((w, h, pixels)) => {
                let _ = display_tx.send(ServerEvent::Frame { width: w, height: h, pixels });
                count += 1;
                if count == 1 {
                    tracing::info!("First frame decoded (backend: {}) - {}x{}", decoder.backend(), w, h);
                }
            }
            Err(termland_codec::decoder::DecoderError::NoFrame) => {}
            Err(e) => tracing::warn!("decode: {e}"),
        }
    }

    tracing::info!("Decode thread exiting ({count} frames decoded)");
}

/// Check the local clipboard for a `text/uri-list` (files copied via a
/// desktop file manager's "Copy" action) and, if present and within the size
/// cap, send the files to the server as `ClientCommand::FileTransferSend`.
///
/// Called from `display.rs`'s focus-gained handler, on the same background
/// thread as (and right after) the existing plain-text clipboard read - kept
/// here rather than in `display.rs` so the file-reading/size-cap logic lives
/// next to its server-side mirror (`termland-server`'s
/// `read_files_from_uri_list`/`clipboard_watch_thread`) instead of split
/// across crates.
pub fn send_clipboard_file_transfer(tx: &mpsc::UnboundedSender<ClientCommand>) {
    let has_uri_list = std::process::Command::new("wl-paste")
        .arg("--list-types")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .output()
        .map(|o| {
            o.status.success()
                && String::from_utf8_lossy(&o.stdout).lines().any(|l| l.trim() == "text/uri-list")
        })
        .unwrap_or(false);
    if !has_uri_list {
        return;
    }

    let uri_output = match std::process::Command::new("wl-paste")
        .args(["--type", "text/uri-list", "--no-newline"])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .output()
    {
        Ok(o) if o.status.success() && !o.stdout.is_empty() => o.stdout,
        _ => return,
    };

    let text = String::from_utf8_lossy(&uri_output);
    let paths = parse_uri_list(&text);
    if paths.is_empty() {
        return;
    }

    let mut files = Vec::new();
    let mut total: u64 = 0;
    for path in &paths {
        let data = match std::fs::read(path) {
            Ok(d) => d,
            Err(e) => {
                tracing::warn!("clipboard file transfer: skipping unreadable {}: {e}", path.display());
                continue;
            }
        };
        total += data.len() as u64;
        if total > MAX_FILE_TRANSFER_BYTES {
            tracing::warn!(
                "clipboard file transfer: total size of {} file(s) exceeds the {} MiB cap, not sending",
                paths.len(),
                MAX_FILE_TRANSFER_BYTES / (1024 * 1024),
            );
            return;
        }
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "file".to_string());
        files.push(FileEntry { name, data });
    }

    if !files.is_empty() {
        tracing::debug!("Clipboard file list changed ({} file(s)), sending to server", files.len());
        let _ = tx.send(ClientCommand::FileTransferSend(FileTransferPayload { files }));
    }
}

/// `$XDG_CACHE_HOME/termland/clipboard-files` (falling back to
/// `~/.cache/termland/clipboard-files`, then `std::env::temp_dir()/termland-clipboard-files`
/// if `$HOME` isn't set either). Scratch storage for files received via
/// clipboard file-paste (`Message::FileTransferData`) - ephemeral by
/// convention (cache dir / temp dir), not explicitly cleaned up by this
/// process; a long-running client could accumulate files here across many
/// pastes, same tradeoff the server makes for its own runtime-dir scratch
/// space (see `registry::clipboard_files_dir`'s doc comment).
///
/// This crate deliberately doesn't depend on the `dirs` crate for XDG
/// lookups - see `profile.rs`'s local `mod dirs` for the existing precedent
/// (a ~10-line lookup doesn't justify the dependency); this mirrors that
/// convention for `XDG_CACHE_HOME` instead of `XDG_CONFIG_HOME`.
fn client_clipboard_files_dir() -> std::path::PathBuf {
    let cache_dir = std::env::var_os("XDG_CACHE_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".cache")));
    match cache_dir {
        Some(d) => d.join("termland").join("clipboard-files"),
        None => std::env::temp_dir().join("termland-clipboard-files"),
    }
}

/// A short, time-keyed subdirectory name for one clipboard file transfer -
/// just needs to be unique across successive transfers in this process, not
/// globally unique or unguessable. Mirrors the server's
/// `transport::transfer_subdir_name` (same reasoning: a fresh subdir per
/// transfer avoids a later paste's filenames overwriting an earlier paste's
/// files while the local clipboard might still reference the earlier ones).
fn transfer_subdir_name() -> String {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis().to_string())
        .unwrap_or_else(|_| "0".to_string())
}

/// Handle an inbound `Message::FileTransferData` from the server: write the
/// files into a fresh subdirectory under `client_clipboard_files_dir()` and
/// point the local clipboard at them via `wl-copy --type text/uri-list`, so a
/// `Ctrl+V` in a local app pastes the real files the server sent.
fn receive_file_transfer(payload: FileTransferPayload) {
    if payload.files.is_empty() {
        return;
    }

    // Defense in depth: the server is expected to enforce
    // MAX_FILE_TRANSFER_BYTES before sending, but re-check here rather than
    // trust the wire.
    let total: u64 = payload.files.iter().map(|f| f.data.len() as u64).sum();
    if total > termland_protocol::MAX_FILE_TRANSFER_BYTES {
        tracing::warn!(
            "rejecting FileTransferData: {} bytes exceeds the {} MiB cap",
            total,
            termland_protocol::MAX_FILE_TRANSFER_BYTES / (1024 * 1024),
        );
        return;
    }

    let dir = client_clipboard_files_dir().join(transfer_subdir_name());
    if let Err(e) = std::fs::create_dir_all(&dir) {
        tracing::warn!("clipboard file transfer: failed to create scratch dir {}: {e}", dir.display());
        return;
    }

    let mut written_paths = Vec::new();
    for entry in &payload.files {
        // SECURITY: entry.name arrives from the server over the wire - see
        // the identical reasoning in termland-server's receive_file_transfer.
        // Only a bare basename is ever accepted; anything else is dropped.
        let Some(safe_name) = termland_protocol::sanitize_filename(&entry.name) else {
            tracing::warn!("clipboard file transfer: rejecting unsafe filename {:?}", entry.name);
            continue;
        };
        let path = dir.join(&safe_name);
        if let Err(e) = std::fs::write(&path, &entry.data) {
            tracing::warn!("clipboard file transfer: failed to write {}: {e}", path.display());
            continue;
        }
        written_paths.push(path);
    }

    if written_paths.is_empty() {
        tracing::warn!("clipboard file transfer: no files were written (all rejected/failed)");
        return;
    }

    let uri_list = termland_protocol::build_uri_list(&written_paths);
    use std::io::Write;
    let mut child = match std::process::Command::new("wl-copy")
        .args(["--type", "text/uri-list"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!("clipboard file transfer: wl-copy failed: {e}");
            return;
        }
    };
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(uri_list.as_bytes());
    }
    let _ = child.wait();

    tracing::info!(
        "Clipboard file transfer: wrote {} file(s) to {} and set local clipboard",
        written_paths.len(),
        dir.display()
    );
}

#[cfg(test)]
mod tests {
    use super::backoff_delay;
    use std::time::Duration;

    #[test]
    fn backoff_schedule_doubles_then_caps() {
        assert_eq!(backoff_delay(1), Duration::from_secs(1));
        assert_eq!(backoff_delay(2), Duration::from_secs(2));
        assert_eq!(backoff_delay(3), Duration::from_secs(4));
        assert_eq!(backoff_delay(4), Duration::from_secs(8));
        assert_eq!(backoff_delay(5), Duration::from_secs(16));
        assert_eq!(backoff_delay(6), Duration::from_secs(30));
        assert_eq!(backoff_delay(7), Duration::from_secs(30));
    }

    #[test]
    fn backoff_stays_capped_for_many_attempts() {
        // Retrying "indefinitely" per the design must never overflow or
        // exceed the cap, however large `attempt` gets.
        for attempt in [10u32, 100, 1_000, u32::MAX] {
            assert_eq!(backoff_delay(attempt), Duration::from_secs(30));
        }
    }
}
