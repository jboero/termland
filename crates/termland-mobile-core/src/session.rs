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

/// Q2 video/audio header layout — must match
/// `crates/termland-server/src/quic.rs`'s `VIDEO_HEADER_LEN`/
/// `AUDIO_HEADER_LEN` and their `*_header_bytes` encoders byte-for-byte.
/// `[codec: u8][frame_type: u8][width: u16][height: u16][timestamp_us: u64][data_len: u32]`,
/// little-endian = 1+1+2+2+8+4 = 18 bytes.
const VIDEO_HEADER_LEN: usize = 18;
/// `[sample_rate: u32][channels: u8]`, little-endian = 4+1 = 5 bytes.
const AUDIO_HEADER_LEN: usize = 5;

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
///
/// Returns the QUIC `Connection` handle alongside the framed stream when the
/// transport is `Transport::Quic` (`None` otherwise) — callers that only do
/// one-shot control ops (`list_sessions`, `close_session`) ignore it; only
/// `open_session`/`run_session` (the actual streaming path) need it, to
/// later accept Q2's video uni stream and read audio datagrams.
pub(crate) async fn connect_and_handshake(
    profile: &ServerProfile,
) -> Result<(Conn, Option<quinn::Connection>)> {
    let connected = Transport::for_profile(profile)
        .connect(&profile.host, profile.port)
        .await?;
    let quic_connection = connected.quic_connection;
    let mut framed = Framed::new(connected.io, TermlandCodec);

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

    Ok((framed, quic_connection))
}

pub(crate) async fn list_sessions(profile: &ServerProfile) -> Result<Vec<SessionSummary>> {
    let (mut framed, _quic_connection) = connect_and_handshake(profile).await?;
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
    let (mut framed, _quic_connection) = connect_and_handshake(profile).await?;
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
) -> Result<(Conn, Option<quinn::Connection>, SessionReadyInfo)> {
    let (mut framed, quic_connection) = connect_and_handshake(profile).await?;
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
            Ok((framed, quic_connection, info))
        }
        other => Err(unexpected("SessionReady", &other)),
    }
}

/// Pump the session: route server messages to the observer, forward queued
/// input. Returns when the server ends the stream or the client detaches
/// (`input_rx` closed by `disconnect()`).
///
/// `quic_connection` is `Some` only for `Transport::Quic` — see
/// `Connected::quic_connection`'s doc comment for why it isn't accepted/read
/// from until here rather than back in `connect_and_handshake`. When `Some`,
/// this spawns two background tasks (`quic_video_reader`, `quic_audio_reader`)
/// that own the video uni stream / datagram reads respectively and forward
/// parsed packets over channels this loop's `select!` drains below - NOT
/// inline `select!` branches directly on `accept_uni()`/`read_datagram()`,
/// because a `select!` branch that loses the race gets its future dropped
/// mid-poll, which would lose whatever partial header/frame bytes a video
/// read was midway through and desync the stream framing. An mpsc channel's
/// `recv()` has no such problem — dropping it never discards an item that
/// was already sent.
pub(crate) async fn run_session(
    mut framed: Conn,
    quic_connection: Option<quinn::Connection>,
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

    // Q2 planes. These channels are created unconditionally but only ever
    // fed when `quic_connection` is `Some` below; for every other transport
    // their `Sender` halves just sit unused for the life of this function,
    // so `video_rx.recv()`/`audio_rx.recv()` below pend forever without ever
    // firing - the same "no-op branch" pattern the server side uses for its
    // optional audio channel in `transport.rs`'s `run_session`.
    let (video_tx, mut video_rx) = mpsc::channel::<VideoPacket>(4);
    let (audio_tx, mut audio_rx) = mpsc::channel::<Vec<u8>>(8);
    if let Some(connection) = quic_connection {
        let video_connection = connection.clone();
        tokio::spawn(quic_video_reader(video_connection, video_tx));
        tokio::spawn(quic_audio_reader(connection, audio_tx));
    }

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

            // Q2 video plane (QUIC only - see this function's doc comment).
            // No-op branch (never fires) on every other transport.
            pkt = video_rx.recv() => {
                if let Some(pkt) = pkt {
                    bytes_since_report += pkt.data.len() as u64;
                    observer.on_video_packet(pkt);
                }
            }

            // Q2 audio plane (QUIC only). No-op branch on every other transport.
            chunk = audio_rx.recv() => {
                if let Some(data) = chunk {
                    bytes_since_report += data.len() as u64;
                    observer.on_audio_packet(data);
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

/// Q2's video plane reader. First `accept_uni()`s the server-opened video
/// stream (deferred to here rather than `Transport::connect` - see
/// `Connected::quic_connection`'s doc comment), then reads one frame after
/// another: an 18-byte fixed header, then exactly `data_len` bytes of
/// encoded frame data - see `crates/termland-server/src/quic.rs`'s module
/// doc comment for the full header layout and why it's fixed-binary rather
/// than the CBOR `Message` envelope.
///
/// Ends the task (dropping `tx`, which `run_session`'s `video_rx.recv()`
/// then reads as "no more packets", a silent no-op) on any read/parse error
/// or clean stream close. A broken video plane must not bring down the
/// control-plane connection - that loss of head-of-line independence is
/// exactly what Q2 exists to avoid.
async fn quic_video_reader(connection: quinn::Connection, tx: mpsc::Sender<VideoPacket>) {
    let mut video = match connection.accept_uni().await {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!("QUIC video stream never opened: {e}");
            return;
        }
    };

    // Defensive cap on `data_len`: a corrupt/malicious header must not turn
    // into an unbounded allocation. No real encoded frame at any sane
    // resolution approaches this; it exists purely as a backstop.
    const MAX_FRAME_BYTES: u32 = 64 * 1024 * 1024;

    loop {
        let mut header = [0u8; VIDEO_HEADER_LEN];
        if let Err(e) = video.read_exact(&mut header).await {
            tracing::info!("QUIC video stream ended: {e}");
            return;
        }

        let Some((codec, keyframe, width, height, timestamp_us, data_len)) = parse_video_header(&header) else {
            tracing::warn!("QUIC video stream: unknown codec tag {}, ending read loop", header[0]);
            return;
        };

        if data_len > MAX_FRAME_BYTES {
            tracing::warn!("QUIC video stream: implausible frame size {data_len} bytes, ending read loop");
            return;
        }

        let mut data = vec![0u8; data_len as usize];
        if let Err(e) = video.read_exact(&mut data).await {
            tracing::info!("QUIC video stream ended mid-frame: {e}");
            return;
        }

        let packet = VideoPacket {
            data,
            timestamp_us,
            keyframe,
            codec: MobileCodec::from(codec),
            width: width as u32,
            height: height as u32,
        };
        if tx.send(packet).await.is_err() {
            return; // run_session exited; nothing left to feed.
        }
    }
}

/// Parse the 18-byte Q2 video header into its fields (codec, keyframe,
/// width, height, timestamp_us, data_len). `None` for an unrecognized codec
/// tag - see `decode_video_codec_tag`. Pulled out of `quic_video_reader` as
/// a pure function so the byte layout can be unit-tested without a real
/// QUIC connection.
fn parse_video_header(header: &[u8; VIDEO_HEADER_LEN]) -> Option<(VideoCodec, bool, u16, u16, u64, u32)> {
    let codec = decode_video_codec_tag(header[0])?;
    let keyframe = header[1] == 1;
    let width = u16::from_le_bytes([header[2], header[3]]);
    let height = u16::from_le_bytes([header[4], header[5]]);
    let timestamp_us = u64::from_le_bytes(header[6..14].try_into().unwrap_or_default());
    let data_len = u32::from_le_bytes(header[14..18].try_into().unwrap_or_default());
    Some((codec, keyframe, width, height, timestamp_us, data_len))
}

/// Matches `crates/termland-server/src/quic.rs`'s `codec_tag` exactly (and,
/// transitively, `termland_protocol::VideoCodec::all_preferred()`'s
/// declared preference order).
fn decode_video_codec_tag(tag: u8) -> Option<VideoCodec> {
    match tag {
        0 => Some(VideoCodec::Av1),
        1 => Some(VideoCodec::Vp9),
        2 => Some(VideoCodec::Vp8),
        3 => Some(VideoCodec::H265),
        4 => Some(VideoCodec::H264),
        _ => None,
    }
}

/// Q2's audio plane reader. `connection.read_datagram()` yields one complete
/// QUIC datagram per call - datagrams are already message-delimited (never
/// partial), so unlike the video stream there is no length-prefixed loop
/// here beyond stripping the fixed 5-byte header. The Opus payload is
/// forwarded as-is: this core never decodes audio (see the module doc
/// comment), so `sample_rate`/`channels` in the header are read but not
/// currently needed downstream - the platform decoder is configured once,
/// up front, from the session's fixed Opus format.
async fn quic_audio_reader(connection: quinn::Connection, tx: mpsc::Sender<Vec<u8>>) {
    loop {
        let datagram = match connection.read_datagram().await {
            Ok(d) => d,
            Err(e) => {
                tracing::info!("QUIC audio datagram stream ended: {e}");
                return;
            }
        };
        let Some((_sample_rate, _channels, payload)) = parse_audio_datagram(&datagram) else {
            tracing::warn!("QUIC audio datagram too short ({} bytes), dropping", datagram.len());
            continue;
        };
        if tx.send(payload.to_vec()).await.is_err() {
            return; // run_session exited; nothing left to feed.
        }
    }
}

/// Split one Q2 audio datagram into its 5-byte header fields
/// (`sample_rate`, `channels`) and the remaining Opus payload. `None` if the
/// datagram is shorter than the fixed header - malformed/truncated, not a
/// framing case this wire format can otherwise produce (QUIC datagrams are
/// never partial). Pulled out of `quic_audio_reader` as a pure function so
/// the byte layout can be unit-tested without a real QUIC connection.
fn parse_audio_datagram(datagram: &[u8]) -> Option<(u32, u8, &[u8])> {
    if datagram.len() < AUDIO_HEADER_LEN {
        return None;
    }
    let sample_rate = u32::from_le_bytes(datagram[0..4].try_into().unwrap_or_default());
    let channels = datagram[4];
    Some((sample_rate, channels, &datagram[AUDIO_HEADER_LEN..]))
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

#[cfg(test)]
mod quic_framing_tests {
    use super::*;

    /// Builds a Q2 video header byte-for-byte the way
    /// `crates/termland-server/src/quic.rs`'s `video_header_bytes` does, so
    /// this test exercises the client's decoder against an independently
    /// constructed fixture rather than its own encoder (there is no
    /// encoder on the client - it never originates video).
    fn build_video_header(codec_tag: u8, frame_type_tag: u8, width: u16, height: u16, timestamp_us: u64, data_len: u32) -> [u8; VIDEO_HEADER_LEN] {
        let mut buf = [0u8; VIDEO_HEADER_LEN];
        buf[0] = codec_tag;
        buf[1] = frame_type_tag;
        buf[2..4].copy_from_slice(&width.to_le_bytes());
        buf[4..6].copy_from_slice(&height.to_le_bytes());
        buf[6..14].copy_from_slice(&timestamp_us.to_le_bytes());
        buf[14..18].copy_from_slice(&data_len.to_le_bytes());
        buf
    }

    #[test]
    fn video_header_decodes_all_codec_and_frame_type_tags() {
        let cases = [
            (0u8, VideoCodec::Av1),
            (1, VideoCodec::Vp9),
            (2, VideoCodec::Vp8),
            (3, VideoCodec::H265),
            (4, VideoCodec::H264),
        ];
        for (tag, expected_codec) in cases {
            for (ft_tag, expected_keyframe) in [(0u8, false), (1u8, true)] {
                let header = build_video_header(tag, ft_tag, 1920, 1080, 123_456_789_012, 65536);
                let (codec, keyframe, width, height, timestamp_us, data_len) =
                    parse_video_header(&header).unwrap_or_else(|| panic!("expected header to decode for codec tag {tag}"));
                assert_eq!(codec, expected_codec);
                assert_eq!(keyframe, expected_keyframe);
                assert_eq!(width, 1920);
                assert_eq!(height, 1080);
                assert_eq!(timestamp_us, 123_456_789_012);
                assert_eq!(data_len, 65536);
            }
        }
    }

    #[test]
    fn video_header_rejects_unknown_codec_tag() {
        let header = build_video_header(255, 0, 640, 480, 0, 0);
        assert!(parse_video_header(&header).is_none());
    }

    #[test]
    fn video_header_field_offsets_match_server_layout() {
        // Same exact-byte-offset pin as the server's
        // `video_header_byte_offsets_are_exact` test - both sides must agree
        // on where every field lives, not just round-trip through each
        // side's own (mirror-image) implementation.
        let header = build_video_header(4, 1, 0x0102, 0x0304, 0x0102030405060708, 0xAABBCCDD);
        assert_eq!(header[0], 4);
        assert_eq!(header[1], 1);
        assert_eq!(&header[2..4], &0x0102u16.to_le_bytes());
        assert_eq!(&header[4..6], &0x0304u16.to_le_bytes());
        assert_eq!(&header[6..14], &0x0102030405060708u64.to_le_bytes());
        assert_eq!(&header[14..18], &0xAABBCCDDu32.to_le_bytes());

        let (codec, keyframe, width, height, timestamp_us, data_len) = parse_video_header(&header).unwrap();
        assert_eq!(codec, VideoCodec::H264);
        assert!(keyframe);
        assert_eq!(width, 0x0102);
        assert_eq!(height, 0x0304);
        assert_eq!(timestamp_us, 0x0102030405060708);
        assert_eq!(data_len, 0xAABBCCDD);
    }

    #[test]
    fn audio_datagram_round_trips_header_and_payload() {
        let mut datagram = Vec::new();
        datagram.extend_from_slice(&48000u32.to_le_bytes());
        datagram.push(2u8);
        datagram.extend_from_slice(b"opus-payload-bytes");

        let (sample_rate, channels, payload) =
            parse_audio_datagram(&datagram).expect("datagram at least AUDIO_HEADER_LEN bytes");
        assert_eq!(sample_rate, 48000);
        assert_eq!(channels, 2);
        assert_eq!(payload, b"opus-payload-bytes");
    }

    #[test]
    fn audio_datagram_shorter_than_header_is_rejected() {
        let short = vec![0u8; AUDIO_HEADER_LEN - 1];
        assert!(parse_audio_datagram(&short).is_none());
    }

    #[test]
    fn audio_datagram_with_empty_payload_is_valid() {
        // A degenerate but well-formed datagram: header only, no Opus bytes.
        // Not something the server would ever send, but the parser must not
        // panic on it.
        let mut datagram = Vec::new();
        datagram.extend_from_slice(&16000u32.to_le_bytes());
        datagram.push(1u8);
        let (sample_rate, channels, payload) = parse_audio_datagram(&datagram).unwrap();
        assert_eq!(sample_rate, 16000);
        assert_eq!(channels, 1);
        assert!(payload.is_empty());
    }
}
