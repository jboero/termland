use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use futures::{SinkExt, StreamExt};
use termland_protocol::*;
use tokio::sync::mpsc;
use tokio_util::codec::Framed;

use crate::error::{Result, TermlandError};
use crate::transport::{Io, Transport};
use crate::types::{
    MobileCodec, ServerProfile, SessionParams, SessionObserver, SessionReadyInfo, SessionSummary,
    VideoPacket,
};

/// The framed protocol stream, whatever transport is underneath.
pub(crate) type Conn = Framed<Box<dyn Io>, TermlandCodec>;

/// Input and control messages queued by the (non-blocking) FFI setters and
/// drained by the session loop. Unbounded so a UI-thread call never waits on
/// the network.
pub(crate) enum InputCommand {
    Key(input::KeyEvent),
    Text(TextInput),
    Motion(input::MouseMove),
    Button(input::MouseButton),
    Scroll(input::MouseScroll),
    Clipboard(ClipboardPayload),
    Resize(SessionResize),
    CursorInFrame(bool),
}

impl InputCommand {
    fn into_message(self) -> Message {
        match self {
            InputCommand::Key(e) => Message::KeyEvent(e),
            InputCommand::Text(t) => Message::TextInput(t),
            InputCommand::Motion(m) => Message::MouseMove(m),
            InputCommand::Button(b) => Message::MouseButton(b),
            InputCommand::Scroll(s) => Message::MouseScroll(s),
            InputCommand::Clipboard(c) => Message::ClipboardSend(c),
            InputCommand::Resize(r) => Message::SessionResize(r),
            InputCommand::CursorInFrame(yes) => Message::CursorMode(CursorModeMsg {
                include_cursor_in_frame: yes,
            }),
        }
    }
}

/// Open the transport and complete Hello/HelloAck + optional PAM auth. Shared by
/// the streaming path and the one-shot control ops, exactly as on the desktop.
pub(crate) async fn connect_and_handshake(profile: &ServerProfile) -> Result<Conn> {
    let io = Transport::for_profile(profile)
        .connect(&profile.host, profile.port)
        .await?;
    let mut framed = Framed::new(io, TermlandCodec);

    framed
        .send(Message::Hello(Hello {
            protocol_version: PROTOCOL_VERSION,
            client_name: "termland-mobile".into(),
        }))
        .await?;

    let auth_required = match next_message(&mut framed).await? {
        Message::HelloAck(ha) => {
            tracing::info!(
                "Server: {} (v{}, session {})",
                ha.server_name,
                ha.protocol_version,
                ha.session_id
            );
            ha.auth_required
        }
        other => return Err(unexpected("HelloAck", &other)),
    };

    if auth_required {
        match next_message(&mut framed).await? {
            Message::AuthRequest(ar) => {
                tracing::info!("Server requires authentication (methods: {:?})", ar.methods)
            }
            other => return Err(unexpected("AuthRequest", &other)),
        }

        // No `whoami` fallback like the desktop client has: the device's local
        // user is meaningless to the remote PAM stack, so the profile must say.
        let username = profile
            .username
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| TermlandError::auth("server requires a username"))?
            .to_string();
        let credential = profile.password.clone().unwrap_or_default();
        if credential.is_empty() {
            tracing::warn!("server requires auth but the profile has no password");
        }

        framed
            .send(Message::AuthResponse(AuthResponse {
                username: username.clone(),
                credential,
            }))
            .await?;

        match next_message(&mut framed).await? {
            Message::AuthResult(ar) if ar.success => tracing::info!("Authenticated as '{username}'"),
            Message::AuthResult(ar) => return Err(TermlandError::auth(ar.message)),
            other => return Err(unexpected("AuthResult", &other)),
        }
    }

    Ok(framed)
}

pub(crate) async fn list_sessions(profile: &ServerProfile) -> Result<Vec<SessionSummary>> {
    let mut framed = connect_and_handshake(profile).await?;
    framed.send(Message::SessionList(SessionList {})).await?;
    match next_message(&mut framed).await? {
        Message::SessionListResult(r) => Ok(r
            .sessions
            .into_iter()
            .map(|s| SessionSummary {
                session_id: s.session_id,
                mode: s.mode,
                width: s.width,
                height: s.height,
                age_secs: s.age_secs,
                attached: s.attached,
            })
            .collect()),
        other => Err(unexpected("SessionListResult", &other)),
    }
}

pub(crate) async fn close_session(profile: &ServerProfile, session_id: String) -> Result<()> {
    let mut framed = connect_and_handshake(profile).await?;
    framed
        .send(Message::SessionClose(SessionClose { session_id }))
        .await?;
    // The server acks with SessionEnd; its contents carry no extra information.
    let _ = framed.next().await;
    Ok(())
}

/// Connect, hand over SessionCreate or SessionAttach, and wait for SessionReady.
/// Returns the live stream so the caller can spawn the loop only once the
/// session is genuinely up — that is what lets `connect_new`/`attach` report
/// setup failures as errors rather than through `on_error`.
pub(crate) async fn open_session(
    profile: &ServerProfile,
    attach_to: Option<String>,
    params: &SessionParams,
) -> Result<(Conn, SessionReadyInfo)> {
    let mut framed = connect_and_handshake(profile).await?;
    let supported_codecs = params.advertised_codecs();

    match attach_to {
        Some(session_id) => {
            tracing::info!("Attaching to session {session_id}");
            framed
                .send(Message::SessionAttach(SessionAttach {
                    session_id,
                    audio: params.audio,
                    quality: params.quality,
                    encoder_preset: None,
                    encoder_crf: None,
                    encoder_extra_params: None,
                    supported_codecs,
                }))
                .await?;
        }
        None => {
            framed
                .send(Message::SessionCreate(SessionCreate {
                    mode: params.session_mode(),
                    width: params.width,
                    height: params.height,
                    audio: params.audio,
                    quality: params.quality,
                    desktop_shell: params.desktop_shell.clone(),
                    encoder_preset: None,
                    encoder_crf: None,
                    encoder_extra_params: None,
                    supported_codecs,
                }))
                .await?;
        }
    }

    match next_message(&mut framed).await? {
        Message::SessionReady(sr) => {
            // MediaCodec has to be configured with a concrete codec before the
            // first packet, so unlike the desktop we cannot auto-detect from the
            // stream. Servers older than the codec-tag change don't announce
            // one; H.264 is the only codec every device decodes, so assume it.
            let codec = match sr.codec {
                Some(c) => {
                    tracing::info!("Session ready: {}x{} codec={c}", sr.width, sr.height);
                    c
                }
                None => {
                    tracing::warn!(
                        "Session ready: {}x{} but server announced no codec; assuming H.264",
                        sr.width,
                        sr.height
                    );
                    VideoCodec::H264
                }
            };
            let info = SessionReadyInfo {
                width: sr.width,
                height: sr.height,
                codec: codec.into(),
                session_id: sr.session_id,
            };
            Ok((framed, info))
        }
        other => Err(unexpected("SessionReady", &other)),
    }
}

/// Pump the session: route server messages to the observer, forward queued
/// input. Returns when the server ends the stream or the client detaches
/// (`input_rx` closed by `disconnect()`).
pub(crate) async fn run_session(
    mut framed: Conn,
    ready: SessionReadyInfo,
    observer: Arc<dyn SessionObserver>,
    mut input_rx: mpsc::UnboundedReceiver<InputCommand>,
    connected: Arc<AtomicBool>,
) {
    let negotiated = ready.codec;
    let mut bytes_since_report: u64 = 0;
    let mut last_report = std::time::Instant::now();
    let mut ticker = tokio::time::interval(Duration::from_secs(1));
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    let reason = loop {
        tokio::select! {
            // Input first: video is high-volume and would otherwise starve it,
            // and input latency is the one thing users feel immediately.
            biased;

            cmd = input_rx.recv() => {
                match cmd {
                    Some(cmd) => {
                        if let Err(e) = framed.send(cmd.into_message()).await {
                            observer.on_error(format!("send failed: {e}"));
                            break "send failed".to_string();
                        }
                    }
                    // Sender dropped == disconnect(): detach without telling the
                    // server anything, which leaves the session resumable.
                    None => break "detached".to_string(),
                }
            }

            _ = ticker.tick() => {
                // Normalise by the real elapsed time: a phone that gets
                // throttled or backgrounded can miss ticks by a wide margin,
                // and a raw byte count would then read as a bandwidth drop.
                let now = std::time::Instant::now();
                let elapsed = now.duration_since(last_report).as_secs_f64().max(0.001);
                observer.on_data_rate((bytes_since_report as f64 / elapsed) as u64);
                bytes_since_report = 0;
                last_report = now;
            }

            incoming = framed.next() => {
                match incoming {
                    Some(Ok(Message::VideoFrame(vf))) => {
                        if vf.data.is_empty() { continue; }
                        bytes_since_report += vf.data.len() as u64;
                        observer.on_video_packet(VideoPacket {
                            timestamp_us: vf.timestamp_us,
                            keyframe: vf.frame_type == FrameType::Keyframe,
                            codec: vf.codec.map(MobileCodec::from).unwrap_or(negotiated),
                            // Older servers leave the per-frame dimensions at 0;
                            // fall back to what the session was set up with so
                            // the decoder always gets something usable.
                            width: if vf.width == 0 { ready.width } else { vf.width as u32 },
                            height: if vf.height == 0 { ready.height } else { vf.height as u32 },
                            data: vf.data,
                        });
                    }
                    Some(Ok(Message::AudioChunk(ac))) => {
                        bytes_since_report += ac.data.len() as u64;
                        // Opus stays encoded: decoding belongs to the platform
                        // audio stack, and pulling libopus in would defeat the
                        // point of a dependency-light core.
                        observer.on_audio_packet(ac.data);
                    }
                    Some(Ok(Message::ClipboardData(cp))) => {
                        bytes_since_report += cp.data.len() as u64;
                        observer.on_clipboard(cp.mime_type, cp.data);
                    }
                    Some(Ok(Message::SessionEnd(se))) => {
                        tracing::info!("Session ended: {}", se.reason);
                        break se.reason;
                    }
                    Some(Ok(Message::Ping(p))) => {
                        let _ = framed.send(Message::Pong(Pong { timestamp_us: p.timestamp_us })).await;
                    }
                    // StillFrame is raw RGBA for the desktop's software path and
                    // CursorUpdate is for client-drawn cursors; mobile renders
                    // neither, so both are dropped rather than shipped over FFI.
                    Some(Ok(_)) => {}
                    Some(Err(e)) => {
                        tracing::error!("Protocol: {e}");
                        observer.on_error(format!("protocol error: {e}"));
                        break "protocol error".to_string();
                    }
                    None => break "connection closed".to_string(),
                }
            }
        }
    };

    connected.store(false, Ordering::SeqCst);
    tracing::info!("Session loop exiting: {reason}");
    observer.on_disconnected(reason);
}

async fn next_message(framed: &mut Conn) -> Result<Message> {
    match framed.next().await {
        Some(Ok(msg)) => Ok(msg),
        Some(Err(e)) => Err(e.into()),
        None => Err(TermlandError::io("connection closed by server")),
    }
}

fn unexpected(expected: &str, got: &Message) -> TermlandError {
    TermlandError::protocol(format!("expected {expected}, got {:?}", got.message_id()))
}
