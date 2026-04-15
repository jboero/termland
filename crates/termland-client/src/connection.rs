use anyhow::{Context, Result};
use futures::{SinkExt, StreamExt};
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

pub async fn connect(
    server: &str, ssh: bool, mode: SessionMode, width: u32, height: u32, quality: u8,
    desktop_shell: Option<String>,
) -> Result<(mpsc::UnboundedReceiver<ServerEvent>, mpsc::UnboundedSender<ClientCommand>)> {
    let (server_tx, server_rx) = mpsc::unbounded_channel();
    let (client_tx, client_rx) = mpsc::unbounded_channel();

    if ssh {
        let child = Command::new("ssh")
            .args(["-s", server, "termland"])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::inherit())
            .spawn().context("failed to spawn ssh")?;
        let io = tokio::io::join(child.stdout.unwrap(), child.stdin.unwrap());
        tokio::spawn(async move {
            if let Err(e) = session_loop(io, mode, width, height, quality, desktop_shell, server_tx, client_rx).await {
                tracing::error!("Session error: {e}");
            }
        });
    } else {
        let stream = TcpStream::connect(server).await
            .context(format!("failed to connect to {server}"))?;
        tracing::info!("Connected to {server}");
        tokio::spawn(async move {
            if let Err(e) = session_loop(stream, mode, width, height, quality, desktop_shell, server_tx, client_rx).await {
                tracing::error!("Session error: {e}");
            }
        });
    }
    Ok((server_rx, client_tx))
}

/// AV1 packet for the decode thread.
struct DecodeJob {
    data: Vec<u8>,
}

async fn session_loop<T: AsyncRead + AsyncWrite + Unpin>(
    io: T, mode: SessionMode, width: u32, height: u32, quality: u8,
    desktop_shell: Option<String>,
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

    framed.send(Message::SessionCreate(SessionCreate {
        mode, width, height, audio: false, quality, desktop_shell,
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
                        // Send to decode thread (non-blocking)
                        let _ = decode_tx.send(DecodeJob { data: vf.data });
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
