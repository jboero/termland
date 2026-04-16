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
    #[allow(dead_code)]
    Pong(Pong),
    Disconnected,
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
}

pub async fn connect(
    server: &str, ssh: bool, params: ConnectParams,
) -> Result<(mpsc::UnboundedReceiver<ServerEvent>, mpsc::UnboundedSender<ClientCommand>)> {
    let (server_tx, server_rx) = mpsc::unbounded_channel();
    let (client_tx, client_rx) = mpsc::unbounded_channel();

    if ssh {
        let mut ssh_args: Vec<String> = params.ssh_opts.clone();
        ssh_args.extend(["-s".into(), server.to_string(), "termland".into()]);
        let child = Command::new("ssh")
            .args(&ssh_args)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::inherit())
            .spawn().context("failed to spawn ssh")?;
        let io = tokio::io::join(child.stdout.unwrap(), child.stdin.unwrap());
        tokio::spawn(async move {
            if let Err(e) = session_loop(io, params, server_tx, client_rx).await {
                tracing::error!("Session error: {e}");
            }
        });
    } else if params.tls {
        let stream = TcpStream::connect(server).await
            .context(format!("failed to connect to {server}"))?;
        tracing::info!("Connected to {server} (TLS)");

        let mut root_store = rustls::RootCertStore::empty();
        // Add system roots
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

        tokio::spawn(async move {
            if let Err(e) = session_loop(tls_stream, params, server_tx, client_rx).await {
                tracing::error!("Session error: {e}");
            }
        });
    } else {
        let stream = TcpStream::connect(server).await
            .context(format!("failed to connect to {server}"))?;
        tracing::info!("Connected to {server}");
        tokio::spawn(async move {
            if let Err(e) = session_loop(stream, params, server_tx, client_rx).await {
                tracing::error!("Session error: {e}");
            }
        });
    }
    Ok((server_rx, client_tx))
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

/// AV1 packet for the decode thread.
struct DecodeJob {
    data: Vec<u8>,
}

async fn session_loop<T: AsyncRead + AsyncWrite + Unpin>(
    io: T, params: ConnectParams,
    server_tx: mpsc::UnboundedSender<ServerEvent>,
    mut client_rx: mpsc::UnboundedReceiver<ClientCommand>,
) -> Result<()> {
    let mut framed = Framed::new(io, TermlandCodec);

    // Handshake
    framed.send(Message::Hello(Hello { protocol_version: PROTOCOL_VERSION, client_name: "termland-client".into() })).await?;
    let msg = framed.next().await.context("closed")?.context("decode")?;
    match &msg {
        Message::HelloAck(ha) => tracing::info!("Server: {} (v{}, session {})", ha.server_name, ha.protocol_version, ha.session_id),
        other => anyhow::bail!("expected HelloAck, got {:?}", other.message_id()),
    }

    // Handle auth if server requires it (indicated by auth_required in HelloAck)
    let auth_required = match &msg {
        Message::HelloAck(ha) => ha.auth_required,
        _ => false,
    };

    if auth_required {
        // Wait for AuthRequest
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
            Message::AuthResult(ar) if ar.success => {
                tracing::info!("Authenticated as '{username}'");
            }
            Message::AuthResult(ar) => {
                anyhow::bail!("Authentication failed: {}", ar.message);
            }
            other => anyhow::bail!("expected AuthResult, got {:?}", other.message_id()),
        }
    }

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
    })).await?;
    let msg = framed.next().await.context("closed")?.context("decode")?;
    match &msg {
        Message::SessionReady(sr) => {
            tracing::info!("Session ready: {}x{}", sr.width, sr.height);
            let _ = server_tx.send(ServerEvent::SessionReady(sr.clone()));
        }
        other => anyhow::bail!("expected SessionReady, got {:?}", other.message_id()),
    }

    // Spawn a dedicated decode thread so it doesn't block the network task
    let (decode_tx, decode_rx) = std::sync::mpsc::channel::<DecodeJob>();
    let display_tx = server_tx.clone();
    let _decode_thread = std::thread::spawn(move || {
        decode_thread(decode_rx, display_tx);
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
                    return Ok(());
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
                        let _ = decode_tx.send(DecodeJob { data: vf.data });
                    }
                    Some(Ok(Message::AudioChunk(ac))) => {
                        bytes_since_report += ac.data.len() as u64;
                        if let Some(ref atx) = audio_tx {
                            let _ = atx.send(AudioJob { data: ac.data });
                        }
                    }
                    Some(Ok(Message::SessionEnd(se))) => {
                        tracing::info!("Session ended: {}", se.reason);
                        let _ = server_tx.send(ServerEvent::Disconnected);
                        break;
                    }
                    Some(Ok(_)) => {}
                    Some(Err(e)) => { tracing::error!("Protocol: {e}"); let _ = server_tx.send(ServerEvent::Disconnected); break; }
                    None => { tracing::info!("Disconnected"); let _ = server_tx.send(ServerEvent::Disconnected); break; }
                }
            }
            _ = tokio::time::sleep(tokio::time::Duration::from_millis(100)) => {}
        }
    }
    Ok(())
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

/// Dedicated decode thread - dav1d decode + YUV→pixel conversion.
/// Runs independently of the network task so input is never blocked.
fn decode_thread(
    rx: std::sync::mpsc::Receiver<DecodeJob>,
    display_tx: mpsc::UnboundedSender<ServerEvent>,
) {
    let mut decoder = match termland_codec::Av1Decoder::new() {
        Ok(d) => d,
        Err(e) => {
            tracing::error!("dav1d init: {e}");
            return;
        }
    };

    let mut count: u64 = 0;

    while let Ok(job) = rx.recv() {
        match decoder.decode(&job.data) {
            Ok((w, h, pixels)) => {
                let _ = display_tx.send(ServerEvent::Frame { width: w, height: h, pixels });
                count += 1;
                if count == 1 {
                    tracing::info!("First AV1 frame decoded ({})", decoder.backend());
                }
            }
            Err(termland_codec::decoder::DecoderError::NoFrame) => {}
            Err(e) => tracing::warn!("decode: {e}"),
        }
    }

    tracing::info!("Decode thread exiting ({count} frames decoded)");
}
