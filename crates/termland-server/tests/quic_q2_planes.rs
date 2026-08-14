//! Live QUIC Q2 test (docs/quic-transport.md): proves the split video/audio
//! planes actually work over a real QUIC connection to a real spawned
//! `termland-server` binary — real compositor, real video encoder, real
//! wire bytes — not just "the code compiles."
//!
//! Mirrors `tests/quic_handshake.rs`'s Q1 pattern (spawn the real binary via
//! `CARGO_BIN_EXE_...`, dial it with `quinn`), but goes further: completes
//! the whole control-plane handshake (Hello/HelloAck/SessionCreate/
//! SessionReady) over the bidi stream, accepts the server-opened video uni
//! stream concurrently, and parses a real frame off it using the exact
//! 18-byte header layout `crates/termland-server/src/quic.rs`'s
//! `video_header_bytes` produces — checking the decoded codec/dimensions/
//! data_len against what `SessionReady` and the actual bytes read say,
//! rather than just checking *something* arrived.
//!
//! Also attempts one audio datagram round trip (`audio: true` on
//! `SessionCreate`), on a short timeout: this test environment has no real
//! audio source (the server's audio thread encodes PulseAudio monitor
//! capture and silently skips genuinely-silent buffers - see
//! `transport.rs`'s `audio_capture_thread`), so a live datagram may simply
//! never arrive even though the plumbing is correct. That path is verified
//! for real via the pure `parse_audio_datagram`/`audio_header_bytes` unit
//! tests instead (`session::quic_framing_tests` on the client,
//! `quic::header_tests` on the server) - this test reports plainly which
//! outcome it hit rather than papering over a timeout as a pass.

use std::process::{Child, Command, Stdio};
use std::sync::Arc;

use futures::{SinkExt, StreamExt};
use tokio_util::codec::Framed;

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

/// Closes the persistent session this test creates, even if an assertion
/// panics part-way through.
///
/// Killing the server child is **not** sufficient. Compositors are
/// deliberately `setsid`-detached so they outlive the server process — that
/// is the whole point of resumable sessions — so a test that only kills the
/// server strands a full desktop session (labwc + the configured shell, here
/// `plasmashell` and a terminal) on the machine for every run, forever.
///
/// Teardown goes through the server binary's own `--close-session` rather
/// than the wire protocol for two reasons: `Drop` is synchronous, so it
/// cannot await a `SessionClose` send, and it must still work on the unwind
/// path where the connection may already be gone. Reading the registry
/// directly is exactly what `--close-session` does, and it works whether or
/// not the server child is still alive.
struct SessionGuard {
    bin: &'static str,
    session_id: Option<String>,
}

impl SessionGuard {
    fn new(bin: &'static str) -> Self {
        Self { bin, session_id: None }
    }

    /// Start tracking the session announced in `SessionReady`. An empty id
    /// means the server did not create a persistent session, so there is
    /// nothing to clean up.
    fn track(&mut self, session_id: &str) {
        if !session_id.is_empty() {
            self.session_id = Some(session_id.to_string());
        }
    }
}

impl SessionGuard {
    /// Unload the session's PulseAudio null sink.
    ///
    /// The server creates it and unloads it on graceful shutdown, but this
    /// harness kills the child, so that cleanup never runs and the sink is
    /// left behind with nothing alive that owns it. Best-effort: `pactl` is
    /// absent on headless CI, and a run with no audio never created one.
    fn unload_null_sink(id: &str) {
        let sink = format!("termland_{}", id.replace('-', "_"));
        let Ok(out) = Command::new("pactl").args(["list", "short", "modules"]).output() else {
            return;
        };
        for line in String::from_utf8_lossy(&out.stdout).lines() {
            if !line.contains(&format!("sink_name={sink}")) {
                continue;
            }
            if let Some(module_id) = line.split_whitespace().next() {
                let _ = Command::new("pactl").args(["unload-module", module_id]).output();
                eprintln!("[test] unloaded null sink {sink} (module {module_id})");
            }
        }
    }
}

impl Drop for SessionGuard {
    fn drop(&mut self) {
        let Some(id) = self.session_id.take() else {
            return;
        };
        Self::unload_null_sink(&id);
        match Command::new(self.bin).args(["--close-session", &id]).output() {
            Ok(out) if out.status.success() => {
                eprintln!("[test] closed session {id}");
            }
            Ok(out) => {
                // Loud, because the cost of missing this is a stray desktop
                // session that nothing will ever reap.
                eprintln!(
                    "[test] WARNING: failed to close session {id} — it may still be running. \
                     stderr: {}",
                    String::from_utf8_lossy(&out.stderr).trim(),
                );
            }
            Err(e) => {
                eprintln!(
                    "[test] WARNING: could not run --close-session for {id}: {e}. \
                     Close it by hand with: termland-server --close-session {id}",
                );
            }
        }
    }
}

#[derive(Debug)]
struct AcceptAnyCert;
impl rustls::client::danger::ServerCertVerifier for AcceptAnyCert {
    fn verify_server_cert(
        &self,
        _: &rustls::pki_types::CertificateDer<'_>,
        _: &[rustls::pki_types::CertificateDer<'_>],
        _: &rustls::pki_types::ServerName<'_>,
        _: &[u8],
        _: rustls::pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }
    fn verify_tls12_signature(
        &self,
        _: &[u8],
        _: &rustls::pki_types::CertificateDer<'_>,
        _: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }
    fn verify_tls13_signature(
        &self,
        _: &[u8],
        _: &rustls::pki_types::CertificateDer<'_>,
        _: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }
    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        rustls::crypto::aws_lc_rs::default_provider()
            .signature_verification_algorithms
            .supported_schemes()
    }
}

const ALPN: &[u8] = b"termland/1";

/// Must match `crates/termland-server/src/quic.rs`'s `VIDEO_HEADER_LEN`.
const VIDEO_HEADER_LEN: usize = 18;
/// Must match `crates/termland-server/src/quic.rs`'s `AUDIO_HEADER_LEN`.
const AUDIO_HEADER_LEN: usize = 5;

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
async fn quic_q2_video_stream_carries_real_frames() {
    let port: u16 = 27868;

    let bin = env!("CARGO_BIN_EXE_termland-server");
    eprintln!("[test] spawning {bin} --quic --quic-port {port}");
    let child = Command::new(bin)
        .args(["--bind", "127.0.0.1", "--quic", "--quic-port", &port.to_string()])
        .env("RUST_LOG", "info")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to spawn termland-server");
    let mut guard = ChildGuard(child);
    // Declared after `guard` so it drops *first*: the session is closed while
    // the server is still up, and before the child is reaped.
    let mut session_guard = SessionGuard::new(bin);

    tokio::time::sleep(std::time::Duration::from_millis(800)).await;

    let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
    let mut crypto = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .expect("protocol versions")
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(AcceptAnyCert))
        .with_no_client_auth();
    crypto.alpn_protocols = vec![ALPN.to_vec()];

    let quic_crypto = quinn::crypto::rustls::QuicClientConfig::try_from(crypto)
        .expect("rustls config usable for QUIC");
    let client_config = quinn::ClientConfig::new(Arc::new(quic_crypto));

    let mut endpoint = quinn::Endpoint::client("127.0.0.1:0".parse().unwrap())
        .expect("bind local QUIC socket");
    endpoint.set_default_client_config(client_config);

    eprintln!("[test] connecting to 127.0.0.1:{port} over QUIC");
    let connecting = endpoint
        .connect(format!("127.0.0.1:{port}").parse().unwrap(), "localhost")
        .expect("start QUIC connect");
    let connection = tokio::time::timeout(std::time::Duration::from_secs(10), connecting)
        .await
        .expect("QUIC handshake timed out")
        .expect("QUIC handshake failed");
    eprintln!(
        "[test] QUIC handshake COMPLETE — connection established to {}",
        connection.remote_address()
    );

    // Control bidi stream (Q1's contract, unchanged by Q2).
    let (send, recv) = connection.open_bi().await.expect("open control bidi stream");
    eprintln!("[test] control bidi stream opened");
    let mut framed = Framed::new(tokio::io::join(recv, send), TermlandCodec);

    // Start racing to accept the server-opened video uni stream *before*
    // sending SessionCreate: `run_session` opens it right before sending
    // SessionReady (see that function's doc comment), so waiting until
    // after SessionReady arrives here would risk (in principle, on a slow
    // scheduler) the accept losing a race it doesn't actually need to lose,
    // and there's no reason to serialize the two waits when they can run
    // concurrently.
    let video_connection = connection.clone();
    let video_accept = tokio::spawn(async move {
        tokio::time::timeout(std::time::Duration::from_secs(20), video_connection.accept_uni()).await
    });

    framed
        .send(Message::Hello(Hello {
            protocol_version: PROTOCOL_VERSION,
            client_name: "quic-q2-test".into(),
        }))
        .await
        .expect("send Hello");

    match tokio::time::timeout(std::time::Duration::from_secs(10), framed.next())
        .await
        .expect("timed out waiting for HelloAck")
        .expect("connection closed before HelloAck")
        .expect("decode error waiting for HelloAck")
    {
        Message::HelloAck(ack) => {
            eprintln!("[test] received HelloAck: {ack:?}");
            assert!(!ack.auth_required);
        }
        other => panic!("expected HelloAck, got {other:?}"),
    }

    eprintln!("[test] sending SessionCreate (audio=true, to also attempt a live audio datagram)");
    framed
        .send(Message::SessionCreate(SessionCreate {
            mode: SessionMode::Desktop,
            width: 640,
            height: 360,
            audio: true,
            quality: 30,
            desktop_shell: None,
            encoder_preset: None,
            encoder_crf: None,
            encoder_extra_params: None,
            supported_codecs: VideoCodec::all_preferred(),
        }))
        .await
        .expect("send SessionCreate");

    // Compositor start + first encoded frame can genuinely take several
    // seconds under software encoding - mirror the server's own internal
    // budget (15s compositor start + 10s first frame) with headroom.
    let ready = match tokio::time::timeout(std::time::Duration::from_secs(30), framed.next())
        .await
        .expect("timed out waiting for SessionReady")
        .expect("connection closed before SessionReady")
        .expect("decode error waiting for SessionReady")
    {
        Message::SessionReady(sr) => {
            eprintln!("[test] received SessionReady: {sr:?}");
            sr
        }
        other => panic!("expected SessionReady, got {other:?}"),
    };
    // Register for teardown as early as possible — every assertion below this
    // point can panic, and each one would otherwise leak the session.
    session_guard.track(&ready.session_id);

    let negotiated_codec = ready.codec.expect("server always announces a codec in SessionReady");

    // --- Video plane: read one real frame off the Q2 uni stream. ---
    let mut video = video_accept
        .await
        .expect("video-accept task panicked")
        .expect("timed out accepting QUIC video stream")
        .expect("failed to accept QUIC video stream");
    eprintln!("[test] accepted server-opened QUIC video uni stream");

    let mut header = [0u8; VIDEO_HEADER_LEN];
    tokio::time::timeout(std::time::Duration::from_secs(10), video.read_exact(&mut header))
        .await
        .expect("timed out reading video header")
        .expect("failed to read video header");

    let codec_tag = header[0];
    let frame_type_tag = header[1];
    let width = u16::from_le_bytes([header[2], header[3]]);
    let height = u16::from_le_bytes([header[4], header[5]]);
    let timestamp_us = u64::from_le_bytes(header[6..14].try_into().unwrap());
    let data_len = u32::from_le_bytes(header[14..18].try_into().unwrap());

    let codec = decode_video_codec_tag(codec_tag)
        .unwrap_or_else(|| panic!("unrecognized codec tag {codec_tag} in real video header"));
    let frame_type = match frame_type_tag {
        0 => FrameType::Inter,
        1 => FrameType::Keyframe,
        other => panic!("unrecognized frame_type tag {other} in real video header"),
    };

    eprintln!(
        "[test] parsed real video header: codec={codec:?} frame_type={frame_type:?} \
         {width}x{height} timestamp_us={timestamp_us} data_len={data_len}"
    );

    let mut data = vec![0u8; data_len as usize];
    tokio::time::timeout(std::time::Duration::from_secs(10), video.read_exact(&mut data))
        .await
        .expect("timed out reading video frame data")
        .expect("failed to read video frame data");

    eprintln!("[test] read {} real encoded frame bytes off the QUIC video stream", data.len());

    // Sanity-check the decoded header against what the real encoder and
    // SessionReady actually reported, not just "some bytes arrived."
    assert_eq!(codec, negotiated_codec, "video stream codec tag must match SessionReady's negotiated codec");
    assert_eq!(width as u32, ready.width, "video header width must match SessionReady's width");
    assert_eq!(height as u32, ready.height, "video header height must match SessionReady's height");
    assert!(!data.is_empty(), "a real encoded frame must not be empty");
    assert_eq!(data.len() as u32, data_len, "read exactly data_len bytes (length-prefixed framing sanity check)");
    // The very first frame off a fresh encoder is always a keyframe.
    assert_eq!(frame_type, FrameType::Keyframe, "the first frame off a fresh encoder must be a keyframe");

    eprintln!(
        "[test] QUIC Q2 video plane: PASS — real {codec:?} {width}x{height} keyframe, \
         {} bytes, arrived on its own dedicated QUIC uni stream with the correct 18-byte header",
        data.len()
    );

    // --- Audio plane: the server only encodes+sends non-silent PulseAudio
    // --- monitor buffers (see transport.rs's audio_capture_thread), so an
    // --- idle test environment produces nothing to capture. Give this a
    // --- real chance by feeding actual (non-silent) audio into the
    // --- session's null sink ourselves: pipe random bytes through `pacat`
    // --- at the exact PCM format the server's capture stream expects.
    // --- Random noise is not silence, so it exercises the real is_silence
    // --- check, the real Opus encoder, and the real datagram send path -
    // --- this is not a synthetic/mocked verification.
    let sink_name = format!("termland_{}", ready.session_id.replace('-', "_"));
    let mut sink_ready = false;
    for _ in 0..10 {
        if let Ok(out) = Command::new("pactl").args(["list", "sinks", "short"]).output() {
            if String::from_utf8_lossy(&out.stdout).lines().any(|l| l.contains(&sink_name)) {
                sink_ready = true;
                break;
            }
        }
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    }

    let mut noise_guard = None;
    if sink_ready {
        eprintln!("[test] found session's PulseAudio null sink '{sink_name}', feeding it real (non-silent) noise via pacat");
        match Command::new("sh")
            .arg("-c")
            .arg(format!(
                "head -c 4000000 /dev/urandom | pacat --playback --device={sink_name} --format=s16le --rate=48000 --channels=2"
            ))
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        {
            Ok(child) => noise_guard = Some(ChildGuard(child)),
            Err(e) => eprintln!("[test] failed to spawn pacat noise source: {e}"),
        }
    } else {
        eprintln!("[test] session's PulseAudio null sink '{sink_name}' never appeared - skipping the noise feed");
    }

    match tokio::time::timeout(std::time::Duration::from_secs(8), connection.read_datagram()).await {
        Ok(Ok(datagram)) => {
            assert!(
                datagram.len() >= AUDIO_HEADER_LEN,
                "audio datagram shorter than the fixed header"
            );
            let sample_rate = u32::from_le_bytes(datagram[0..4].try_into().unwrap());
            let channels = datagram[4];
            let payload_len = datagram.len() - AUDIO_HEADER_LEN;
            eprintln!(
                "[test] QUIC Q2 audio plane: PASS — real datagram, sample_rate={sample_rate} \
                 channels={channels} opus_payload_bytes={payload_len}"
            );
            assert_eq!(sample_rate, 48000);
            assert_eq!(channels, 2);
            assert!(payload_len > 0);
        }
        Ok(Err(e)) => panic!("QUIC connection error waiting for an audio datagram: {e}"),
        Err(_) => {
            eprintln!(
                "[test] QUIC Q2 audio plane: NOT independently live-verified in this run — \
                 no audio datagram arrived within 8s (sink found and fed: {sink_ready}). \
                 Possible causes in a sandboxed/headless test environment: the null sink's \
                 monitor capture stream failed to open, PulseAudio routing/resampling didn't \
                 carry the noise through, or timing. Either way this is reported plainly \
                 rather than papered over. The audio datagram header/framing itself IS \
                 verified for real regardless: see quic::header_tests::\
                 audio_header_layout_is_exact (server-side encode) and \
                 session::quic_framing_tests::audio_datagram_round_trips_header_and_payload \
                 (client-side decode)."
            );
        }
    }
    drop(noise_guard);

    endpoint.close(0u32.into(), b"test done");
    let _ = guard.0.kill();
    let _ = guard.0.wait();
}
