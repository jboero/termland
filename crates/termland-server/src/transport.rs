use anyhow::{Context, Result};
use futures::{SinkExt, StreamExt};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::TcpListener;
use tokio_util::codec::Framed;
use termland_protocol::*;
use libpulse_binding as pulse;
use libpulse_simple_binding as psimple;

/// Run the server in SSH subsystem mode: protocol over stdin/stdout.
pub async fn run_subsystem() -> Result<()> {
    let stdin = tokio::io::stdin();
    let stdout = tokio::io::stdout();
    let io = tokio::io::join(stdin, stdout);
    handle_session(io, false).await
}

const MAX_CONCURRENT_SESSIONS: usize = 32;

/// Run the server as a TCP listener, optionally with TLS and PAM auth.
pub async fn run_tcp_listener(
    bind: &str,
    port: u16,
    tls_acceptor: Option<tokio_rustls::TlsAcceptor>,
    require_auth: bool,
) -> Result<()> {
    let listener = TcpListener::bind(format!("{bind}:{port}")).await?;
    tracing::info!("Listening on {bind}:{port} (max {MAX_CONCURRENT_SESSIONS} sessions)");

    let semaphore = Arc::new(tokio::sync::Semaphore::new(MAX_CONCURRENT_SESSIONS));

    loop {
        let (socket, addr) = listener.accept().await?;

        let permit = match semaphore.clone().try_acquire_owned() {
            Ok(p) => p,
            Err(_) => {
                tracing::warn!("Rejected connection from {addr}: max sessions reached");
                drop(socket);
                continue;
            }
        };

        tracing::info!("Connection from {addr} ({} active)", MAX_CONCURRENT_SESSIONS - semaphore.available_permits());
        let acceptor = tls_acceptor.clone();
        let auth = require_auth;
        tokio::spawn(async move {
            let result = if let Some(acceptor) = acceptor {
                match acceptor.accept(socket).await {
                    Ok(tls_stream) => {
                        tracing::info!("TLS handshake complete for {addr}");
                        handle_session(tls_stream, auth).await
                    }
                    Err(e) => {
                        tracing::warn!("TLS handshake failed for {addr}: {e}");
                        return;
                    }
                }
            } else {
                handle_session(socket, auth).await
            };
            if let Err(e) = result {
                tracing::error!("Session error for {addr}: {e}");
            }
            drop(permit);
        });
    }
}

/// Frame data from the capture thread (video-encoded).
enum CapturedFrame {
    Video {
        data: Vec<u8>,
        keyframe: bool,
        width: u16,
        height: u16,
        timestamp_us: u64,
        codec: termland_protocol::VideoCodec,
    },
}

/// Input commands sent to the input injection thread.
enum InputCommand {
    Key { scancode: u32, pressed: bool },
    PointerMove { x: f64, y: f64, width: u32, height: u32 },
    PointerButton { button: u32, pressed: bool },
    Scroll { dx: f64, dy: f64 },
    Stop,
}

/// Handle a single client session over any AsyncRead+AsyncWrite transport.
#[allow(clippy::too_many_lines)]
async fn handle_session<T>(io: T, require_auth: bool) -> Result<()>
where
    T: AsyncRead + AsyncWrite + Unpin,
{
    let mut framed = Framed::new(io, TermlandCodec);

    // Wait for Hello
    let msg = framed
        .next()
        .await
        .context("connection closed before Hello")?
        .context("failed to decode Hello")?;

    let hello = match msg {
        Message::Hello(h) => {
            tracing::info!(
                "Client hello: {} (protocol v{})",
                h.client_name,
                h.protocol_version
            );
            h
        }
        other => anyhow::bail!("expected Hello, got {:?}", other.message_id()),
    };

    // Send HelloAck
    let session_id = format!("session-{}", std::process::id());
    framed
        .send(Message::HelloAck(HelloAck {
            protocol_version: PROTOCOL_VERSION,
            server_name: "termland-server".into(),
            session_id: session_id.clone(),
            auth_required: require_auth,
        }))
        .await
        .context("failed to send HelloAck")?;

    if hello.protocol_version != PROTOCOL_VERSION {
        tracing::warn!(
            "Protocol version mismatch: client={}, server={}",
            hello.protocol_version,
            PROTOCOL_VERSION
        );
    }

    // Authentication (if required)
    if require_auth {
        framed.send(Message::AuthRequest(AuthRequest {
            methods: vec!["password".into()],
        })).await.context("failed to send AuthRequest")?;

        let msg = framed.next().await
            .context("connection closed before AuthResponse")?
            .context("failed to decode AuthResponse")?;

        let (username, password) = match msg {
            Message::AuthResponse(ar) => (ar.username, ar.credential),
            other => anyhow::bail!("expected AuthResponse, got {:?}", other.message_id()),
        };

        let ok = tokio::task::spawn_blocking(move || {
            crate::auth::pam_authenticate_user(&username, &password)
        }).await??;

        if !ok {
            // Delay before responding to slow down brute-force attempts
            tokio::time::sleep(tokio::time::Duration::from_secs(3)).await;
            framed.send(Message::AuthResult(AuthResult {
                success: false,
                message: "authentication failed".into(),
            })).await?;
            anyhow::bail!("authentication failed");
        }

        framed.send(Message::AuthResult(AuthResult {
            success: true,
            message: "authenticated".into(),
        })).await.context("failed to send AuthResult")?;

        tracing::info!("Client authenticated");
    }

    // Ensure the session registry directory exists.
    let _ = crate::registry::ensure_dir();

    // Session control loop. The client either manages sessions (list / close) or
    // starts one — create a new persistent session, or attach to an existing one
    // — which then streams until the client detaches (disconnects) or closes it.
    loop {
        let msg = match framed.next().await {
            Some(Ok(m)) => m,
            Some(Err(e)) => {
                tracing::warn!("control decode error: {e}");
                return Ok(());
            }
            None => {
                tracing::info!("Client disconnected before selecting a session");
                return Ok(());
            }
        };

        match msg {
            Message::SessionList(_) => {
                let sessions: Vec<SessionInfo> = crate::registry::list_alive()
                    .into_iter()
                    .map(|r| SessionInfo {
                        session_id: r.session_id,
                        mode: r.mode,
                        width: r.width,
                        height: r.height,
                        age_secs: crate::registry::now_unix().saturating_sub(r.created_at_unix),
                        attached: false,
                    })
                    .collect();
                tracing::info!("Listing {} live session(s)", sessions.len());
                framed
                    .send(Message::SessionListResult(SessionListResult { sessions }))
                    .await
                    .context("failed to send SessionListResult")?;
            }
            Message::SessionClose(sc) => {
                tracing::info!("Client requested close of session {}", sc.session_id);
                crate::registry::close(&sc.session_id);
                framed
                    .send(Message::SessionEnd(SessionEnd {
                        reason: format!("closed {}", sc.session_id),
                    }))
                    .await
                    .ok();
            }
            Message::SessionCreate(sc) => {
                run_session(&mut framed, SessionRequest::Create(sc)).await?;
                return Ok(());
            }
            Message::SessionAttach(sa) => {
                run_session(&mut framed, SessionRequest::Attach(sa)).await?;
                return Ok(());
            }
            other => {
                anyhow::bail!("expected a session control message, got {:?}", other.message_id());
            }
        }
    }
}

/// A client's request that starts streaming: a new session or a resume.
enum SessionRequest {
    Create(SessionCreate),
    Attach(SessionAttach),
}

/// Run one streaming session (freshly created or resumed) until the client
/// detaches or closes it. On plain disconnect the compositor is left running
/// (the session persists); on an explicit close/compositor-exit it is removed.
async fn run_session<T>(
    framed: &mut Framed<T, TermlandCodec>,
    request: SessionRequest,
) -> Result<()>
where
    T: AsyncRead + AsyncWrite + Unpin,
{
    // Resolve everything the capture/stream setup needs from either request kind.
    let (
        session_id,
        width,
        height,
        quality,
        mode,
        mode_desc,
        desktop_shell,
        encoder_preset,
        encoder_crf,
        encoder_extra_params,
        supported_codecs,
        audio,
        start_kind,
        is_new,
    );

    match request {
        SessionRequest::Create(sc) => {
            tracing::info!(
                "Create session: {}x{} mode={:?} audio={} quality={} desktop_shell={:?}",
                sc.width, sc.height, sc.mode, sc.audio, sc.quality, sc.desktop_shell
            );
            let ds = sc.desktop_shell.clone().filter(|s| !s.is_empty());
            let ep = sc.encoder_preset.clone().filter(|s| !s.is_empty());
            let ex = sc.encoder_extra_params.clone().filter(|s| !s.is_empty());

            // SECURITY: validate client-supplied commands before any shell use.
            if let Some(ref shell) = ds {
                termland_compositor::validate_shell_command(shell)
                    .map_err(|e| anyhow::anyhow!("rejected desktop_shell: {e}"))?;
            }
            if let termland_protocol::SessionMode::App { ref command, ref args } = sc.mode {
                termland_compositor::validate_shell_command(command)
                    .map_err(|e| anyhow::anyhow!("rejected app command: {e}"))?;
                for arg in args {
                    termland_compositor::validate_shell_command(arg)
                        .map_err(|e| anyhow::anyhow!("rejected app arg: {e}"))?;
                }
            }
            if let Some(ref preset) = ep {
                termland_compositor::validate_shell_command(preset)
                    .map_err(|e| anyhow::anyhow!("rejected encoder_preset: {e}"))?;
            }
            if let Some(ref extra) = ex {
                termland_compositor::validate_shell_command(extra)
                    .map_err(|e| anyhow::anyhow!("rejected encoder_extra_params: {e}"))?;
            }

            let id = crate::registry::new_session_id();
            session_id = id.clone();
            width = sc.width;
            height = sc.height;
            quality = sc.quality.clamp(1, 100);
            mode_desc = crate::registry::describe_mode(&sc.mode);
            mode = sc.mode.into();
            desktop_shell = ds;
            encoder_preset = ep;
            encoder_crf = sc.encoder_crf;
            encoder_extra_params = ex;
            supported_codecs = if sc.supported_codecs.is_empty() {
                termland_protocol::VideoCodec::all_preferred()
            } else {
                sc.supported_codecs
            };
            audio = sc.audio;
            start_kind = StartKind::Create { log_path: crate::registry::log_path(&id) };
            is_new = true;
        }
        SessionRequest::Attach(sa) => {
            let Some(rec) = crate::registry::read(&sa.session_id).filter(crate::registry::is_live) else {
                tracing::warn!("attach to unknown/dead session {}", sa.session_id);
                framed
                    .send(Message::SessionEnd(SessionEnd {
                        reason: format!("no such session: {}", sa.session_id),
                    }))
                    .await
                    .ok();
                return Ok(());
            };
            tracing::info!("Attaching to session {} ({})", rec.session_id, rec.wayland_display);
            let ep = sa.encoder_preset.clone().filter(|s| !s.is_empty());
            let ex = sa.encoder_extra_params.clone().filter(|s| !s.is_empty());
            if let Some(ref preset) = ep {
                termland_compositor::validate_shell_command(preset)
                    .map_err(|e| anyhow::anyhow!("rejected encoder_preset: {e}"))?;
            }
            if let Some(ref extra) = ex {
                termland_compositor::validate_shell_command(extra)
                    .map_err(|e| anyhow::anyhow!("rejected encoder_extra_params: {e}"))?;
            }
            session_id = rec.session_id.clone();
            width = rec.width;
            height = rec.height;
            quality = sa.quality.clamp(1, 100);
            mode_desc = rec.mode.clone();
            mode = termland_compositor::SessionMode::Desktop; // unused on attach
            desktop_shell = None;
            encoder_preset = ep;
            encoder_crf = sa.encoder_crf;
            encoder_extra_params = ex;
            supported_codecs = if sa.supported_codecs.is_empty() {
                termland_protocol::VideoCodec::all_preferred()
            } else {
                sa.supported_codecs
            };
            audio = sa.audio;
            start_kind = StartKind::Attach {
                wayland_display: rec.wayland_display.clone(),
                compositor_pid: rec.compositor_pid,
            };
            is_new = false;
        }
    }
    tracing::info!("Client supports codecs: {supported_codecs:?}");

    // Spawn the compositor + capture loop on a blocking thread.
    let (frame_tx, mut frame_rx) = tokio::sync::mpsc::channel::<CapturedFrame>(2);
    let (display_tx, display_rx) = tokio::sync::oneshot::channel::<(String, u32)>();
    let (stop_tx, stop_rx) = tokio::sync::oneshot::channel::<()>();
    // Resize requests from the session loop → capture thread.
    let (resize_tx, resize_rx) = std::sync::mpsc::channel::<(u32, u32)>();

    // Shared cursor-mode flag. Client defaults to client-side cursor rendering.
    let overlay_cursor = Arc::new(AtomicBool::new(false));
    let overlay_cursor_capture = overlay_cursor.clone();

    let capture_codecs = supported_codecs.clone();
    let capture_handle = std::thread::spawn(move || {
        capture_thread(width, height, quality, mode, desktop_shell,
                       encoder_preset, encoder_crf, encoder_extra_params,
                       capture_codecs, start_kind,
                       overlay_cursor_capture, resize_rx, frame_tx, display_tx, stop_rx);
    });

    // Wait for the compositor to report its Wayland display name + pid.
    let (wayland_display, compositor_pid) = tokio::time::timeout(
        tokio::time::Duration::from_secs(15),
        async { display_rx.await },
    )
    .await
    .context("timeout waiting for compositor to start")?
    .context("capture thread died before compositor started")?;

    // For a freshly created session, record it in the registry so it can be
    // listed and resumed later. Resumes reuse the existing record.
    if is_new {
        let rec = crate::registry::SessionRecord {
            session_id: session_id.clone(),
            compositor_pid,
            wayland_display: wayland_display.clone(),
            mode: mode_desc.clone(),
            width,
            height,
            created_at_unix: crate::registry::now_unix(),
            audio,
        };
        if let Err(e) = crate::registry::write(&rec) {
            tracing::warn!("failed to write session registry record: {e}");
        }
    }

    // Spawn input injection thread connected to the same Wayland display
    let (input_tx, input_rx) = std::sync::mpsc::channel::<InputCommand>();
    let input_display = wayland_display.clone();
    let input_handle = std::thread::spawn(move || {
        input_thread(&input_display, width, height, input_rx);
    });

    // Wait for first frame
    let first_frame = tokio::time::timeout(
        tokio::time::Duration::from_secs(10),
        frame_rx.recv(),
    )
    .await
    .context("timeout waiting for first frame")?
    .context("capture thread died before producing a frame")?;

    let (first_w, first_h, first_codec) = match &first_frame {
        CapturedFrame::Video { width: w, height: h, codec, .. } => (*w as u32, *h as u32, *codec),
    };

    // Send SessionReady, announcing the negotiated codec so the client builds
    // the matching decoder up front instead of guessing from the stream.
    framed
        .send(Message::SessionReady(SessionReady {
            width: first_w,
            height: first_h,
            xkb_keymap: None,
            codec: Some(first_codec),
            session_id: session_id.clone(),
        }))
        .await
        .context("failed to send SessionReady")?;

    tracing::info!("Session {session_id} active");
    tracing::info!("  Compositor: headless wlroots on {wayland_display}");
    tracing::info!("  Resolution: {first_w}x{first_h}");
    tracing::info!("  Video codec: {first_codec}");
    tracing::info!("  Transport: TCP");

    // Optionally start audio capture if the client requested it.
    let (mut audio_rx, _audio_stop_tx, _audio_handle) = if audio {
        let (atx, arx) = tokio::sync::mpsc::channel::<AudioChunk>(8);
        let (astop_tx, astop_rx) = tokio::sync::oneshot::channel::<()>();
        let sid = session_id.clone();
        let h = std::thread::spawn(move || {
            audio_capture_thread(&sid, atx, astop_rx);
        });
        tracing::info!("  Audio encoder: Opus 48kHz stereo 64kbps");
        (Some(arx), Some(astop_tx), Some(h))
    } else {
        tracing::info!("  Audio encoder: disabled (client did not request audio)");
        (None, None, None)
    };
    // Shared hash: tracks content set by the client so the watch thread
    // doesn't echo it back.
    let client_clip_hash = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let client_clip_hash_watch = client_clip_hash.clone();

    // Spawn clipboard watch thread
    let (mut clip_rx, _clip_stop_tx, _clip_handle) = {
        let (ctx, crx) = tokio::sync::mpsc::channel::<ClipboardPayload>(4);
        let (cstop_tx, cstop_rx) = tokio::sync::oneshot::channel::<()>();
        let clip_display = wayland_display.clone();
        let h = std::thread::spawn(move || {
            clipboard_watch_thread(&clip_display, ctx, cstop_rx, client_clip_hash_watch);
        });
        tracing::info!("  Clipboard: bidirectional via wl-clipboard");
        (Some(crx), Some(cstop_tx), Some(h))
    };

    // Send the first frame
    let first_msg = frame_to_message(&first_frame);
    framed.send(first_msg).await.context("failed to send first frame")?;

    let mut frame_num: u64 = 1;
    let mut current_width = first_w;
    let mut current_height = first_h;

    // How this streaming session ends. Default: the client detached (the
    // compositor keeps running and the session stays resumable).
    let mut outcome = SessionOutcome::Detached;

    // Main event loop
    loop {
        tokio::select! {
            frame = frame_rx.recv() => {
                let Some(frame) = frame else {
                    // Capture thread ended because the compositor exited.
                    tracing::info!("Compositor exited, ending session");
                    let _ = framed.send(Message::SessionEnd(SessionEnd {
                        reason: "compositor exited".into(),
                    })).await;
                    outcome = SessionOutcome::Gone;
                    break;
                };

                let (fw, fh) = match &frame {
                    CapturedFrame::Video { width: w, height: h, .. } => (*w as u32, *h as u32),
                };
                current_width = fw;
                current_height = fh;

                let msg = frame_to_message(&frame);

                if let Err(e) = framed.send(msg).await {
                    tracing::error!("Failed to send frame: {e}");
                    break;
                }

                frame_num += 1;
                if frame_num % 30 == 0 {
                    tracing::debug!("Sent frame {frame_num}");
                }
            }

            audio = async {
                match audio_rx.as_mut() {
                    Some(rx) => rx.recv().await,
                    None => std::future::pending().await,
                }
            } => {
                if let Some(chunk) = audio {
                    if let Err(e) = framed.send(Message::AudioChunk(chunk)).await {
                        tracing::error!("Failed to send audio: {e}");
                        break;
                    }
                }
            }

            clip = async {
                match clip_rx.as_mut() {
                    Some(rx) => rx.recv().await,
                    None => std::future::pending().await,
                }
            } => {
                if let Some(payload) = clip {
                    if let Err(e) = framed.send(Message::ClipboardData(payload)).await {
                        tracing::error!("Failed to send clipboard: {e}");
                        break;
                    }
                }
            }

            incoming = framed.next() => {
                match incoming {
                    Some(Ok(msg)) => {
                        match msg {
                            Message::SessionEnd(se) => {
                                tracing::info!("Client ended session: {}", se.reason);
                                break;
                            }
                            Message::Ping(p) => {
                                let _ = framed.send(Message::Pong(Pong {
                                    timestamp_us: p.timestamp_us,
                                })).await;
                            }
                            Message::SessionResize(sr) => {
                                let w = sr.width.clamp(320, 7680);
                                let h = sr.height.clamp(240, 4320);
                                tracing::info!("Client requested resize to {w}x{h}");
                                let _ = resize_tx.send((w, h));
                            }
                            Message::KeyEvent(ke) => {
                                let pressed = ke.state == termland_protocol::input::KeyState::Pressed;
                                tracing::debug!("Key received: scancode={} pressed={pressed}", ke.scancode);
                                let _ = input_tx.send(InputCommand::Key {
                                    scancode: ke.scancode,
                                    pressed,
                                });
                            }
                            Message::MouseMove(mm) => {
                                let _ = input_tx.send(InputCommand::PointerMove {
                                    x: mm.x,
                                    y: mm.y,
                                    width: current_width,
                                    height: current_height,
                                });
                            }
                            Message::MouseButton(mb) => {
                                let pressed = mb.state == termland_protocol::input::ButtonState::Pressed;
                                let _ = input_tx.send(InputCommand::PointerButton {
                                    button: mb.button,
                                    pressed,
                                });
                            }
                            Message::MouseScroll(ms) => {
                                let _ = input_tx.send(InputCommand::Scroll {
                                    dx: ms.dx,
                                    dy: ms.dy,
                                });
                            }
                            Message::CursorMode(cm) => {
                                overlay_cursor.store(cm.include_cursor_in_frame, Ordering::Relaxed);
                                tracing::info!("Cursor mode: {}", if cm.include_cursor_in_frame { "server-side (in frame)" } else { "client-side (local render)" });
                            }
                            Message::SessionClose(sc) if sc.session_id == session_id || sc.session_id.is_empty() => {
                                tracing::info!("Client closed the active session");
                                outcome = SessionOutcome::Closed;
                                break;
                            }
                            Message::ClipboardSend(cp) => {
                                // Record hash so watch thread doesn't echo it back
                                let hash = {
                                    use std::hash::{Hash, Hasher};
                                    let mut h = std::collections::hash_map::DefaultHasher::new();
                                    cp.data.hash(&mut h);
                                    h.finish()
                                };
                                client_clip_hash.store(hash, Ordering::Relaxed);
                                let wd = wayland_display.clone();
                                tokio::task::spawn_blocking(move || {
                                    set_remote_clipboard(&wd, &cp.data, &cp.mime_type);
                                });
                            }
                            _ => {}
                        }
                    }
                    Some(Err(e)) => {
                        tracing::error!("Decode error: {e}");
                        break;
                    }
                    None => {
                        tracing::info!("Client disconnected");
                        break;
                    }
                }
            }
        }
    }

    // Stop threads
    let _ = stop_tx.send(());
    let _ = input_tx.send(InputCommand::Stop);
    if let Some(astop) = _audio_stop_tx {
        let _ = astop.send(());
    }
    if let Some(cstop) = _clip_stop_tx {
        let _ = cstop.send(());
    }
    let _ = capture_handle.join();
    let _ = input_handle.join();
    if let Some(ah) = _audio_handle {
        let _ = ah.join();
    }
    if let Some(ch) = _clip_handle {
        let _ = ch.join();
    }

    match outcome {
        SessionOutcome::Detached => {
            // The compositor keeps running; the session stays resumable.
            tracing::info!("Session {session_id} detached (compositor still running, resumable)");
        }
        SessionOutcome::Closed => {
            crate::registry::close(&session_id);
            tracing::info!("Session {session_id} closed");
        }
        SessionOutcome::Gone => {
            crate::registry::remove(&session_id);
            tracing::info!("Session {session_id} ended (compositor gone)");
        }
    }
    Ok(())
}

/// How a streaming session ended, deciding the fate of its compositor.
enum SessionOutcome {
    /// Client went away; leave the compositor running (persist, resumable).
    Detached,
    /// Client explicitly closed the session; terminate the compositor.
    Closed,
    /// The compositor exited on its own; drop the (now dead) record.
    Gone,
}

/// Encoder tuning collected once per session from the client.
struct EncoderTuning {
    bitrate_kbps: u32,
    preset: Option<String>,
    crf: Option<u8>,
    extra_svt_params: Option<String>,
}

impl EncoderTuning {
    fn build_config(&self, width: u32, height: u32) -> termland_codec::EncoderConfig {
        termland_codec::EncoderConfig {
            width,
            height,
            fps: 30,
            bitrate_kbps: self.bitrate_kbps,
            keyframe_interval: 30,
            preset: self.preset.clone(),
            crf: self.crf,
            extra_svt_params: self.extra_svt_params.clone(),
        }
    }
}

/// Build a fresh video encoder for the given dimensions + tuning, restricted
/// to codecs the client advertised it can decode.
/// Panics if no encoder is available (no supported codec could initialize).
fn init_encoder(
    tuning: &EncoderTuning,
    width: u32,
    height: u32,
    supported_codecs: &[termland_protocol::VideoCodec],
) -> Box<dyn termland_codec::VideoEncoder> {
    let config = tuning.build_config(width, height);
    match termland_codec::probe_best_encoder(&config, supported_codecs) {
        Ok(enc) => {
            let backend = enc.backend();
            tracing::info!(
                "Video encoder ready for {width}x{height}: {backend} (codec {})",
                backend.codec()
            );
            enc
        }
        Err(e) => {
            panic!("No video encoder available for client-supported codecs {supported_codecs:?}: {e}");
        }
    }
}

/// Capture thread: creates compositor, captures frames, sends display name back.
#[allow(clippy::too_many_arguments)]
/// How a capture thread obtains its compositor: spawn a fresh detached one, or
/// attach to an already-running session's Wayland display (resume).
enum StartKind {
    Create { log_path: std::path::PathBuf },
    Attach { wayland_display: String, compositor_pid: u32 },
}

#[allow(clippy::too_many_arguments)]
fn capture_thread(
    width: u32,
    height: u32,
    quality: u8,
    mode: termland_compositor::SessionMode,
    desktop_shell: Option<String>,
    encoder_preset: Option<String>,
    encoder_crf: Option<u8>,
    encoder_extra_params: Option<String>,
    supported_codecs: Vec<termland_protocol::VideoCodec>,
    start_kind: StartKind,
    overlay_cursor: Arc<AtomicBool>,
    resize_rx: std::sync::mpsc::Receiver<(u32, u32)>,
    frame_tx: tokio::sync::mpsc::Sender<CapturedFrame>,
    display_tx: tokio::sync::oneshot::Sender<(String, u32)>,
    mut stop_rx: tokio::sync::oneshot::Receiver<()>,
) {
    let config = termland_compositor::CompositorConfig { width, height, mode, desktop_shell };
    let (mut compositor, comp_pid) = match start_kind {
        StartKind::Create { log_path } => {
            match termland_compositor::Compositor::create(config, &log_path) {
                Ok(x) => x,
                Err(e) => {
                    tracing::error!("Failed to create compositor: {e}");
                    return;
                }
            }
        }
        StartKind::Attach { wayland_display, compositor_pid } => {
            match termland_compositor::Compositor::attach(config, wayland_display) {
                Ok(c) => (c, compositor_pid),
                Err(e) => {
                    tracing::error!("Failed to attach to compositor: {e}");
                    return;
                }
            }
        }
    };

    // Send the Wayland display name + compositor pid back to the session task.
    let _ = display_tx.send((compositor.wayland_display().to_string(), comp_pid));

    // Map quality 1-100 to bitrate. quality 100 → 15 Mbps, 50 → 7.5 Mbps, 1 → 500 kbps
    let bitrate_kbps = (quality as u32 * 150).max(500);
    tracing::info!("Video quality={quality} → bitrate={bitrate_kbps}kbps");

    let tuning = EncoderTuning {
        bitrate_kbps,
        preset: encoder_preset,
        crf: encoder_crf,
        extra_svt_params: encoder_extra_params,
    };

    let mut video_encoder = init_encoder(&tuning, width, height, &supported_codecs);
    let mut encoder_codec = video_encoder.backend().codec();
    let mut encoder_dims = (width, height);

    let frame_duration = std::time::Duration::from_millis(33);

    loop {
        if stop_rx.try_recv().is_ok() {
            tracing::info!("Capture thread received stop signal");
            break;
        }

        if !crate::registry::pid_alive(comp_pid) {
            tracing::info!("Compositor process (pid {comp_pid}) exited, ending capture");
            break;
        }

        // Drain pending resize requests - only act on the most recent one.
        let mut latest_resize: Option<(u32, u32)> = None;
        while let Ok(sz) = resize_rx.try_recv() {
            latest_resize = Some(sz);
        }
        if let Some((rw, rh)) = latest_resize {
            if (rw, rh) != encoder_dims {
                match compositor.resize(rw, rh) {
                    Ok(()) => tracing::info!("Compositor output resized to {rw}x{rh}"),
                    Err(e) => tracing::warn!("Compositor resize failed: {e}"),
                }
                // Encoder reinit happens lazily when we detect new frame dims below.
            }
        }

        let start = std::time::Instant::now();

        match compositor.capture_frame(overlay_cursor.load(Ordering::Relaxed)) {
            Ok((w, h, rgba)) => {
                // Rebuild the encoder if the captured frame size changed.
                if (w, h) != encoder_dims {
                    tracing::info!("Capture dims changed {}x{} → {w}x{h}, reinitializing encoder",
                        encoder_dims.0, encoder_dims.1);
                    // Flush the old encoder before dropping it. This sends
                    // EOS so the encoder can properly close.
                    let _ = video_encoder.flush();
                    video_encoder = init_encoder(&tuning, w, h, &supported_codecs);
                    encoder_codec = video_encoder.backend().codec();
                    encoder_dims = (w, h);
                }

                let timestamp_us = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_micros() as u64;

                // Force a keyframe when dimensions change
                let force_keyframe = (w, h) != encoder_dims;

                let frames: Vec<CapturedFrame> = match video_encoder.encode_frame(&rgba, timestamp_us, force_keyframe) {
                    Ok(packets) if !packets.is_empty() => {
                        packets.into_iter().map(|p| {
                            let keyframe_str = if p.keyframe { "keyframe" } else { "frame" };
                            tracing::debug!("{keyframe_str}: {} bytes", p.data.len());
                            CapturedFrame::Video {
                                data: p.data,
                                keyframe: p.keyframe,
                                width: w as u16,
                                height: h as u16,
                                timestamp_us,
                                codec: encoder_codec,
                            }
                        }).collect()
                    }
                    Ok(_) => {
                        // Encoder buffering - send nothing this iteration
                        continue;
                    }
                    Err(e) => {
                        tracing::warn!("Video encode failed: {e}");
                        // Continue capturing - the encoder may recover
                        continue;
                    }
                };

                let mut send_failed = false;
                for frame in frames {
                    if frame_tx.blocking_send(frame).is_err() {
                        send_failed = true;
                        break;
                    }
                }
                if send_failed {
                    break;
                }
            }
            Err(e) => {
                if !crate::registry::pid_alive(comp_pid) {
                    tracing::info!("Compositor process (pid {comp_pid}) exited, ending capture");
                    break;
                }
                tracing::warn!("Frame capture failed: {e}");
            }
        }

        let elapsed = start.elapsed();
        if elapsed < frame_duration {
            std::thread::sleep(frame_duration - elapsed);
        }
    }

    tracing::info!("Capture thread exiting");
}

/// Input injection thread: receives input commands and injects them into cage.
fn input_thread(
    display_name: &str,
    _width: u32,
    _height: u32,
    rx: std::sync::mpsc::Receiver<InputCommand>,
) {
    // Give cage a moment to be fully ready for another client connection
    std::thread::sleep(std::time::Duration::from_millis(300));

    let mut injector = match termland_compositor::InputInjector::connect(display_name) {
        Ok(i) => i,
        Err(e) => {
            tracing::error!("Failed to create input injector: {e}");
            return;
        }
    };

    tracing::info!("Input injector ready");

    while let Ok(cmd) = rx.recv() {
        match cmd {
            InputCommand::Key { scancode, pressed } => {
                tracing::debug!("Input thread: injecting key scancode={scancode} pressed={pressed}");
                injector.key(scancode, pressed);
            }
            InputCommand::PointerMove { x, y, width, height } => {
                injector.pointer_motion_absolute(x, y, width, height);
            }
            InputCommand::PointerButton { button, pressed } => {
                injector.pointer_button(button, pressed);
            }
            InputCommand::Scroll { dx, dy } => {
                injector.pointer_scroll(dx, dy);
            }
            InputCommand::Stop => break,
        }
    }

    tracing::info!("Input thread exiting");
}

/// Audio capture thread: creates a PulseAudio null sink for the session,
/// captures from its monitor, encodes Opus, and sends AudioChunk packets.
fn audio_capture_thread(
    session_id: &str,
    audio_tx: tokio::sync::mpsc::Sender<AudioChunk>,
    mut stop_rx: tokio::sync::oneshot::Receiver<()>,
) {
    let sink_name = format!("termland_{}", session_id.replace('-', "_"));
    let monitor_source = format!("{sink_name}.monitor");

    // Load a null sink so apps in this session have somewhere to output audio.
    let load_result = std::process::Command::new("pactl")
        .args(["load-module", "module-null-sink",
               &format!("sink_name={sink_name}"),
               &format!("sink_properties=device.description=\"Termland\\ {session_id}\"")])
        .output();

    let module_id = match load_result {
        Ok(out) if out.status.success() => {
            let id = String::from_utf8_lossy(&out.stdout).trim().to_string();
            tracing::info!("Loaded PA null sink '{sink_name}' (module {id})");
            Some(id)
        }
        Ok(out) => {
            tracing::warn!("pactl load-module failed: {}", String::from_utf8_lossy(&out.stderr).trim());
            return;
        }
        Err(e) => {
            tracing::warn!("Could not run pactl: {e} — audio disabled");
            return;
        }
    };

    // Set this sink as the default so apps in the session use it.
    let _ = std::process::Command::new("pactl")
        .args(["set-default-sink", &sink_name])
        .output();

    // Open a PulseAudio recording stream from the monitor source.
    let spec = pulse::sample::Spec {
        format: pulse::sample::Format::S16le,
        rate: termland_codec::audio::SAMPLE_RATE,
        channels: termland_codec::audio::CHANNELS,
    };

    let pa = match psimple::Simple::new(
        None,
        "termland-server",
        pulse::stream::Direction::Record,
        Some(&monitor_source),
        "session audio capture",
        &spec,
        None,
        None,
    ) {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!("PA record stream failed for '{monitor_source}': {e}");
            cleanup_null_sink(module_id.as_deref());
            return;
        }
    };

    let mut opus_enc = match termland_codec::OpusEncoder::new() {
        Ok(e) => e,
        Err(e) => {
            tracing::error!("Opus encoder init: {e}");
            cleanup_null_sink(module_id.as_deref());
            return;
        }
    };

    tracing::info!("Audio capture started from '{monitor_source}'");

    let frame_samples = termland_codec::audio::FRAME_SIZE * termland_codec::audio::CHANNELS as usize;
    let mut byte_buf = vec![0u8; frame_samples * 2];

    loop {
        if stop_rx.try_recv().is_ok() {
            break;
        }

        if let Err(e) = pa.read(&mut byte_buf) {
            tracing::warn!("PA read error: {e}");
            break;
        }

        let pcm_buf: &[i16] = bytemuck::cast_slice(&byte_buf);
        let is_silence = pcm_buf.iter().all(|&s| s == 0);
        if is_silence {
            continue;
        }

        match opus_enc.encode(&pcm_buf) {
            Ok(opus_data) => {
                let chunk = AudioChunk {
                    timestamp_us: std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_micros() as u64,
                    sample_rate: termland_codec::audio::SAMPLE_RATE,
                    channels: termland_codec::audio::CHANNELS,
                    data: opus_data,
                };
                if audio_tx.blocking_send(chunk).is_err() {
                    break;
                }
            }
            Err(e) => tracing::warn!("Opus encode: {e}"),
        }
    }

    cleanup_null_sink(module_id.as_deref());
    tracing::info!("Audio capture thread exiting");
}

fn cleanup_null_sink(module_id: Option<&str>) {
    if let Some(id) = module_id {
        let _ = std::process::Command::new("pactl")
            .args(["unload-module", id])
            .output();
    }
}

/// Clipboard poll thread: periodically runs `wl-paste` on the session's
/// Wayland display and sends clipboard changes to the client.
fn clipboard_watch_thread(
    wayland_display: &str,
    clip_tx: tokio::sync::mpsc::Sender<ClipboardPayload>,
    mut stop_rx: tokio::sync::oneshot::Receiver<()>,
    client_set_hash: Arc<std::sync::atomic::AtomicU64>,
) {
    // Verify wl-paste is available
    if std::process::Command::new("wl-paste")
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_err()
    {
        tracing::warn!("wl-paste not available — clipboard sync disabled");
        return;
    }

    tracing::info!("Clipboard watch started on {wayland_display}");

    let mut last_hash: u64 = 0;

    loop {
        if stop_rx.try_recv().is_ok() {
            break;
        }

        std::thread::sleep(std::time::Duration::from_millis(500));

        // Run wl-paste to get current clipboard contents
        let output = match std::process::Command::new("wl-paste")
            .arg("--no-newline")
            .env("WAYLAND_DISPLAY", wayland_display)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .output()
        {
            Ok(o) if o.status.success() => o.stdout,
            _ => continue,
        };

        if output.is_empty() {
            continue;
        }

        let hash = {
            use std::hash::{Hash, Hasher};
            let mut h = std::collections::hash_map::DefaultHasher::new();
            output.hash(&mut h);
            h.finish()
        };

        if hash == last_hash {
            continue;
        }
        // Skip content that was set by the client (avoid echo loop)
        if hash == client_set_hash.load(std::sync::atomic::Ordering::Relaxed) {
            last_hash = hash;
            continue;
        }
        {
            last_hash = hash;
            tracing::debug!("Clipboard changed ({} bytes), sending to client", output.len());
            let payload = ClipboardPayload {
                mime_type: "text/plain".into(),
                data: output,
            };
            if clip_tx.blocking_send(payload).is_err() {
                break;
            }
        }
    }

    tracing::info!("Clipboard watch thread exiting");
}

/// Set the remote session's clipboard via wl-copy.
fn set_remote_clipboard(wayland_display: &str, data: &[u8], mime_type: &str) {
    use std::io::Write;

    let mime_arg = if mime_type.is_empty() { "text/plain" } else { mime_type };

    let mut child = match std::process::Command::new("wl-copy")
        .args(["--type", mime_arg])
        .env("WAYLAND_DISPLAY", wayland_display)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!("wl-copy failed: {e}");
            return;
        }
    };

    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(data);
    }
    let _ = child.wait();
}

/// Convert a captured frame (video) into a protocol message.
fn frame_to_message(frame: &CapturedFrame) -> Message {
    match frame {
        CapturedFrame::Video { data, keyframe, width, height, timestamp_us, codec } => {
            Message::VideoFrame(VideoFrame {
                timestamp_us: *timestamp_us,
                frame_type: if *keyframe { FrameType::Keyframe } else { FrameType::Inter },
                codec: Some(*codec),
                width: *width,
                height: *height,
                data: data.clone(),
            })
        }
    }
}