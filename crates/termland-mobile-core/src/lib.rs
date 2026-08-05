//! Termland's shared core for mobile clients (Android/iOS), exposed to
//! Kotlin and Swift with UniFFI.
//!
//! It owns everything platform-independent — transport, handshake, PAM auth,
//! session create/attach/list/close, codec negotiation, packet demux and input
//! event construction — and deliberately owns nothing else. In particular it
//! **never links FFmpeg**: video packets are handed up codec-tagged and decoded
//! by MediaCodec/VideoToolbox, and Opus packets are passed through undecoded.
//! That keeps the binary small, the battery cost on the hardware decoder, and
//! the licensing simple.
//!
//! The wire format comes from `termland-protocol` unchanged, so a mobile client
//! and the desktop client are indistinguishable to the server.

mod error;
mod session;
mod transport;
mod types;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use tokio::sync::mpsc;

pub use error::TermlandError;
pub use types::{
    MobileCodec, ServerProfile, SessionObserver, SessionParams, SessionReadyInfo, SessionSummary,
    VideoPacket,
};

use error::Result;
use session::InputCommand;

uniffi::setup_scaffolding!();

/// evdev button codes, matching what the desktop client sends.
const BTN_LEFT: u32 = 0x110;

/// A live streaming session's handle. Dropping `input_tx` is what tells the
/// session loop to detach.
struct Session {
    input_tx: mpsc::UnboundedSender<InputCommand>,
    connected: Arc<AtomicBool>,
}

/// The single object the foreign side holds. It owns the tokio runtime for the
/// whole process lifetime of the app: mobile UIs create and tear down screens
/// constantly, and spinning a runtime up per connect would be both slow and a
/// good way to leak threads.
///
/// One client streams at most one session at a time; connecting again replaces
/// (detaches) the previous one.
#[derive(uniffi::Object)]
pub struct TermlandClient {
    /// `None` only if the runtime failed to build. Kept as an Option so that
    /// failure surfaces as a `TermlandError` instead of a panic through FFI,
    /// where an unwind is undefined behaviour.
    runtime: Option<tokio::runtime::Runtime>,
    session: std::sync::Mutex<Option<Session>>,
}

#[uniffi::export]
impl TermlandClient {
    #[uniffi::constructor]
    pub fn new() -> Arc<Self> {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            // Two workers is ample: this crate only pumps one socket and never
            // decodes, and phone core counts are better spent elsewhere.
            .worker_threads(2)
            .thread_name("termland-net")
            .enable_all()
            .build()
            .inspect_err(|e| tracing::error!("failed to build tokio runtime: {e}"))
            .ok();

        Arc::new(Self { runtime, session: std::sync::Mutex::new(None) })
    }

    // --- control plane (blocking; call from a background thread) ---

    pub fn list_sessions(&self, profile: ServerProfile) -> Result<Vec<SessionSummary>> {
        self.runtime()?.block_on(session::list_sessions(&profile))
    }

    pub fn close_session(&self, profile: ServerProfile, session_id: String) -> Result<()> {
        self.runtime()?
            .block_on(session::close_session(&profile, session_id))
    }

    // --- streaming ---

    /// Create a new remote session and start streaming it. Blocks until the
    /// server reports SessionReady, so a returned `Ok` means the observer has
    /// already been told the codec and geometry.
    pub fn connect_new(
        &self,
        profile: ServerProfile,
        params: SessionParams,
        observer: Arc<dyn SessionObserver>,
    ) -> Result<()> {
        self.start(profile, None, params, observer)
    }

    /// Resume an existing session by id (the v0.5 persistence path — this is how
    /// a dropped mobile link recovers without losing the desktop).
    pub fn attach(
        &self,
        profile: ServerProfile,
        session_id: String,
        params: SessionParams,
        observer: Arc<dyn SessionObserver>,
    ) -> Result<()> {
        self.start(profile, Some(session_id), params, observer)
    }

    /// Detach: stop streaming, leave the remote session running and resumable.
    ///
    /// Nothing is sent to the server — a plain disconnect is exactly what the
    /// server treats as a detach. Sending SessionClose here would destroy the
    /// session, which is what the session-list long-press does, not this.
    pub fn disconnect(&self) {
        let taken = match self.session.lock() {
            Ok(mut guard) => guard.take(),
            Err(poisoned) => poisoned.into_inner().take(),
        };
        if let Some(s) = taken {
            s.connected.store(false, Ordering::SeqCst);
            // Dropping the sender closes the channel, which wakes the loop and
            // drops the socket.
            drop(s.input_tx);
        }
    }

    pub fn is_connected(&self) -> bool {
        self.with_session(|s| s.connected.load(Ordering::SeqCst))
            .unwrap_or(false)
    }

    // --- input: fire-and-forget, safe to call from the UI thread ---

    /// `scancode` is an evdev scancode and `keysym` an X11 keysym; the Android
    /// keycode mapping lives in Kotlin because only the UI knows the layout.
    /// Use this for editing/navigation/shortcut keys — committed text goes to
    /// `send_text`.
    pub fn send_key(&self, scancode: u32, keysym: u32, pressed: bool, modifiers: u32) {
        self.send(InputCommand::Key(termland_protocol::input::KeyEvent {
            scancode,
            keysym,
            state: if pressed {
                termland_protocol::input::KeyState::Pressed
            } else {
                termland_protocol::input::KeyState::Released
            },
            modifiers,
        }));
    }

    /// Committed Unicode text from an IME / soft keyboard. Separate from
    /// `send_key` because autocorrect, emoji and CJK output has no scancode to
    /// send; the server injects it as text.
    pub fn send_text(&self, text: String) {
        if text.is_empty() {
            return;
        }
        self.send(InputCommand::Text(termland_protocol::TextInput { text }));
    }

    /// `absolute = true` => `x`/`y` are remote-framebuffer pixel coordinates
    /// (direct touch mode); `false` => a relative delta (trackpad mode).
    pub fn send_pointer_motion(&self, x: f64, y: f64, absolute: bool) {
        self.send(InputCommand::Motion(termland_protocol::input::MouseMove {
            x,
            y,
            absolute,
        }));
    }

    /// `button` is an evdev code: 0x110 left, 0x111 right, 0x112 middle.
    pub fn send_pointer_button(&self, button: u32, pressed: bool) {
        self.send(InputCommand::Button(termland_protocol::input::MouseButton {
            button: if button == 0 { BTN_LEFT } else { button },
            state: if pressed {
                termland_protocol::input::ButtonState::Pressed
            } else {
                termland_protocol::input::ButtonState::Released
            },
        }));
    }

    pub fn send_scroll(&self, dx: f64, dy: f64) {
        self.send(InputCommand::Scroll(termland_protocol::input::MouseScroll { dx, dy }));
    }

    pub fn send_clipboard(&self, mime_type: String, data: Vec<u8>) {
        self.send(InputCommand::Clipboard(termland_protocol::ClipboardPayload {
            mime_type,
            data,
        }));
    }

    /// Ask the server to resize the session (device rotation, window change).
    pub fn resize(&self, width: u32, height: u32) {
        self.send(InputCommand::Resize(termland_protocol::SessionResize {
            width,
            height,
        }));
    }

    /// `true` = server composites its cursor into the video (right for touch,
    /// where there is no local pointer to draw); `false` = the client draws it.
    pub fn set_cursor_in_frame(&self, in_frame: bool) {
        self.send(InputCommand::CursorInFrame(in_frame));
    }
}

impl TermlandClient {
    fn runtime(&self) -> Result<&tokio::runtime::Runtime> {
        self.runtime
            .as_ref()
            .ok_or_else(|| TermlandError::io("tokio runtime unavailable"))
    }

    fn with_session<T>(&self, f: impl FnOnce(&Session) -> T) -> Option<T> {
        let guard = match self.session.lock() {
            Ok(g) => g,
            Err(poisoned) => poisoned.into_inner(),
        };
        guard.as_ref().map(f)
    }

    /// Queue an input message. Silently dropped when nothing is connected:
    /// these are called from touch handlers where an error has nowhere to go,
    /// and a stale event after a disconnect is not worth reporting.
    fn send(&self, cmd: InputCommand) {
        let queued = self
            .with_session(|s| s.input_tx.send(cmd).is_ok())
            .unwrap_or(false);
        if !queued {
            tracing::debug!("input dropped: no active session");
        }
    }

    fn start(
        &self,
        profile: ServerProfile,
        attach_to: Option<String>,
        params: SessionParams,
        observer: Arc<dyn SessionObserver>,
    ) -> Result<()> {
        // Replacing a session implicitly detaches the old one, so the previous
        // socket and loop are never left running behind the new stream.
        self.disconnect();

        let runtime = self.runtime()?;
        let (framed, quic_connection, ready) =
            runtime.block_on(session::open_session(&profile, attach_to, &params))?;

        let (input_tx, input_rx) = mpsc::unbounded_channel();
        let connected = Arc::new(AtomicBool::new(true));

        // Announce readiness before the loop starts so the platform decoder is
        // guaranteed to be configured before the first packet arrives.
        observer.on_session_ready(ready.clone());

        let loop_connected = connected.clone();
        runtime.spawn(session::run_session(
            framed,
            quic_connection,
            ready,
            observer,
            input_rx,
            loop_connected,
        ));

        match self.session.lock() {
            Ok(mut guard) => *guard = Some(Session { input_tx, connected }),
            Err(poisoned) => *poisoned.into_inner() = Some(Session { input_tx, connected }),
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use termland_protocol::{Message, TermlandCodec, VideoCodec};
    use tokio_util::codec::{Decoder, Encoder};

    fn params() -> SessionParams {
        SessionParams {
            width: 1920,
            height: 1080,
            quality: 75,
            audio: false,
            desktop_shell: None,
            app_command: None,
            supported_codecs: vec![MobileCodec::H265, MobileCodec::H264],
        }
    }

    /// The contract's `SessionParams` has to produce a `SessionCreate` the real
    /// server would accept, so round-trip it through the actual wire codec
    /// rather than just constructing it.
    #[test]
    fn session_params_roundtrip_through_wire_codec() {
        let p = params();
        let msg = Message::SessionCreate(termland_protocol::SessionCreate {
            mode: p.session_mode(),
            width: p.width,
            height: p.height,
            audio: p.audio,
            quality: p.quality,
            desktop_shell: p.desktop_shell.clone(),
            encoder_preset: None,
            encoder_crf: None,
            encoder_extra_params: None,
            supported_codecs: p.advertised_codecs(),
        });

        let mut codec = TermlandCodec;
        let mut buf = bytes::BytesMut::new();
        Encoder::encode(&mut codec, msg, &mut buf).expect("encode SessionCreate");

        match codec.decode(&mut buf).expect("decode").expect("full frame") {
            Message::SessionCreate(sc) => {
                assert_eq!(sc.width, 1920);
                assert_eq!(sc.height, 1080);
                assert_eq!(sc.quality, 75);
                assert!(matches!(sc.mode, termland_protocol::SessionMode::Desktop));
                assert_eq!(sc.supported_codecs, vec![VideoCodec::H265, VideoCodec::H264]);
            }
            other => panic!("expected SessionCreate, got {other:?}"),
        }
    }

    #[test]
    fn app_command_becomes_app_mode() {
        let p = SessionParams { app_command: Some("firefox --kiosk about:blank".into()), ..params() };
        match p.session_mode() {
            termland_protocol::SessionMode::App { command, args } => {
                assert_eq!(command, "firefox");
                assert_eq!(args, vec!["--kiosk", "about:blank"]);
            }
            other => panic!("expected App mode, got {other:?}"),
        }
        // Blank/whitespace must not turn into an App session with no command.
        assert!(matches!(
            SessionParams { app_command: Some("  ".into()), ..params() }.session_mode(),
            termland_protocol::SessionMode::Desktop
        ));
    }

    #[test]
    fn empty_codec_list_advertises_all() {
        let p = SessionParams { supported_codecs: vec![], ..params() };
        assert_eq!(p.advertised_codecs(), VideoCodec::all_preferred());
    }

    #[test]
    fn codec_tags_survive_both_conversions() {
        for c in VideoCodec::all_preferred() {
            assert_eq!(VideoCodec::from(MobileCodec::from(c)), c);
        }
    }

    /// `send_text` must produce the TextInput message the server's text injector
    /// listens for, and must be a no-op with nothing connected.
    #[test]
    fn text_input_encodes_and_input_is_safe_while_disconnected() {
        let mut codec = TermlandCodec;
        let mut buf = bytes::BytesMut::new();
        Encoder::encode(
            &mut codec,
            Message::TextInput(termland_protocol::TextInput { text: "héllo 🌍".into() }),
            &mut buf,
        )
        .expect("encode TextInput");
        match codec.decode(&mut buf).expect("decode").expect("full frame") {
            Message::TextInput(t) => assert_eq!(t.text, "héllo 🌍"),
            other => panic!("expected TextInput, got {other:?}"),
        }

        let client = TermlandClient::new();
        assert!(!client.is_connected());
        client.send_text("dropped".into());
        client.send_key(1, 0xff1b, true, 0);
        client.send_pointer_motion(10.0, 20.0, true);
        client.send_pointer_button(BTN_LEFT, true);
        client.send_scroll(0.0, -1.0);
        client.send_clipboard("text/plain".into(), b"x".to_vec());
        client.resize(800, 600);
        client.set_cursor_in_frame(true);
        client.disconnect();
        assert!(!client.is_connected());
    }

    fn profile() -> ServerProfile {
        ServerProfile {
            host: "example.test".into(),
            port: 7867,
            use_tls: false,
            accept_invalid_certs: false,
            username: None,
            password: None,
            use_ssh: false,
            use_quic: false,
        }
    }

    /// `Transport::for_profile` is the only place a `ServerProfile` gets
    /// turned into a transport choice; a wrong priority here would silently
    /// send SSH-configured profiles over plain TCP or vice versa.
    #[test]
    fn transport_selection_prioritizes_ssh_over_tls_over_tcp() {
        use crate::transport::Transport;

        assert!(matches!(Transport::for_profile(&profile()), Transport::Tcp));

        let tls = ServerProfile { use_tls: true, ..profile() };
        assert!(matches!(Transport::for_profile(&tls), Transport::Tls { accept_invalid_certs: false }));

        let ssh = ServerProfile { use_ssh: true, ..profile() };
        assert!(matches!(Transport::for_profile(&ssh), Transport::Ssh { .. }));

        // use_ssh wins even if use_tls is also set: SSH tunnels the whole
        // connection, so a leftover TLS flag from switching modes in the UI
        // must not be able to downgrade or conflict with it.
        let both = ServerProfile { use_ssh: true, use_tls: true, ..profile() };
        assert!(matches!(Transport::for_profile(&both), Transport::Ssh { .. }));
    }

    /// Same priority check as above, for `use_quic`: it must win over
    /// `use_tls` (QUIC replaces the TCP+TLS socket outright, so a leftover
    /// TLS flag must not fight it) but lose to `use_ssh` (an SSH tunnel is a
    /// different connection mechanism entirely, not a peer of QUIC/TLS).
    #[test]
    fn transport_selection_prioritizes_quic_over_tls_but_not_over_ssh() {
        use crate::transport::Transport;

        let quic = ServerProfile { use_quic: true, ..profile() };
        assert!(matches!(Transport::for_profile(&quic), Transport::Quic { accept_invalid_certs: false }));

        let quic_and_tls = ServerProfile { use_quic: true, use_tls: true, ..profile() };
        assert!(matches!(Transport::for_profile(&quic_and_tls), Transport::Quic { .. }));

        let quic_and_ssh = ServerProfile { use_quic: true, use_ssh: true, ..profile() };
        assert!(matches!(Transport::for_profile(&quic_and_ssh), Transport::Ssh { .. }));
    }

    /// SSH auth credentials come straight from the profile's existing
    /// username/password fields (no separate SSH-specific fields), and must
    /// tolerate an absent password rather than panicking or auth-ing with a
    /// stale value.
    #[test]
    fn ssh_transport_takes_username_and_password_from_profile() {
        use crate::transport::Transport;

        let p = ServerProfile {
            use_ssh: true,
            username: Some("alice".into()),
            password: Some("hunter2".into()),
            ..profile()
        };
        match Transport::for_profile(&p) {
            Transport::Ssh { username, password } => {
                assert_eq!(username, "alice");
                assert_eq!(password, "hunter2");
            }
            _ => panic!("expected Transport::Ssh"),
        }

        // No password set: must default to empty, not panic.
        let no_pw = ServerProfile { use_ssh: true, username: Some("bob".into()), ..profile() };
        match Transport::for_profile(&no_pw) {
            Transport::Ssh { username, password } => {
                assert_eq!(username, "bob");
                assert_eq!(password, "");
            }
            _ => panic!("expected Transport::Ssh"),
        }
    }
}
