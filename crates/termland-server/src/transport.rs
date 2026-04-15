use anyhow::{Context, Result};
use futures::{SinkExt, StreamExt};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::TcpListener;
use tokio_util::codec::Framed;
use termland_protocol::*;

/// Run the server in SSH subsystem mode: protocol over stdin/stdout.
pub async fn run_subsystem() -> Result<()> {
    let stdin = tokio::io::stdin();
    let stdout = tokio::io::stdout();
    let io = tokio::io::join(stdin, stdout);
    handle_session(io).await
}

/// Run the server as a TCP listener.
pub async fn run_tcp_listener(bind: &str, port: u16) -> Result<()> {
    let listener = TcpListener::bind(format!("{bind}:{port}")).await?;
    tracing::info!("Listening on {bind}:{port}");

    loop {
        let (socket, addr) = listener.accept().await?;
        tracing::info!("Connection from {addr}");
        tokio::spawn(async move {
            if let Err(e) = handle_session(socket).await {
                tracing::error!("Session error for {addr}: {e}");
            }
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
async fn handle_session<T>(io: T) -> Result<()>
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
    let mode: termland_compositor::SessionMode = session_create.mode.into();

    // Spawn the compositor + capture loop on a blocking thread.
    let (frame_tx, mut frame_rx) = tokio::sync::mpsc::channel::<CapturedFrame>(2);
    let (display_tx, display_rx) = tokio::sync::oneshot::channel::<String>();
    let (stop_tx, stop_rx) = tokio::sync::oneshot::channel::<()>();

    // Shared cursor-mode flag. Client defaults to server-side cursor rendering
    // (overlay_cursor=true). Can be toggled at runtime via CursorMode message.
    let overlay_cursor = Arc::new(AtomicBool::new(true));
    let overlay_cursor_capture = overlay_cursor.clone();

    let capture_handle = std::thread::spawn(move || {
        capture_thread(width, height, quality, mode, desktop_shell,
                       overlay_cursor_capture, frame_tx, display_tx, stop_rx);
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
    tracing::info!("  Audio encoder: none (Phase 5)");
    tracing::info!("  Transport: TCP (unencrypted - use --ssh for encryption)");

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
                            Message::SessionResize(_sr) => {}
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
    let _ = capture_handle.join();
    let _ = input_handle.join();

    tracing::info!("Session {session_id} ended");
    Ok(())
}

/// Capture thread: creates compositor, captures frames, sends display name back.
fn capture_thread(
    width: u32,
    height: u32,
    quality: u8,
    mode: termland_compositor::SessionMode,
    desktop_shell: Option<String>,
    overlay_cursor: Arc<AtomicBool>,
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

    // Map quality 1-100 to bitrate and CRF.
    // quality 100 = 15000 kbps, quality 75 = 8000 kbps, quality 50 = 4000, quality 25 = 1500, quality 1 = 500
    let bitrate_kbps = (quality as u32 * 150).max(500);
    let crf = 51 - (quality as u32 * 40 / 100); // quality 100 → CRF 11, quality 50 → CRF 31, quality 1 → CRF 51
    tracing::info!("Video quality={quality} → bitrate={bitrate_kbps}kbps crf={crf}");

    let encoder_config = termland_codec::EncoderConfig {
        width,
        height,
        fps: 30,
        bitrate_kbps,
        keyframe_interval: 30,
    };

    let mut av1_encoder: Option<Box<dyn termland_codec::Av1Encoder>> =
        match termland_codec::probe_best_encoder(&encoder_config) {
            Ok(enc) => {
                tracing::info!("AV1 encoder ready: {}", enc.backend());
                Some(enc)
            }
            Err(e) => {
                tracing::warn!("No AV1 encoder available ({e}), using raw RGBA");
                None
            }
        };

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

        let start = std::time::Instant::now();

        match compositor.capture_frame(overlay_cursor.load(Ordering::Relaxed)) {
            Ok((w, h, rgba)) => {
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
