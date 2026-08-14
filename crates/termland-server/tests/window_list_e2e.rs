//! End-to-end check that a real session actually delivers `WindowList` to a
//! client.
//!
//! `#[ignore]`d: spawns a real server, which starts a real compositor and a
//! real GUI app. Run on a desktop with:
//!
//! ```text
//! cargo test -p termland-server --test window_list_e2e -- --ignored --nocapture
//! ```
//!
//! `toplevel_enumeration` in termland-compositor proves the Wayland side in
//! isolation; the protocol tests prove the message round-trips. Neither proves
//! the server actually *sends* it during a session, which is the part a user
//! would notice missing.

use std::process::{Child, Command, Stdio};
use std::time::Duration;

use futures::{SinkExt, StreamExt};
use tokio::net::TcpStream;
use tokio_util::codec::Framed;

use termland_protocol::{
    AudioCodec, Hello, Message, SessionCreate, SessionMode, TermlandCodec, VideoCodec,
    PROTOCOL_VERSION,
};

struct ChildGuard(Child);
impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Closes the session the test creates. Same reasoning as `quic_q2_planes`:
/// compositors are setsid-detached and outlive the server, so killing the
/// child is not teardown.
struct SessionGuard {
    bin: &'static str,
    id: Option<String>,
}
impl Drop for SessionGuard {
    fn drop(&mut self) {
        let Some(id) = self.id.take() else { return };
        let sink = format!("termland_{}", id.replace('-', "_"));
        if let Ok(out) = Command::new("pactl").args(["list", "short", "modules"]).output() {
            for line in String::from_utf8_lossy(&out.stdout).lines() {
                if line.contains(&format!("sink_name={sink}")) {
                    if let Some(m) = line.split_whitespace().next() {
                        let _ = Command::new("pactl").args(["unload-module", m]).output();
                    }
                }
            }
        }
        match Command::new(self.bin).args(["--close-session", &id]).output() {
            Ok(o) if o.status.success() => eprintln!("[test] closed session {id}"),
            _ => eprintln!("[test] WARNING: could not close session {id} — close it by hand"),
        }
    }
}

#[tokio::test]
#[ignore = "needs a real compositor and GUI app; run manually on a desktop"]
async fn a_real_session_delivers_a_window_list() {
    let port: u16 = 27871;
    let bin = env!("CARGO_BIN_EXE_termland-server");

    let child = Command::new(bin)
        .args(["--bind", "127.0.0.1", "--port", &port.to_string()])
        .env("RUST_LOG", "info")
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to spawn termland-server");
    let mut server = ChildGuard(child);
    let mut session = SessionGuard { bin, id: None };

    tokio::time::sleep(Duration::from_millis(800)).await;

    let stream = TcpStream::connect(("127.0.0.1", port)).await.expect("connect");
    let mut framed = Framed::new(stream, TermlandCodec);

    framed
        .send(Message::Hello(Hello {
            protocol_version: PROTOCOL_VERSION,
            client_name: "window-list-e2e".into(),
        }))
        .await
        .expect("send Hello");
    match framed.next().await {
        Some(Ok(Message::HelloAck(_))) => {}
        other => panic!("expected HelloAck, got {other:?}"),
    }

    framed
        .send(Message::SessionCreate(SessionCreate {
            mode: SessionMode::Desktop,
            width: 800,
            height: 600,
            audio: false,
            quality: 30,
            // A bare terminal: enough to map one window, without dragging in a
            // whole Plasma session.
            desktop_shell: Some("konsole".into()),
            encoder_preset: None,
            encoder_crf: None,
            encoder_extra_params: None,
            supported_codecs: VideoCodec::all_preferred(),
            supported_audio_codecs: AudioCodec::all_preferred(),
        }))
        .await
        .expect("send SessionCreate");

    // Collect until a WindowList arrives, or we give up.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(40);
    let mut got: Option<termland_protocol::WindowList> = None;

    while tokio::time::Instant::now() < deadline {
        let Ok(Some(Ok(msg))) =
            tokio::time::timeout(Duration::from_secs(5), framed.next()).await
        else {
            continue;
        };
        match msg {
            Message::SessionReady(sr) => {
                eprintln!("[test] SessionReady, session_id={}", sr.session_id);
                session.id = Some(sr.session_id.clone());
            }
            Message::WindowList(list) => {
                eprintln!("[test] WindowList: {list:#?}");
                if !list.windows.is_empty() {
                    got = Some(list);
                    break;
                }
            }
            Message::SessionEnd(se) => panic!("session ended early: {}", se.reason),
            _ => {}
        }
    }

    let list = got.expect(
        "no non-empty WindowList arrived — the server never reported the session's windows",
    );
    assert!(
        list.windows.iter().any(|w| !w.app_id.is_empty() || !w.title.is_empty()),
        "a window was reported but had neither app_id nor title: {list:#?}",
    );

    let _ = server.0.kill();
}
