//! Right Ctrl must not reach a nested KWin, which grabs the pointer on it.
//!
//! `#[ignore]`d, and needs a server to talk to: a Plasma 6.8+ host with KWin
//! and a GPU, where the default desktop is KWin nested in labwc. Point it at
//! one (the session it creates is ended at the end):
//!
//! ```text
//! TERMLAND_TEST_SERVER=127.0.0.1:7867 \
//!   cargo test -p termland-server --test nested_kwin_right_ctrl -- --ignored --nocapture
//! ```
//!
//! KWin shows the grab in its window's title ("Press right control to
//! ungrab pointer"), which the server reports in its window list, so the
//! test watches that.

use std::time::{Duration, Instant};

use futures::{SinkExt, StreamExt};
use termland_protocol::input::{KeyEvent, KeyState};
use termland_protocol::{
    AudioCodec, Hello, Message, SessionClose, SessionCreate, SessionMode, TermlandCodec, VideoCodec,
    PROTOCOL_VERSION,
};
use tokio::net::TcpStream;
use tokio_util::codec::Framed;

const KEY_RIGHTCTRL: u32 = 97;

type Conn = Framed<TcpStream, TermlandCodec>;

/// KWin's window title from the next window lists, for up to `wait`.
async fn kwin_titles(conn: &mut Conn, wait: Duration) -> Vec<String> {
    let deadline = Instant::now() + wait;
    let mut titles = Vec::new();
    while Instant::now() < deadline {
        let left = deadline.saturating_duration_since(Instant::now());
        match tokio::time::timeout(left, conn.next()).await {
            Ok(Some(Ok(Message::WindowList(list)))) => titles.extend(
                list.windows.into_iter().map(|w| w.title).filter(|t| t.starts_with("KDE Wayland Compositor")),
            ),
            Ok(Some(Ok(_))) => {}
            _ => break,
        }
    }
    titles
}

async fn key(conn: &mut Conn, scancode: u32, state: KeyState) {
    conn.send(Message::KeyEvent(KeyEvent { scancode, keysym: 0, state, modifiers: 0 }))
        .await
        .expect("send key");
}

#[tokio::test]
#[ignore = "needs a Plasma 6.8 host with KWin and a GPU; set TERMLAND_TEST_SERVER"]
async fn right_ctrl_does_not_make_a_nested_kwin_grab_the_pointer() {
    let Ok(addr) = std::env::var("TERMLAND_TEST_SERVER") else {
        panic!("set TERMLAND_TEST_SERVER=host:port");
    };
    let stream = TcpStream::connect(&addr).await.expect("connect");
    let mut conn = Framed::new(stream, TermlandCodec);
    conn.send(Message::Hello(Hello { protocol_version: PROTOCOL_VERSION, client_name: "right-ctrl-test".into() }))
        .await
        .expect("send Hello");
    match conn.next().await {
        Some(Ok(Message::HelloAck(_))) => {}
        other => panic!("expected HelloAck, got {other:?}"),
    }
    conn.send(Message::SessionCreate(SessionCreate {
        mode: SessionMode::Desktop,
        width: 1280,
        height: 720,
        audio: false,
        quality: 30,
        desktop_shell: None,
        encoder_preset: None,
        encoder_crf: None,
        encoder_extra_params: None,
        supported_codecs: VideoCodec::all_preferred(),
        supported_audio_codecs: AudioCodec::all_preferred(),
        video_may_idle: true,
        bitrate_kbps: None,
    }))
    .await
    .expect("send SessionCreate");

    // KWin's window, before anything is pressed.
    let before = kwin_titles(&mut conn, Duration::from_secs(20)).await;
    eprintln!("[test] KWin's window before: {before:?}");
    assert!(!before.is_empty(), "no nested KWin window appeared: is this a Plasma 6.8 host with a GPU?");

    key(&mut conn, KEY_RIGHTCTRL, KeyState::Pressed).await;
    key(&mut conn, KEY_RIGHTCTRL, KeyState::Released).await;
    let after = kwin_titles(&mut conn, Duration::from_secs(4)).await;
    eprintln!("[test] after Right Ctrl: {after:?}");

    // End the session this test created, whatever the outcome.
    let _ = conn.send(Message::SessionClose(SessionClose { session_id: String::new() })).await;
    tokio::time::sleep(Duration::from_secs(3)).await;

    assert!(
        !after.iter().any(|t| t.contains("ungrab")),
        "KWin grabbed the pointer on Right Ctrl: {after:?}"
    );
}
