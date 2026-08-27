//! Live check that WebTransport sessions get Q2 video, not CBOR-on-control.
//!
//! Mirrors `quic_q2_planes.rs` for the HTTP/3 listener: a real compositor,
//! a real encoder, one real keyframe on a server-opened uni stream, parsed
//! with the same 18-byte header the Android client already reads. Without
//! this, a regression that passed `None` into `handle_session` would still
//! complete Hello/HelloAck and look fine.

use std::process::{Child, Command, Stdio};
use std::time::Duration;

use anyhow::Result;
use futures::{SinkExt, StreamExt};
use tokio_util::codec::Framed;
use wtransport::{ClientConfig, Endpoint};

use termland_protocol::{
    FrameType, Hello, Message, SessionCreate, SessionMode, TermlandCodec, VideoCodec,
    PROTOCOL_VERSION,
};

struct ChildGuard(Child);
impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

struct SessionGuard {
    bin: &'static str,
    session_id: Option<String>,
}

impl SessionGuard {
    fn new(bin: &'static str) -> Self {
        Self {
            bin,
            session_id: None,
        }
    }
    fn track(&mut self, session_id: &str) {
        if !session_id.is_empty() {
            self.session_id = Some(session_id.to_string());
        }
    }
}

impl Drop for SessionGuard {
    fn drop(&mut self) {
        let Some(id) = self.session_id.take() else {
            return;
        };
        match Command::new(self.bin)
            .args(["--close-session", &id])
            .output()
        {
            Ok(out) if out.status.success() => eprintln!("[test] closed session {id}"),
            Ok(out) => eprintln!(
                "[test] WARNING: failed to close session {id}: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            ),
            Err(e) => eprintln!("[test] WARNING: could not run --close-session for {id}: {e}"),
        }
    }
}

const VIDEO_HEADER_LEN: usize = 18;

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

#[tokio::test]
#[ignore = "spawns a real server and compositor; run with --ignored"]
async fn webtransport_q2_video_stream_carries_a_real_keyframe() -> Result<()> {
    let bin = env!("CARGO_BIN_EXE_termland-server");
    let child = Command::new(bin)
        .args([
            "--bind",
            "127.0.0.1",
            "--port",
            "28821",
            "--webtransport",
            "--webtransport-port",
            "28822",
            "--webtransport-origin",
            "https://app.example",
        ])
        .env("RUST_LOG", "info")
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to spawn termland-server");
    let _guard = ChildGuard(child);
    let mut session_guard = SessionGuard::new(bin);

    tokio::time::sleep(Duration::from_millis(1200)).await;

    let config = ClientConfig::builder()
        .with_bind_default()
        .with_no_cert_validation()
        .build();
    let endpoint = Endpoint::client(config).expect("client endpoint");
    let connection = endpoint
        .connect(
            wtransport::endpoint::ConnectOptions::builder("https://127.0.0.1:28822/termland")
                .add_header("origin", "https://app.example")
                .build(),
        )
        .await?;

    let (send, recv) = connection.open_bi().await?.await?;
    let mut framed = Framed::new(tokio::io::join(recv, send), TermlandCodec);

    let video_connection = connection.clone();
    let video_accept = tokio::spawn(async move {
        tokio::time::timeout(Duration::from_secs(20), video_connection.accept_uni()).await
    });

    framed
        .send(Message::Hello(Hello {
            protocol_version: PROTOCOL_VERSION,
            client_name: "webtransport-q2-test".into(),
        }))
        .await?;

    match tokio::time::timeout(Duration::from_secs(10), framed.next())
        .await
        .expect("timed out waiting for HelloAck")
        .expect("stream closed before HelloAck")?
    {
        Message::HelloAck(ack) => assert!(!ack.auth_required),
        other => panic!("expected HelloAck, got {:?}", other.message_id()),
    }

    framed
        .send(Message::SessionCreate(SessionCreate {
            mode: SessionMode::Desktop,
            width: 640,
            height: 360,
            audio: false,
            quality: 30,
            desktop_shell: None,
            encoder_preset: None,
            encoder_crf: None,
            encoder_extra_params: None,
            supported_codecs: VideoCodec::all_preferred(),
            supported_audio_codecs: vec![],
        }))
        .await?;

    let ready = match tokio::time::timeout(Duration::from_secs(30), framed.next())
        .await
        .expect("timed out waiting for SessionReady")
        .expect("stream closed before SessionReady")?
    {
        Message::SessionReady(sr) => sr,
        other => panic!("expected SessionReady, got {:?}", other.message_id()),
    };
    session_guard.track(&ready.session_id);

    let negotiated_codec = ready
        .codec
        .expect("server always announces a codec in SessionReady");

    let mut video = video_accept
        .await
        .expect("video-accept task panicked")
        .expect("timed out accepting WebTransport video stream")
        .expect("failed to accept WebTransport video stream");

    let mut header = [0u8; VIDEO_HEADER_LEN];
    tokio::time::timeout(Duration::from_secs(10), video.read_exact(&mut header))
        .await
        .expect("timed out reading video header")
        .expect("failed to read video header");

    let codec = decode_video_codec_tag(header[0])
        .unwrap_or_else(|| panic!("unrecognized codec tag {}", header[0]));
    let frame_type = match header[1] {
        0 => FrameType::Inter,
        1 => FrameType::Keyframe,
        other => panic!("unrecognized frame_type tag {other}"),
    };
    let width = u16::from_le_bytes([header[2], header[3]]);
    let height = u16::from_le_bytes([header[4], header[5]]);
    let data_len = u32::from_le_bytes(header[14..18].try_into().unwrap());

    let mut data = vec![0u8; data_len as usize];
    tokio::time::timeout(Duration::from_secs(10), video.read_exact(&mut data))
        .await
        .expect("timed out reading video frame data")
        .expect("failed to read video frame data");

    assert_eq!(codec, negotiated_codec);
    assert_eq!(width as u32, ready.width);
    assert_eq!(height as u32, ready.height);
    assert!(!data.is_empty());
    assert_eq!(data.len() as u32, data_len);
    assert_eq!(frame_type, FrameType::Keyframe);

    eprintln!(
        "[test] WebTransport Q2 video plane: PASS — real {codec:?} {width}x{height} keyframe, {} bytes",
        data.len()
    );
    Ok(())
}
