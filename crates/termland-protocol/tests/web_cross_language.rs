//! Cross-language fixtures for the TypeScript browser client.
//!
//! The browser speaks Termland's wire format in TypeScript rather than by
//! compiling this crate to wasm. These files are the contract between the
//! two implementations:
//!
//! - `web/fixtures/from-rust/*.cbor` are encoded here and decoded in
//!   `web/tests/fixtures.test.ts`.
//! - `web/fixtures/from-ts/*.cbor` are encoded by TypeScript and decoded
//!   here.
//!
//! Set `UPDATE_WEB_FIXTURES=1` to rewrite the Rust-originated files after
//! a deliberate protocol change. The TypeScript files are written by
//! `web/`'s own test with the same env var.

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
        let encoded = msg.encode().unwrap_or_else(|e| panic!("{name}: encode {e}"));
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

/// TypeScript-encoded bytes must decode as the same message type, with the
/// fields that matter for the handshake/session/input path intact.
#[test]
fn typescript_fixtures_decode() {
    let dir = fixtures_dir("from-ts");
    if !dir.is_dir() {
        panic!(
            "web/fixtures/from-ts is missing — run the TypeScript test with UPDATE_WEB_FIXTURES=1"
        );
    }
    let mut seen = 0;
    for (name, expected) in canonical_messages() {
        let path = dir.join(format!("{name}.cbor"));
        let bytes = std::fs::read(&path)
            .unwrap_or_else(|e| panic!("{}: {e} — generate with the TypeScript test", path.display()));
        let decoded = Message::decode(&bytes).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(
            decoded.message_id(),
            expected.message_id(),
            "{name} decoded as the wrong type"
        );
        match (&decoded, &expected) {
            (Message::Hello(a), Message::Hello(b)) => {
                assert_eq!(a.protocol_version, b.protocol_version);
                assert_eq!(a.client_name, b.client_name);
            }
            (Message::HelloAck(a), Message::HelloAck(b)) => {
                assert_eq!(a.protocol_version, b.protocol_version);
                assert_eq!(a.server_name, b.server_name);
                assert_eq!(a.session_id, b.session_id);
                assert_eq!(a.auth_required, b.auth_required);
            }
            (Message::SessionCreate(a), Message::SessionCreate(b)) => {
                assert_eq!(a.width, b.width);
                assert_eq!(a.height, b.height);
                assert_eq!(a.supported_codecs, b.supported_codecs);
            }
            (Message::KeyEvent(a), Message::KeyEvent(b)) => {
                assert_eq!(a.scancode, b.scancode);
                assert_eq!(a.state, b.state);
            }
            (Message::TextInput(a), Message::TextInput(b)) => {
                assert_eq!(a.text, b.text);
            }
            (Message::MouseMove(a), Message::MouseMove(b)) => {
                assert!((a.x - b.x).abs() < 1e-6);
                assert!((a.y - b.y).abs() < 1e-6);
                assert_eq!(a.absolute, b.absolute);
            }
            _ => {}
        }
        seen += 1;
    }
    assert!(seen >= 15, "expected the full control-plane fixture set, saw {seen}");
}
