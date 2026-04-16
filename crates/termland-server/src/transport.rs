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

/// Frame data from the capture thread (already AV1-encoded or raw fallback).
enum CapturedFrame {
    Av1 {
        data: Vec<u8>,
        keyframe: bool,
        width: u16,
        height: u16,
        timestamp_us: u64,
    },
    Raw {
        rgba: Vec<u8>,
        width: u32,
        height: u32,
        timestamp_us: u64,
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

    // Wait for SessionCreate
    let msg = framed
        .next()
        .await
        .context("connection closed before SessionCreate")?
        .context("failed to decode SessionCreate")?;

    let session_create = match msg {
        Message::SessionCreate(sc) => {
            tracing::info!(
                "Session request: {}x{} mode={:?} audio={} quality={} desktop_shell={:?}",
                sc.width,
                sc.height,
                sc.mode,
                sc.audio,
                sc.quality,
                sc.desktop_shell
            );
            sc
        }
        other => anyhow::bail!("expected SessionCreate, got {:?}", other.message_id()),
    };

    let width = session_create.width;
    let height = session_create.height;
    let quality = session_create.quality.clamp(1, 100);
    let desktop_shell = session_create.desktop_shell.filter(|s| !s.is_empty());
    let encoder_preset = session_create.encoder_preset.filter(|s| !s.is_empty());
    let encoder_crf = session_create.encoder_crf;
    let encoder_extra_params = session_create.encoder_extra_params.filter(|s| !s.is_empty());

    // SECURITY: validate client-supplied commands before they reach any shell.
    // desktop_shell and app command/args come from the untrusted client.
    if let Some(ref shell) = desktop_shell {
        termland_compositor::validate_shell_command(shell)
            .map_err(|e| anyhow::anyhow!("rejected desktop_shell: {e}"))?;
    }
    if let termland_protocol::SessionMode::App { ref command, ref args } = session_create.mode {
        termland_compositor::validate_shell_command(command)
            .map_err(|e| anyhow::anyhow!("rejected app command: {e}"))?;
        for arg in args {
            termland_compositor::validate_shell_command(arg)
                .map_err(|e| anyhow::anyhow!("rejected app arg: {e}"))?;
        }
    }
    if let Some(ref preset) = encoder_preset {
        termland_compositor::validate_shell_command(preset)
            .map_err(|e| anyhow::anyhow!("rejected encoder_preset: {e}"))?;
    }
    if let Some(ref extra) = encoder_extra_params {
        termland_compositor::validate_shell_command(extra)
            .map_err(|e| anyhow::anyhow!("rejected encoder_extra_params: {e}"))?;
    }

    let mode: termland_compositor::SessionMode = session_create.mode.into();

    // Spawn the compositor + capture loop on a blocking thread.
    let (frame_tx, mut frame_rx) = tokio::sync::mpsc::channel::<CapturedFrame>(2);
    let (display_tx, display_rx) = tokio::sync::oneshot::channel::<String>();
    let (stop_tx, stop_rx) = tokio::sync::oneshot::channel::<()>();
    // Resize requests from the session loop → capture thread.
    let (resize_tx, resize_rx) = std::sync::mpsc::channel::<(u32, u32)>();

    // Shared cursor-mode flag. Client defaults to server-side cursor rendering
    // (overlay_cursor=true). Can be toggled at runtime via CursorMode message.
    let overlay_cursor = Arc::new(AtomicBool::new(false));
    let overlay_cursor_capture = overlay_cursor.clone();

    let capture_handle = std::thread::spawn(move || {
        capture_thread(width, height, quality, mode, desktop_shell,
                       encoder_preset, encoder_crf, encoder_extra_params,
                       overlay_cursor_capture, resize_rx, frame_tx, display_tx, stop_rx);
    });

    // Wait for the compositor to report its Wayland display name
    let wayland_display = tokio::time::timeout(
        tokio::time::Duration::from_secs(10),
        async { display_rx.await },
    )
    .await
    .context("timeout waiting for compositor to start")?
    .context("capture thread died before compositor started")?;

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

    let (first_w, first_h) = match &first_frame {
        CapturedFrame::Av1 { width: w, height: h, .. } => (*w as u32, *h as u32),
        CapturedFrame::Raw { width: w, height: h, .. } => (*w, *h),
    };

    // Send SessionReady
    framed
        .send(Message::SessionReady(SessionReady {
            width: first_w,
            height: first_h,
            xkb_keymap: None,
        }))
        .await
        .context("failed to send SessionReady")?;

    // Log session info
    let encoder_name = match &first_frame {
        CapturedFrame::Av1 { .. } => "AV1 (see encoder log above)",
        CapturedFrame::Raw { .. } => "raw RGBA (no AV1 encoder available)",
    };
    tracing::info!("Session {session_id} active");
    tracing::info!("  Compositor: headless wlroots on {wayland_display}");
    tracing::info!("  Resolution: {first_w}x{first_h}");
    tracing::info!("  Video encoder: {encoder_name}");
    // Optionally start audio capture if the client requested it.
    let (mut audio_rx, _audio_stop_tx, _audio_handle) = if session_create.audio {
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
    tracing::info!("  Transport: TCP");

    // Send the first frame
    let first_msg = frame_to_message(&first_frame);
    framed.send(first_msg).await.context("failed to send first frame")?;

    let mut frame_num: u64 = 1;
    let mut current_width = first_w;
    let mut current_height = first_h;

    // Main event loop
    loop {
        tokio::select! {
            frame = frame_rx.recv() => {
                let Some(frame) = frame else {
                    // Capture thread ended - compositor (cage) exited
                    tracing::info!("Compositor exited, ending session");
                    let _ = framed.send(Message::SessionEnd(SessionEnd {
                        reason: "compositor exited".into(),
                    })).await;
                    break;
                };

                let (fw, fh) = match &frame {
                    CapturedFrame::Av1 { width: w, height: h, .. } => (*w as u32, *h as u32),
                    CapturedFrame::Raw { width: w, height: h, .. } => (*w, *h),
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
    let _ = capture_handle.join();
    let _ = input_handle.join();
    if let Some(ah) = _audio_handle {
        let _ = ah.join();
    }

    tracing::info!("Session {session_id} ended");
    Ok(())
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

/// Build a fresh AV1 encoder for the given dimensions + tuning.
/// Returns None if no encoder is available (we fall back to raw RGBA).
fn init_encoder(tuning: &EncoderTuning, width: u32, height: u32)
    -> Option<Box<dyn termland_codec::Av1Encoder>>
{
    let config = tuning.build_config(width, height);
    match termland_codec::probe_best_encoder(&config) {
        Ok(enc) => {
            tracing::info!("AV1 encoder ready for {width}x{height}: {}", enc.backend());
            Some(enc)
        }
        Err(e) => {
            tracing::warn!("No AV1 encoder available ({e}), using raw RGBA");
            None
        }
    }
}

/// Capture thread: creates compositor, captures frames, sends display name back.
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
    overlay_cursor: Arc<AtomicBool>,
    resize_rx: std::sync::mpsc::Receiver<(u32, u32)>,
    frame_tx: tokio::sync::mpsc::Sender<CapturedFrame>,
    display_tx: tokio::sync::oneshot::Sender<String>,
    mut stop_rx: tokio::sync::oneshot::Receiver<()>,
) {
    let compositor = match termland_compositor::Compositor::new(
        termland_compositor::CompositorConfig { width, height, mode, desktop_shell },
    ) {
        Ok(c) => c,
        Err(e) => {
            tracing::error!("Failed to create compositor: {e}");
            return;
        }
    };

    // Send the Wayland display name back so input thread can connect
    let _ = display_tx.send(compositor.wayland_display().to_string());

    // Map quality 1-100 to bitrate. quality 100 → 15 Mbps, 50 → 7.5 Mbps, 1 → 500 kbps
    let bitrate_kbps = (quality as u32 * 150).max(500);
    tracing::info!("Video quality={quality} → bitrate={bitrate_kbps}kbps");

    let tuning = EncoderTuning {
        bitrate_kbps,
        preset: encoder_preset,
        crf: encoder_crf,
        extra_svt_params: encoder_extra_params,
    };

    let mut av1_encoder = init_encoder(&tuning, width, height);
    let mut encoder_dims = (width, height);

    let mut compositor = compositor;
    let frame_duration = std::time::Duration::from_millis(33);

    loop {
        if stop_rx.try_recv().is_ok() {
            tracing::info!("Capture thread received stop signal");
            break;
        }

        if !compositor.is_alive() {
            tracing::info!("Compositor process exited, ending capture");
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
                    // EOS to libsvtav1 so SVT doesn't complain with
                    // "deinit called without sending EOS!" in its log.
                    // We discard the flushed packets because they belong
                    // to the old dimensions and would confuse the client.
                    if let Some(old) = av1_encoder.as_mut() {
                        let _ = old.flush();
                    }
                    av1_encoder = init_encoder(&tuning, w, h);
                    encoder_dims = (w, h);
                }

                let timestamp_us = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_micros() as u64;

                let frames: Vec<CapturedFrame> = if let Some(encoder) = &mut av1_encoder {
                    match encoder.encode_frame(&rgba, timestamp_us, false) {
                        Ok(packets) if !packets.is_empty() => {
                            packets.into_iter().map(|p| {
                                if p.keyframe {
                                    tracing::info!("AV1 keyframe: {} bytes", p.data.len());
                                }
                                CapturedFrame::Av1 {
                                    data: p.data,
                                    keyframe: p.keyframe,
                                    width: w as u16,
                                    height: h as u16,
                                    timestamp_us,
                                }
                            }).collect()
                        }
                        Ok(_) => {
                            // Encoder warming up - send raw so client isn't blank
                            vec![CapturedFrame::Raw { rgba, width: w, height: h, timestamp_us }]
                        }
                        Err(e) => {
                            tracing::warn!("AV1 encode failed: {e}, sending raw");
                            vec![CapturedFrame::Raw { rgba, width: w, height: h, timestamp_us }]
                        }
                    }
                } else {
                    vec![CapturedFrame::Raw { rgba, width: w, height: h, timestamp_us }]
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
                if !compositor.is_alive() {
                    tracing::info!("Compositor process exited, ending capture");
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

/// Convert a captured frame (AV1 or raw) into a protocol message.
fn frame_to_message(frame: &CapturedFrame) -> Message {
    match frame {
        CapturedFrame::Av1 { data, keyframe, width, height, timestamp_us } => {
            Message::VideoFrame(VideoFrame {
                timestamp_us: *timestamp_us,
                frame_type: if *keyframe { FrameType::Keyframe } else { FrameType::Inter },
                width: *width,
                height: *height,
                data: data.clone(),
            })
        }
        CapturedFrame::Raw { rgba, width, height, timestamp_us } => {
            Message::StillFrame(StillFrame {
                timestamp_us: *timestamp_us,
                x: 0,
                y: 0,
                width: *width,
                height: *height,
                lossless: true,
                data: rgba.clone(),
            })
        }
    }
}
