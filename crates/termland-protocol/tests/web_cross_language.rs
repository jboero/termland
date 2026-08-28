//! Canonical CBOR fixtures for the browser protocol codec.
//!
//! `web/fixtures/from-rust/*.cbor` are encoded here. The wasm client
//! (`crates/termland-web`) must emit the same bytes — `web/tests/fixtures.test.ts`
//! byte-compares `encodePayload` against these files. There is no second
//! TypeScript encoder.
//!
//! Set `UPDATE_WEB_FIXTURES=1` to rewrite the files after a deliberate
//! protocol change.

use std::path::{Path, PathBuf};

use termland_protocol::*;

fn fixtures_dir(which: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../web/fixtures")
        .join(which)
}

fn canonical_messages() -> Vec<(&'static str, Message)> {
    vec![
        (
            "hello",
            Message::Hello(Hello {
                protocol_version: PROTOCOL_VERSION,
                client_name: "fixture-client".into(),
            }),
        ),
        (
            "hello_ack",
            Message::HelloAck(HelloAck {
                protocol_version: PROTOCOL_VERSION,
                server_name: "termland-server".into(),
                session_id: "session-fixture".into(),
                auth_required: true,
            }),
        ),
        (
            "auth_request",
            Message::AuthRequest(AuthRequest {
                methods: vec!["password".into()],
            }),
        ),
        (
            "auth_response",
            Message::AuthResponse(AuthResponse {
                username: "alice".into(),
                credential: "secret".into(),
            }),
        ),
        (
            "auth_result",
            Message::AuthResult(AuthResult {
                success: true,
                message: "authenticated".into(),
            }),
        ),
        ("session_list", Message::SessionList(SessionList {})),
        (
            "session_list_result",
            Message::SessionListResult(SessionListResult {
                sessions: vec![SessionInfo {
                    session_id: "s1".into(),
                    mode: "desktop".into(),
                    width: 1920,
                    height: 1080,
                    age_secs: 42,
                    attached: false,
                }],
            }),
        ),
        (
            "session_create",
            Message::SessionCreate(SessionCreate {
                mode: SessionMode::Desktop,
                width: 1280,
                height: 720,
                audio: false,
                quality: 75,
                desktop_shell: None,
                encoder_preset: None,
                encoder_crf: None,
                encoder_extra_params: None,
                supported_codecs: vec![VideoCodec::Av1, VideoCodec::Vp9],
                supported_audio_codecs: vec![AudioCodec::Opus],
            }),
        ),
        (
            "session_attach",
            Message::SessionAttach(SessionAttach {
                session_id: "s1".into(),
                audio: false,
                quality: 75,
                encoder_preset: None,
                encoder_crf: None,
                encoder_extra_params: None,
                supported_codecs: vec![VideoCodec::Av1],
                supported_audio_codecs: vec![AudioCodec::Opus],
            }),
        ),
        (
            "session_close",
            Message::SessionClose(SessionClose {
                session_id: "s1".into(),
            }),
        ),
        (
            "session_ready",
            Message::SessionReady(SessionReady {
                width: 1280,
                height: 720,
                xkb_keymap: None,
                codec: Some(VideoCodec::Av1),
                audio_codec: None,
                session_id: "s1".into(),
            }),
        ),
        (
            "session_resize",
            Message::SessionResize(SessionResize {
                width: 800,
                height: 600,
            }),
        ),
        (
            "session_end",
            Message::SessionEnd(SessionEnd {
                reason: "closed by fixture".into(),
            }),
        ),
        (
            "ping",
            Message::Ping(Ping {
                timestamp_us: 1_000_000,
            }),
        ),
        (
            "pong",
            Message::Pong(Pong {
                timestamp_us: 1_000_000,
            }),
        ),
        (
            "key_event",
            Message::KeyEvent(KeyEvent {
                scancode: 30,
                keysym: 0,
                state: KeyState::Pressed,
                modifiers: 0,
            }),
        ),
        (
            "text_input",
            Message::TextInput(TextInput {
                text: "héllo 世界".into(),
            }),
        ),
        (
            "mouse_move",
            Message::MouseMove(MouseMove {
                x: 100.5,
                y: 200.25,
                absolute: true,
            }),
        ),
        (
            "mouse_button",
            Message::MouseButton(MouseButton {
                button: 0x110,
                state: ButtonState::Pressed,
            }),
        ),
        (
            "mouse_scroll",
            Message::MouseScroll(MouseScroll { dx: 0.0, dy: -15.0 }),
        ),
    ]
}

/// Every message this crate encodes must round-trip, and (when the files
/// exist) match what is committed for TypeScript to decode.
#[test]
fn rust_fixtures_round_trip_and_match_committed() {
    let dir = fixtures_dir("from-rust");
    if std::env::var("UPDATE_WEB_FIXTURES").is_ok() {
        std::fs::create_dir_all(&dir).unwrap();
    }
    let mut missing = 0;
    for (name, msg) in canonical_messages() {
        let encoded = msg
            .encode()
            .unwrap_or_else(|e| panic!("{name}: encode {e}"));
        let decoded = Message::decode(&encoded).unwrap_or_else(|e| panic!("{name}: decode {e}"));
        assert_eq!(
            decoded.message_id(),
            msg.message_id(),
            "{name} changed type on round trip"
        );

        let path = dir.join(format!("{name}.cbor"));
        if std::env::var("UPDATE_WEB_FIXTURES").is_ok() {
            std::fs::write(&path, &encoded).unwrap();
            continue;
        }
        match std::fs::read(&path) {
            Ok(committed) => assert_eq!(
                encoded, committed,
                "{name}: Rust encoding drifted from the committed fixture; \
                 re-run with UPDATE_WEB_FIXTURES=1 if the change is deliberate"
            ),
            Err(_) => missing += 1,
        }
    }
    if std::env::var("UPDATE_WEB_FIXTURES").is_err() {
        assert_eq!(
            missing, 0,
            "web/fixtures/from-rust is missing {missing} file(s); \
             run UPDATE_WEB_FIXTURES=1 cargo test -p termland-protocol --test web_cross_language"
        );
    }
}
