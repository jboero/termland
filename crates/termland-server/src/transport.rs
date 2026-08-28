use anyhow::{Context, Result};
use futures::{SinkExt, StreamExt};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::TcpListener;
use tokio_util::codec::Framed;
use termland_protocol::*;
use libpulse_binding as pulse;
use libpulse_simple_binding as psimple;

/// Run the server in SSH subsystem mode: protocol over stdin/stdout.
pub async fn run_subsystem() -> Result<()> {
    let stdin = tokio::io::stdin();
    let stdout = tokio::io::stdout();
    let io = tokio::io::join(stdin, stdout);
    handle_session(io, false, crate::media::MediaConnection::None).await
}

const MAX_CONCURRENT_SESSIONS: usize = 32;

/// Run the server as a TCP listener, optionally with TLS and PAM auth.
pub async fn run_tcp_listener(
    bind: &str,
    port: u16,
    tls_acceptor: Option<tokio_rustls::TlsAcceptor>,
    require_auth: bool,
) -> Result<()> {
    let listener = TcpListener::bind(format!("{bind}:{port}")).await?;
    tracing::info!("Listening on {bind}:{port} (max {MAX_CONCURRENT_SESSIONS} sessions)");

    let semaphore = Arc::new(tokio::sync::Semaphore::new(MAX_CONCURRENT_SESSIONS));

    loop {
        let (socket, addr) = listener.accept().await?;

        let permit = match semaphore.clone().try_acquire_owned() {
            Ok(p) => p,
            Err(_) => {
                tracing::warn!("Rejected connection from {addr}: max sessions reached");
                drop(socket);
                continue;
            }
        };

        tracing::info!("Connection from {addr} ({} active)", MAX_CONCURRENT_SESSIONS - semaphore.available_permits());
        let acceptor = tls_acceptor.clone();
        let auth = require_auth;
        tokio::spawn(async move {
            let result = if let Some(acceptor) = acceptor {
                match acceptor.accept(socket).await {
                    Ok(tls_stream) => {
                        tracing::info!("TLS handshake complete for {addr}");
                        handle_session(tls_stream, auth, crate::media::MediaConnection::None).await
                    }
                    Err(e) => {
                        tracing::warn!("TLS handshake failed for {addr}: {e}");
                        return;
                    }
                }
            } else {
                handle_session(socket, auth, crate::media::MediaConnection::None).await
            };
            if let Err(e) = result {
                tracing::error!("Session error for {addr}: {e}");
            }
            drop(permit);
        });
    }
}

/// Frame data from the capture thread (video-encoded).
enum CapturedFrame {
    Video {
        data: Vec<u8>,
        keyframe: bool,
        width: u16,
        height: u16,
        timestamp_us: u64,
        codec: termland_protocol::VideoCodec,
    },
}

/// Input commands sent to the input injection thread.
enum InputCommand {
    Key { scancode: u32, pressed: bool },
    /// Already-composed Unicode from a soft keyboard / IME. Injected via a
    /// temporary xkb keymap rather than scancodes.
    Text(String),
    PointerMove { x: f64, y: f64, width: u32, height: u32, absolute: bool },
    PointerButton { button: u32, pressed: bool },
    Scroll { dx: f64, dy: f64 },
    Stop,
}

/// Handle a single client session over any AsyncRead+AsyncWrite transport.
/// `media` is `None` for TCP/TLS/SSH (video on the control stream) and a
/// connection handle for `--quic` / `--webtransport` (Q2 video uni stream).
#[allow(clippy::too_many_lines)]
pub(crate) async fn handle_session<T>(
    io: T,
    require_auth: bool,
    media: crate::media::MediaConnection,
) -> Result<()>
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
            auth_required: require_auth,
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

    // Authentication (if required). `authenticated_user` is the server-side
    // source of truth for session isolation (setuid into this user once a
    // session is created below) - it comes from PAM here, never from any
    // client-supplied message, and stays `None` (preserving pre-isolation
    // behavior exactly) when the server isn't running with `--auth`.
    let authenticated_user: Option<String> = if require_auth {
        framed.send(Message::AuthRequest(AuthRequest {
            methods: vec!["password".into()],
        })).await.context("failed to send AuthRequest")?;

        let msg = framed.next().await
            .context("connection closed before AuthResponse")?
            .context("failed to decode AuthResponse")?;

        let (username, password) = match msg {
            Message::AuthResponse(ar) => (ar.username, ar.credential),
            other => anyhow::bail!("expected AuthResponse, got {:?}", other.message_id()),
        };

        let auth_username = username.clone();
        let ok = tokio::task::spawn_blocking(move || {
            crate::auth::pam_authenticate_user(&auth_username, &password)
        }).await??;

        if !ok {
            // Delay before responding to slow down brute-force attempts
            tokio::time::sleep(tokio::time::Duration::from_secs(3)).await;
            framed.send(Message::AuthResult(AuthResult {
                success: false,
                message: "authentication failed".into(),
            })).await?;
            anyhow::bail!("authentication failed");
        }

        // A successful PAM auth confirms this is a real, authenticatable
        // system account, but not that the username string is free of
        // control characters some PAM backend might tolerate. `owner` below
        // gets persisted into the registry's line-based key=value format
        // (registry.rs), where an embedded newline could inject a spoofed
        // key into a session record - reject rather than let that reach
        // storage. A newline is not a valid character in any real Unix
        // account name, so this can only ever reject a pathological
        // username, never a real user.
        if username.contains('\n') {
            anyhow::bail!("rejected authenticated username containing a newline");
        }

        framed.send(Message::AuthResult(AuthResult {
            success: true,
            message: "authenticated".into(),
        })).await.context("failed to send AuthResult")?;

        tracing::info!("Client authenticated");
        Some(username)
    } else {
        None
    };

    // Ensure the session registry directory exists. Do not discard the error:
    // when this fails, the session still proceeds and dies much later with a
    // bare "open log <path>: No such file or directory" from the compositor
    // spawn, which names the symptom and hides the cause (issue #8). The
    // usual culprit is unit sandboxing — ProtectHome= covers /run/user, not
    // just /home and /root — or an XDG_RUNTIME_DIR that logind has reaped.
    if let Err(e) = crate::registry::ensure_dir() {
        tracing::error!(
            "Failed to create session registry directory {}: {e} \
             (session creation will fail; check the service unit's sandboxing \
             and XDG_RUNTIME_DIR)",
            crate::registry::base_dir().display(),
        );
    }

    // Session control loop. The client either manages sessions (list / close) or
    // starts one — create a new persistent session, or attach to an existing one
    // — which then streams until the client detaches (disconnects) or closes it.
    loop {
        let msg = match framed.next().await {
            Some(Ok(m)) => m,
            Some(Err(e)) => {
                tracing::warn!("control decode error: {e}");
                return Ok(());
            }
            None => {
                tracing::info!("Client disconnected before selecting a session");
                return Ok(());
            }
        };

        match msg {
            // Ping is valid before SessionCreate: the browser sends it from
            // the session list. Rejecting it closed the session with
            // STOP_SENDING five seconds after connect.
            Message::Ping(p) => {
                framed
                    .send(Message::Pong(Pong {
                        timestamp_us: p.timestamp_us,
                    }))
                    .await
                    .context("failed to send Pong")?;
            }
            Message::SessionEnd(se) => {
                tracing::info!("Client ended before selecting a session: {}", se.reason);
                return Ok(());
            }
            Message::SessionList(_) => {
                // Once --auth is on, only show the caller's own sessions -
                // otherwise any authenticated user could enumerate every
                // other user's session ids from this list alone (and then
                // Attach/Close would be the only remaining gate). A record
                // with no owner predates ownership tracking and is shown to
                // everyone, same as when auth is off entirely.
                let sessions: Vec<SessionInfo> = crate::registry::list_alive()
                    .into_iter()
                    .filter(|r| owned_by_caller(&r.owner, &authenticated_user))
                    .map(|r| SessionInfo {
                        session_id: r.session_id,
                        mode: r.mode,
                        width: r.width,
                        height: r.height,
                        age_secs: crate::registry::now_unix().saturating_sub(r.created_at_unix),
                        attached: false,
                    })
                    .collect();
                tracing::info!("Listing {} live session(s)", sessions.len());
                framed
                    .send(Message::SessionListResult(SessionListResult { sessions }))
                    .await
                    .context("failed to send SessionListResult")?;
            }
            Message::SessionClose(sc) => {
                // Same ownership check as SessionAttach (see run_session) -
                // knowing a session id (even an unguessable one, once handed
                // out by SessionList to its owner) must not be enough for a
                // different authenticated user to close it.
                if let Some(rec) = crate::registry::read(&sc.session_id) {
                    if !owned_by_caller(&rec.owner, &authenticated_user) {
                        tracing::warn!(
                            "refusing SessionClose for {}: not owned by the authenticated caller",
                            sc.session_id
                        );
                        framed
                            .send(Message::SessionEnd(SessionEnd {
                                reason: "not authorized to close this session".into(),
                            }))
                            .await
                            .ok();
                        continue;
                    }
                }
                tracing::info!("Client requested close of session {}", sc.session_id);
                crate::registry::close(&sc.session_id);
                framed
                    .send(Message::SessionEnd(SessionEnd {
                        reason: format!("closed {}", sc.session_id),
                    }))
                    .await
                    .ok();
            }
            Message::SessionCreate(sc) => {
                run_session(&mut framed, SessionRequest::Create(sc), authenticated_user.clone(), media).await?;
                return Ok(());
            }
            Message::SessionAttach(sa) => {
                run_session(&mut framed, SessionRequest::Attach(sa), authenticated_user.clone(), media).await?;
                return Ok(());
            }
            other => {
                anyhow::bail!("expected a session control message, got {:?}", other.message_id());
            }
        }
    }
}

/// Is a session record visible/accessible to the currently-authenticated
/// caller? `caller` is `handle_session`'s `authenticated_user` - `None` when
/// the server isn't running with `--auth`, in which case there is no
/// identity concept at all and everything is accessible (unchanged from
/// before session ownership existed). When auth IS on, a record with no
/// owner (written before this field existed) is also accessible to everyone,
/// since it can't actually belong to a different authenticated user under
/// the current code. Otherwise, the owner must match exactly.
fn owned_by_caller(owner: &Option<String>, caller: &Option<String>) -> bool {
    match (owner, caller) {
        (_, None) => true,
        (None, Some(_)) => true,
        (Some(o), Some(c)) => o == c,
    }
}

/// A client's request that starts streaming: a new session or a resume.
enum SessionRequest {
    Create(SessionCreate),
    Attach(SessionAttach),
}

/// Audio codecs this server can encode, in preference order.
const SERVER_AUDIO_CODECS: &[termland_protocol::AudioCodec] =
    &[termland_protocol::AudioCodec::Opus];

/// PulseAudio sink name for a session's null sink.
///
/// Single source of truth: `audio_capture_thread` creates the sink under this
/// name and the compositor is pointed at it via `PULSE_SINK`, so the two must
/// agree exactly or session audio silently goes to the wrong place.
fn session_sink_name(session_id: &str) -> String {
    format!("termland_{}", session_id.replace('-', "_"))
}

/// Run one streaming session (freshly created or resumed) until the client
/// detaches or closes it. On plain disconnect the compositor is left running
/// (the session persists); on an explicit close/compositor-exit it is removed.
///
/// `run_as`: the PAM-authenticated username from `handle_session`'s auth
/// block (server-side truth), or `None` if the server isn't running with
/// `--auth`. Deliberately NOT sourced from `request` - the client must never
/// get to say who the session runs as.
///
/// `media`: how (if at all) this session opens Q2 video/audio planes. `None`
/// for TCP/TLS/SSH; a live connection handle for `--quic` / `--webtransport`.
/// The video uni stream is opened here (not in `handle_session`) — see the
/// `open_planes()` call below for why.
async fn run_session<T>(
    framed: &mut Framed<T, TermlandCodec>,
    request: SessionRequest,
    run_as: Option<String>,
    media: crate::media::MediaConnection,
) -> Result<()>
where
    T: AsyncRead + AsyncWrite + Unpin,
{
    // Resolve everything the capture/stream setup needs from either request kind.
    let (
        session_id,
        width,
        height,
        quality,
        mode,
        mode_desc,
        desktop_shell,
        encoder_preset,
        encoder_crf,
        encoder_extra_params,
        supported_codecs,
        supported_audio_codecs,
        audio,
        start_kind,
        is_new,
    );

    match request {
        SessionRequest::Create(sc) => {
            tracing::info!(
                "Create session: {}x{} mode={:?} audio={} quality={} desktop_shell={:?}",
                sc.width, sc.height, sc.mode, sc.audio, sc.quality, sc.desktop_shell
            );
            let ds = sc.desktop_shell.clone().filter(|s| !s.is_empty());
            let ep = sc.encoder_preset.clone().filter(|s| !s.is_empty());
            let ex = sc.encoder_extra_params.clone().filter(|s| !s.is_empty());

            // SECURITY: validate client-supplied commands before any shell use.
            if let Some(ref shell) = ds {
                termland_compositor::validate_shell_command(shell)
                    .map_err(|e| anyhow::anyhow!("rejected desktop_shell: {e}"))?;
            }
            if let termland_protocol::SessionMode::App { ref command, ref args } = sc.mode {
                termland_compositor::validate_shell_command(command)
                    .map_err(|e| anyhow::anyhow!("rejected app command: {e}"))?;
                for arg in args {
                    termland_compositor::validate_shell_command(arg)
                        .map_err(|e| anyhow::anyhow!("rejected app arg: {e}"))?;
                }
            }
            if let Some(ref preset) = ep {
                termland_compositor::validate_shell_command(preset)
                    .map_err(|e| anyhow::anyhow!("rejected encoder_preset: {e}"))?;
            }
            if let Some(ref extra) = ex {
                termland_compositor::validate_shell_command(extra)
                    .map_err(|e| anyhow::anyhow!("rejected encoder_extra_params: {e}"))?;
            }

            let id = crate::registry::new_session_id();
            session_id = id.clone();
            width = sc.width;
            height = sc.height;
            quality = sc.quality.clamp(1, 100);
            mode_desc = crate::registry::describe_mode(&sc.mode);
            mode = sc.mode.into();
            desktop_shell = ds;
            encoder_preset = ep;
            encoder_crf = sc.encoder_crf;
            encoder_extra_params = ex;
            supported_codecs = if sc.supported_codecs.is_empty() {
                termland_protocol::VideoCodec::all_preferred()
            } else {
                sc.supported_codecs
            };
            supported_audio_codecs = if sc.supported_audio_codecs.is_empty() {
                termland_protocol::AudioCodec::legacy_default()
            } else {
                sc.supported_audio_codecs
            };
            audio = sc.audio;
            start_kind = StartKind::Create { log_path: crate::registry::log_path(&id) };
            is_new = true;
        }
        SessionRequest::Attach(sa) => {
            let Some(rec) = crate::registry::read(&sa.session_id).filter(crate::registry::is_live) else {
                tracing::warn!("attach to unknown/dead session {}", sa.session_id);
                framed
                    .send(Message::SessionEnd(SessionEnd {
                        reason: format!("no such session: {}", sa.session_id),
                    }))
                    .await
                    .ok();
                return Ok(());
            };
            // A session id is unguessable (registry::new_session_id() draws
            // from /dev/urandom), but SessionList hands it out to its owner
            // and this codebase deliberately doesn't treat that alone as a
            // sufficient secret - see owned_by_caller's doc comment. Refuse
            // an attach from a different authenticated user rather than
            // silently letting them stream and inject input into someone
            // else's live desktop.
            if !owned_by_caller(&rec.owner, &run_as) {
                tracing::warn!(
                    "refusing SessionAttach for {}: not owned by the authenticated caller",
                    rec.session_id
                );
                framed
                    .send(Message::SessionEnd(SessionEnd {
                        reason: "not authorized to attach to this session".into(),
                    }))
                    .await
                    .ok();
                return Ok(());
            }
            tracing::info!("Attaching to session {} ({})", rec.session_id, rec.wayland_display);
            let ep = sa.encoder_preset.clone().filter(|s| !s.is_empty());
            let ex = sa.encoder_extra_params.clone().filter(|s| !s.is_empty());
            if let Some(ref preset) = ep {
                termland_compositor::validate_shell_command(preset)
                    .map_err(|e| anyhow::anyhow!("rejected encoder_preset: {e}"))?;
            }
            if let Some(ref extra) = ex {
                termland_compositor::validate_shell_command(extra)
                    .map_err(|e| anyhow::anyhow!("rejected encoder_extra_params: {e}"))?;
            }
            session_id = rec.session_id.clone();
            width = rec.width;
            height = rec.height;
            quality = sa.quality.clamp(1, 100);
            mode_desc = rec.mode.clone();
            mode = termland_compositor::SessionMode::Desktop; // unused on attach
            desktop_shell = None;
            encoder_preset = ep;
            encoder_crf = sa.encoder_crf;
            encoder_extra_params = ex;
            supported_codecs = if sa.supported_codecs.is_empty() {
                termland_protocol::VideoCodec::all_preferred()
            } else {
                sa.supported_codecs
            };
            supported_audio_codecs = if sa.supported_audio_codecs.is_empty() {
                termland_protocol::AudioCodec::legacy_default()
            } else {
                sa.supported_audio_codecs
            };
            audio = sa.audio;
            start_kind = StartKind::Attach {
                wayland_display: rec.wayland_display.clone(),
                compositor_pid: rec.compositor_pid,
                runtime_dir: rec.runtime_dir.clone(),
            };
            is_new = false;
        }
    }
    tracing::info!("Client supports codecs: {supported_codecs:?}");

    // WebTransport cannot carry Opus today: Q2's 5-byte audio header has no
    // `timestamp_us`, which `EncodedAudioChunk` requires. Refuse here so we
    // do not spawn Pulse capture and then drop the packets.
    let audio = if audio && matches!(media, crate::media::MediaConnection::WebTransport(_)) {
        tracing::warn!(
            "client requested audio, but WebTransport does not send it \
             (Q2 datagram header has no timestamp_us) — continuing without capture"
        );
        false
    } else {
        audio
    };

    // Spawn the compositor + capture loop on a blocking thread.
    let (frame_tx, mut frame_rx) = tokio::sync::mpsc::channel::<CapturedFrame>(2);
    let (display_tx, display_rx) = tokio::sync::oneshot::channel::<(String, u32, std::path::PathBuf)>();
    let (stop_tx, stop_rx) = tokio::sync::oneshot::channel::<()>();
    // Resize requests from the session loop → capture thread.
    let (resize_tx, resize_rx) = std::sync::mpsc::channel::<(u32, u32)>();

    // Shared cursor-mode flag. Client defaults to client-side cursor rendering.
    let overlay_cursor = Arc::new(AtomicBool::new(false));
    let overlay_cursor_capture = overlay_cursor.clone();

    let capture_codecs = supported_codecs.clone();
    // Cloned before the move into capture_thread below: also needed to
    // populate the registry record's `owner` field once the compositor is up.
    let record_owner = run_as.clone();
    // Only when audio was requested; the sink itself is created by
    // `audio_capture_thread` once the capture stream opens.
    let capture_audio_sink = audio.then(|| session_sink_name(&session_id));
    let capture_handle = std::thread::spawn(move || {
        capture_thread(width, height, quality, mode, desktop_shell,
                       encoder_preset, encoder_crf, encoder_extra_params,
                       capture_codecs, start_kind, capture_audio_sink, run_as,
                       overlay_cursor_capture, resize_rx, frame_tx, display_tx, stop_rx);
    });

    // Wait for the compositor to report its Wayland display name, pid, and
    // actual runtime dir (see `registry::SessionRecord::runtime_dir`'s doc
    // comment for why the last one can't just be assumed).
    let (wayland_display, compositor_pid, compositor_runtime_dir) = tokio::time::timeout(
        tokio::time::Duration::from_secs(15),
        async { display_rx.await },
    )
    .await
    .context("timeout waiting for compositor to start")?
    .context("capture thread died before compositor started")?;
    let compositor_runtime_dir = compositor_runtime_dir.to_string_lossy().into_owned();

    // For a freshly created session, record it in the registry so it can be
    // listed and resumed later. Resumes reuse the existing record.
    if is_new {
        let rec = crate::registry::SessionRecord {
            session_id: session_id.clone(),
            compositor_pid,
            wayland_display: wayland_display.clone(),
            mode: mode_desc.clone(),
            width,
            height,
            created_at_unix: crate::registry::now_unix(),
            audio,
            owner: record_owner,
            runtime_dir: compositor_runtime_dir.clone(),
        };
        if let Err(e) = crate::registry::write(&rec) {
            tracing::warn!("failed to write session registry record: {e}");
        }
    }

    // Spawn input injection thread connected to the same Wayland display
    let (input_tx, input_rx) = std::sync::mpsc::channel::<InputCommand>();
    let input_display = wayland_display.clone();
    let input_runtime_dir = compositor_runtime_dir.clone();
    let input_handle = std::thread::spawn(move || {
        input_thread(&input_display, &input_runtime_dir, width, height, input_rx);
    });

    // Wait for first frame
    let first_frame = tokio::time::timeout(
        tokio::time::Duration::from_secs(10),
        frame_rx.recv(),
    )
    .await
    .context("timeout waiting for first frame")?
    .context("capture thread died before producing a frame")?;

    let (first_w, first_h, first_codec) = match &first_frame {
        CapturedFrame::Video { width: w, height: h, codec, .. } => (*w as u32, *h as u32, *codec),
    };

    // Q2: open the video uni stream now, right as the encoder has produced
    // its first frame and the session is about to be declared ready.
    //
    // Not eagerly, back at connection-accept time: at that point we don't
    // yet know whether this connection will even reach a live session —
    // Hello/auth/SessionCreate can still fail, or the client might only be
    // listing/closing sessions — and an eagerly-opened stream would just be
    // wasted (and need explicit teardown) on every path that doesn't end in
    // streaming.
    //
    // Not any later than here either: the first video frame is sent
    // moments below, so the stream has to exist before that send or the
    // client's incoming-uni accept would race an as-yet-unopened stream
    // right at the most latency-sensitive moment (session startup).
    let mut media_planes = media.open_planes().await?;

    // Negotiate the audio codec: walk the client's preference order and take
    // the first one this server can actually encode. Done before SessionReady
    // so the choice can be announced alongside the video codec.
    //
    // `None` when the client asked for audio but shares no codec with us. The
    // session then runs silently, which is the only safe outcome — sending a
    // stream the client cannot decode looks like working audio that produces
    // no sound, and is far harder to diagnose than no audio at all.
    let audio_codec = if audio {
        let picked =
            termland_protocol::AudioCodec::negotiate(&supported_audio_codecs, SERVER_AUDIO_CODECS);
        if picked.is_none() {
            tracing::warn!(
                "  Audio: client advertised {supported_audio_codecs:?}, none of which this \
                 server can encode — continuing without audio",
            );
        }
        picked
    } else {
        None
    };

    // Send SessionReady, announcing the negotiated codecs so the client builds
    // the matching decoders up front instead of guessing from the stream.
    framed
        .send(Message::SessionReady(SessionReady {
            width: first_w,
            height: first_h,
            xkb_keymap: None,
            codec: Some(first_codec),
            audio_codec,
            session_id: session_id.clone(),
        }))
        .await
        .context("failed to send SessionReady")?;

    tracing::info!("Session {session_id} active");
    tracing::info!("  Compositor: headless wlroots on {wayland_display}");
    tracing::info!("  Resolution: {first_w}x{first_h}");
    tracing::info!("  Video codec: {first_codec}");
    tracing::info!(
        "  Transport: {}",
        media_planes
            .as_ref()
            .map(|p| p.transport_name())
            .unwrap_or("single control stream (TCP/TLS/SSH)")
    );

    // Optionally start audio capture if the client requested it and we agreed
    // on a codec.
    let (mut audio_rx, _audio_stop_tx, _audio_handle) = if let Some(codec) = audio_codec {
        let (atx, arx) = tokio::sync::mpsc::channel::<AudioChunk>(8);
        let (astop_tx, astop_rx) = tokio::sync::oneshot::channel::<()>();
        let sid = session_id.clone();
        let h = std::thread::spawn(move || {
            audio_capture_thread(&sid, atx, astop_rx);
        });
        tracing::info!(
            "  Audio encoder: {codec} {}Hz {} channel {}kbps",
            termland_codec::audio::SAMPLE_RATE,
            termland_codec::audio::CHANNELS,
            termland_codec::audio::BITRATE / 1000,
        );
        (Some(arx), Some(astop_tx), Some(h))
    } else if audio {
        (None, None, None)
    } else {
        tracing::info!("  Audio encoder: disabled (client did not request audio)");
        (None, None, None)
    };
    // Shared hashes: track content set by the client so the watch thread
    // doesn't echo it back. Separate trackers for plain text and file-list
    // (text/uri-list) content since they're independent clipboard mime types.
    let client_clip_hash = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let client_clip_hash_watch = client_clip_hash.clone();
    let client_clip_uri_hash = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let client_clip_uri_hash_watch = client_clip_uri_hash.clone();

    // Spawn clipboard watch thread
    let (mut clip_rx, _clip_stop_tx, _clip_handle) = {
        let (ctx, crx) = tokio::sync::mpsc::channel::<ClipboardEvent>(4);
        let (cstop_tx, cstop_rx) = tokio::sync::oneshot::channel::<()>();
        let clip_display = wayland_display.clone();
        let clip_runtime_dir = compositor_runtime_dir.clone();
        let h = std::thread::spawn(move || {
            clipboard_watch_thread(&clip_display, &clip_runtime_dir, ctx, cstop_rx, client_clip_hash_watch, client_clip_uri_hash_watch);
        });
        tracing::info!("  Clipboard: bidirectional via wl-clipboard (text + file paste)");
        (Some(crx), Some(cstop_tx), Some(h))
    };

    // Spawn cursor-shape watch thread. It only actually captures/sends while
    // the client is in client-side-cursor mode (overlay_cursor == false) -
    // when overlay_cursor is true the cursor is already baked into the video
    // frame by screencopy, so sending it again here would be redundant
    // bandwidth. The thread itself always runs so it can react immediately
    // when the client flips CursorMode mid-session.
    let overlay_cursor_watch = overlay_cursor.clone();
    let (mut cursor_rx, _cursor_stop_tx, _cursor_handle) = {
        let (ctx, crx) = tokio::sync::mpsc::channel::<CursorUpdate>(4);
        let (cstop_tx, cstop_rx) = tokio::sync::oneshot::channel::<()>();
        let cursor_display = wayland_display.clone();
        let cursor_runtime_dir = compositor_runtime_dir.clone();
        let h = std::thread::spawn(move || {
            cursor_watch_thread(&cursor_display, &cursor_runtime_dir, ctx, cstop_rx, overlay_cursor_watch);
        });
        tracing::info!("  Cursor shape sync: client-side-cursor mode only (ext-image-copy-capture-v1)");
        (Some(crx), Some(cstop_tx), Some(h))
    };

    // Send the first frame
    match media_planes.as_mut() {
        Some(planes) => {
            send_video_frame_split(planes, &first_frame)
                .await
                .context("failed to send first frame (split video stream)")?;
        }
        None => {
            let first_msg = frame_to_message(&first_frame);
            framed.send(first_msg).await.context("failed to send first frame")?;
        }
    }

    let mut frame_num: u64 = 1;
    let mut current_width = first_w;
    let mut current_height = first_h;

    // Window enumeration for the client's task list (issue #1). Optional in
    // exactly the way OutputResizer is: a compositor without
    // zwlr_foreign_toplevel_management_v1 simply means no window list, never a
    // failed session.
    let mut toplevels = match termland_compositor::ToplevelWatcher::connect(
        &wayland_display,
        std::path::Path::new(&compositor_runtime_dir),
    ) {
        Ok(w) => Some(w),
        Err(e) => {
            tracing::info!("Window enumeration unavailable: {e}");
            None
        }
    };
    let mut last_windows: Vec<termland_protocol::WindowInfo> = Vec::new();
    // Polled rather than event-driven: the Wayland connection is blocking, and
    // half a second is well inside what a task list needs while costing
    // nothing when nothing changes (the list is only sent on change).
    let mut window_poll = tokio::time::interval(tokio::time::Duration::from_millis(500));

    // How this streaming session ends. Default: the client detached (the
    // compositor keeps running and the session stays resumable).
    let mut outcome = SessionOutcome::Detached;

    // Main event loop
    loop {
        tokio::select! {
            // Window list, sent only when it differs from what the client was
            // last told — a desktop sitting idle produces no traffic here.
            _ = window_poll.tick(), if toplevels.is_some() => {
                let watcher = toplevels.as_mut().expect("guarded by the branch condition");
                match watcher.poll() {
                    Ok(list) => {
                        let windows: Vec<_> = list
                            .into_iter()
                            .map(|w| termland_protocol::WindowInfo {
                                id: w.id,
                                title: w.title,
                                app_id: w.app_id,
                                minimized: w.minimized,
                                maximized: w.maximized,
                                fullscreen: w.fullscreen,
                                activated: w.activated,
                            })
                            .collect();
                        if windows != last_windows {
                            last_windows.clone_from(&windows);
                            if let Err(e) = framed
                                .send(Message::WindowList(termland_protocol::WindowList { windows }))
                                .await
                            {
                                tracing::error!("Failed to send window list: {e}");
                                break;
                            }
                        }
                    }
                    Err(e) => {
                        // Losing this connection must not take the session
                        // with it; drop the feature and keep streaming.
                        tracing::warn!("Window enumeration stopped: {e}");
                        toplevels = None;
                    }
                }
            }

            frame = frame_rx.recv() => {
                let Some(frame) = frame else {
                    // Capture thread ended because the compositor exited.
                    tracing::info!("Compositor exited, ending session");
                    let _ = framed.send(Message::SessionEnd(SessionEnd {
                        reason: "compositor exited".into(),
                    })).await;
                    outcome = SessionOutcome::Gone;
                    break;
                };

                let (fw, fh) = match &frame {
                    CapturedFrame::Video { width: w, height: h, .. } => (*w as u32, *h as u32),
                };
                current_width = fw;
                current_height = fh;

                match media_planes.as_mut() {
                    Some(planes) => {
                        if let Err(e) = send_video_frame_split(planes, &frame).await {
                            tracing::error!("Failed to send frame (split video stream): {e}");
                            break;
                        }
                    }
                    None => {
                        let msg = frame_to_message(&frame);
                        if let Err(e) = framed.send(msg).await {
                            tracing::error!("Failed to send frame: {e}");
                            break;
                        }
                    }
                }

                frame_num += 1;
                if frame_num % 30 == 0 {
                    tracing::debug!("Sent frame {frame_num}");
                }
            }

            audio = async {
                match audio_rx.as_mut() {
                    Some(rx) => rx.recv().await,
                    None => std::future::pending().await,
                }
            } => {
                if let Some(chunk) = audio {
                    match media_planes.as_ref() {
                        Some(planes) => planes.send_audio(&chunk),
                        None => {
                            if let Err(e) = framed.send(Message::AudioChunk(chunk)).await {
                                tracing::error!("Failed to send audio: {e}");
                                break;
                            }
                        }
                    }
                }
            }

            clip = async {
                match clip_rx.as_mut() {
                    Some(rx) => rx.recv().await,
                    None => std::future::pending().await,
                }
            } => {
                if let Some(event) = clip {
                    let msg = match event {
                        ClipboardEvent::Text(payload) => Message::ClipboardData(payload),
                        ClipboardEvent::Files(payload) => Message::FileTransferData(payload),
                    };
                    if let Err(e) = framed.send(msg).await {
                        tracing::error!("Failed to send clipboard: {e}");
                        break;
                    }
                }
            }

            cursor = async {
                match cursor_rx.as_mut() {
                    Some(rx) => rx.recv().await,
                    None => std::future::pending().await,
                }
            } => {
                if let Some(update) = cursor {
                    if let Err(e) = framed.send(Message::CursorUpdate(update)).await {
                        tracing::error!("Failed to send cursor update: {e}");
                        break;
                    }
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
                            Message::SessionResize(sr) => {
                                let w = sr.width.clamp(320, 7680);
                                let h = sr.height.clamp(240, 4320);
                                tracing::info!("Client requested resize to {w}x{h}");
                                let _ = resize_tx.send((w, h));
                            }
                            Message::KeyEvent(ke) => {
                                let pressed = ke.state == termland_protocol::input::KeyState::Pressed;
                                tracing::debug!("Key received: scancode={} pressed={pressed}", ke.scancode);
                                let _ = input_tx.send(InputCommand::Key {
                                    scancode: ke.scancode,
                                    pressed,
                                });
                            }
                            Message::TextInput(ti) => {
                                tracing::debug!("Text received: {} chars", ti.text.chars().count());
                                let _ = input_tx.send(InputCommand::Text(ti.text));
                            }
                            Message::MouseMove(mm) => {
                                let _ = input_tx.send(InputCommand::PointerMove {
                                    x: mm.x,
                                    y: mm.y,
                                    width: current_width,
                                    height: current_height,
                                    absolute: mm.absolute,
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
                            Message::SessionClose(sc) if sc.session_id == session_id || sc.session_id.is_empty() => {
                                tracing::info!("Client closed the active session");
                                outcome = SessionOutcome::Closed;
                                break;
                            }
                            Message::ClipboardSend(cp) => {
                                // Record hash so watch thread doesn't echo it back
                                let hash = hash_bytes(&cp.data);
                                client_clip_hash.store(hash, Ordering::Relaxed);
                                let wd = wayland_display.clone();
                                let rd = compositor_runtime_dir.clone();
                                tokio::task::spawn_blocking(move || {
                                    set_remote_clipboard(&wd, &rd, &cp.data, &cp.mime_type);
                                });
                            }
                            Message::FileTransferSend(payload) => {
                                // Client copied files locally and is sending them to us -
                                // write them into this session's scratch dir and point the
                                // remote (server-side) desktop clipboard at them, so a
                                // Ctrl+V inside the remote session pastes the real files.
                                let wd = wayland_display.clone();
                                let rd = compositor_runtime_dir.clone();
                                let sid = session_id.clone();
                                let uri_hash_flag = client_clip_uri_hash.clone();
                                tokio::task::spawn_blocking(move || {
                                    receive_file_transfer(&sid, &wd, &rd, payload, &uri_hash_flag);
                                });
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
    if let Some(astop) = _audio_stop_tx {
        let _ = astop.send(());
    }
    if let Some(cstop) = _clip_stop_tx {
        let _ = cstop.send(());
    }
    if let Some(cstop) = _cursor_stop_tx {
        let _ = cstop.send(());
    }
    let _ = capture_handle.join();
    let _ = input_handle.join();
    if let Some(ah) = _audio_handle {
        let _ = ah.join();
    }
    if let Some(ch) = _clip_handle {
        let _ = ch.join();
    }
    if let Some(ch) = _cursor_handle {
        let _ = ch.join();
    }

    match outcome {
        SessionOutcome::Detached => {
            // The compositor keeps running; the session stays resumable.
            // Deliberately do NOT clean up clipboard_files_dir here: the
            // remote desktop's clipboard may still hold a text/uri-list
            // pointing at those scratch files (pasted later, well after
            // detach) - only clean up once the session is actually gone.
            tracing::info!("Session {session_id} detached (compositor still running, resumable)");
        }
        SessionOutcome::Closed => {
            crate::registry::close(&session_id);
            crate::registry::clipboard_files_cleanup(&session_id);
            tracing::info!("Session {session_id} closed");
        }
        SessionOutcome::Gone => {
            crate::registry::remove(&session_id);
            crate::registry::clipboard_files_cleanup(&session_id);
            tracing::info!("Session {session_id} ended (compositor gone)");
        }
    }
    Ok(())
}

/// How a streaming session ended, deciding the fate of its compositor.
enum SessionOutcome {
    /// Client went away; leave the compositor running (persist, resumable).
    Detached,
    /// Client explicitly closed the session; terminate the compositor.
    Closed,
    /// The compositor exited on its own; drop the (now dead) record.
    Gone,
}

/// Encoder tuning collected once per session from the client.
struct EncoderTuning {
    bitrate_kbps: u32,
    preset: Option<String>,
    crf: Option<u8>,
    extra_svt_params: Option<String>,
}

impl EncoderTuning {
    fn build_config(&self, width: u32, height: u32) -> termland_codec::EncoderConfig {
        termland_codec::EncoderConfig {
            width,
            height,
            fps: 30,
            bitrate_kbps: self.bitrate_kbps,
            keyframe_interval: 30,
            preset: self.preset.clone(),
            crf: self.crf,
            extra_svt_params: self.extra_svt_params.clone(),
        }
    }
}

/// Build a fresh video encoder for the given dimensions + tuning, restricted
/// to codecs the client advertised it can decode.
/// Panics if no encoder is available (no supported codec could initialize).
fn init_encoder(
    tuning: &EncoderTuning,
    width: u32,
    height: u32,
    supported_codecs: &[termland_protocol::VideoCodec],
) -> Box<dyn termland_codec::VideoEncoder> {
    let config = tuning.build_config(width, height);
    match termland_codec::probe_best_encoder(&config, supported_codecs) {
        Ok(enc) => {
            let backend = enc.backend();
            tracing::info!(
                "Video encoder ready for {width}x{height}: {backend} (codec {})",
                backend.codec()
            );
            enc
        }
        Err(e) => {
            panic!("No video encoder available for client-supported codecs {supported_codecs:?}: {e}");
        }
    }
}

/// Capture thread: creates compositor, captures frames, sends display name back.
#[allow(clippy::too_many_arguments)]
/// How a capture thread obtains its compositor: spawn a fresh detached one, or
/// attach to an already-running session's Wayland display (resume).
enum StartKind {
    Create { log_path: std::path::PathBuf },
    Attach { wayland_display: String, compositor_pid: u32, runtime_dir: String },
}

#[allow(clippy::too_many_arguments)]
fn capture_thread(
    width: u32,
    height: u32,
    quality: u8,
    mode: termland_compositor::SessionMode,
    desktop_shell: Option<String>,
    encoder_preset: Option<String>,
    encoder_crf: Option<u8>,
    encoder_extra_params: Option<String>,
    supported_codecs: Vec<termland_protocol::VideoCodec>,
    start_kind: StartKind,
    // Sink that session applications should play to, exported as
    // `PULSE_SINK`. `None` when the client did not request audio.
    audio_sink: Option<String>,
    // The PAM-authenticated username to isolate a freshly-created session's
    // compositor into (session isolation), or `None` when the server is
    // running without `--auth`. Only consulted for `StartKind::Create` - an
    // `Attach` reconnects to a compositor that already runs as whatever user
    // it was originally spawned with, so there's nothing to (re-)drop here.
    run_as: Option<String>,
    overlay_cursor: Arc<AtomicBool>,
    resize_rx: std::sync::mpsc::Receiver<(u32, u32)>,
    frame_tx: tokio::sync::mpsc::Sender<CapturedFrame>,
    display_tx: tokio::sync::oneshot::Sender<(String, u32, std::path::PathBuf)>,
    mut stop_rx: tokio::sync::oneshot::Receiver<()>,
) {
    let config = termland_compositor::CompositorConfig {
        width, height, mode, desktop_shell, audio_sink,
    };
    let (mut compositor, comp_pid) = match start_kind {
        StartKind::Create { log_path } => {
            match termland_compositor::Compositor::create(config, &log_path, run_as.as_deref()) {
                Ok(x) => x,
                Err(e) => {
                    tracing::error!("Failed to create compositor: {e}");
                    return;
                }
            }
        }
        StartKind::Attach { wayland_display, compositor_pid, runtime_dir } => {
            match termland_compositor::Compositor::attach(config, wayland_display, std::path::PathBuf::from(runtime_dir)) {
                Ok(c) => (c, compositor_pid),
                Err(e) => {
                    tracing::error!("Failed to attach to compositor: {e}");
                    return;
                }
            }
        }
    };

    // Send the Wayland display name + compositor pid + actual runtime dir
    // back to the session task (see registry::SessionRecord::runtime_dir's
    // doc comment for why the caller can't just assume its own).
    let _ = display_tx.send((
        compositor.wayland_display().to_string(),
        comp_pid,
        compositor.runtime_dir().to_path_buf(),
    ));

    // Map quality 1-100 to bitrate. quality 100 → 15 Mbps, 50 → 7.5 Mbps, 1 → 500 kbps
    let bitrate_kbps = (quality as u32 * 150).max(500);
    tracing::info!("Video quality={quality} → bitrate={bitrate_kbps}kbps");

    let tuning = EncoderTuning {
        bitrate_kbps,
        preset: encoder_preset,
        crf: encoder_crf,
        extra_svt_params: encoder_extra_params,
    };

    let mut video_encoder = init_encoder(&tuning, width, height, &supported_codecs);
    let mut encoder_codec = video_encoder.backend().codec();
    let mut encoder_dims = (width, height);

    let frame_duration = std::time::Duration::from_millis(33);

    loop {
        if stop_rx.try_recv().is_ok() {
            tracing::info!("Capture thread received stop signal");
            break;
        }

        if !crate::registry::pid_alive(comp_pid) {
            tracing::info!("Compositor process (pid {comp_pid}) exited, ending capture");
            break;
        }

        // Drain pending resize requests - only act on the most recent one.
        let mut latest_resize: Option<(u32, u32)> = None;
        while let Ok(sz) = resize_rx.try_recv() {
            latest_resize = Some(sz);
        }
        if let Some((rw, rh)) = latest_resize {
            if (rw, rh) != encoder_dims {
                match compositor.resize(rw, rh) {
                    Ok(()) => tracing::info!("Compositor output resized to {rw}x{rh}"),
                    Err(e) => tracing::warn!("Compositor resize failed: {e}"),
                }
                // Encoder reinit happens lazily when we detect new frame dims below.
            }
        }

        let start = std::time::Instant::now();

        match compositor.capture_frame(overlay_cursor.load(Ordering::Relaxed)) {
            Ok((w, h, rgba)) => {
                // Rebuild the encoder if the captured frame size changed.
                if (w, h) != encoder_dims {
                    tracing::info!("Capture dims changed {}x{} → {w}x{h}, reinitializing encoder",
                        encoder_dims.0, encoder_dims.1);
                    // Flush the old encoder before dropping it. This sends
                    // EOS so the encoder can properly close.
                    let _ = video_encoder.flush();
                    video_encoder = init_encoder(&tuning, w, h, &supported_codecs);
                    encoder_codec = video_encoder.backend().codec();
                    encoder_dims = (w, h);
                }

                let timestamp_us = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_micros() as u64;

                // Force a keyframe when dimensions change
                let force_keyframe = (w, h) != encoder_dims;

                let frames: Vec<CapturedFrame> = match video_encoder.encode_frame(&rgba, timestamp_us, force_keyframe) {
                    Ok(packets) if !packets.is_empty() => {
                        packets.into_iter().map(|p| {
                            let keyframe_str = if p.keyframe { "keyframe" } else { "frame" };
                            tracing::debug!("{keyframe_str}: {} bytes", p.data.len());
                            CapturedFrame::Video {
                                data: p.data,
                                keyframe: p.keyframe,
                                width: w as u16,
                                height: h as u16,
                                timestamp_us,
                                codec: encoder_codec,
                            }
                        }).collect()
                    }
                    Ok(_) => {
                        // Encoder buffering - send nothing this iteration
                        continue;
                    }
                    Err(e) => {
                        tracing::warn!("Video encode failed: {e}");
                        // Continue capturing - the encoder may recover
                        continue;
                    }
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
                if !crate::registry::pid_alive(comp_pid) {
                    tracing::info!("Compositor process (pid {comp_pid}) exited, ending capture");
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
    // See clipboard_watch_thread's `runtime_dir` doc comment.
    runtime_dir: &str,
    _width: u32,
    _height: u32,
    rx: std::sync::mpsc::Receiver<InputCommand>,
) {
    // Give cage a moment to be fully ready for another client connection
    std::thread::sleep(std::time::Duration::from_millis(300));

    let runtime_path = std::path::Path::new(runtime_dir);
    let mut injector = match termland_compositor::InputInjector::connect(display_name, runtime_path) {
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
            InputCommand::Text(text) => {
                tracing::debug!("Input thread: injecting text ({} chars)", text.chars().count());
                if let Err(e) = injector.text(&text) {
                    tracing::error!("Text injection failed: {e}");
                }
            }
            InputCommand::PointerMove { x, y, width, height, absolute } => {
                if absolute {
                    injector.pointer_motion_absolute(x, y, width, height);
                } else {
                    injector.pointer_motion_relative(x, y);
                }
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

/// Audio capture thread: creates a PulseAudio null sink for the session,
/// captures from its monitor, encodes Opus, and sends AudioChunk packets.
fn audio_capture_thread(
    session_id: &str,
    audio_tx: tokio::sync::mpsc::Sender<AudioChunk>,
    mut stop_rx: tokio::sync::oneshot::Receiver<()>,
) {
    let sink_name = session_sink_name(session_id);
    let monitor_source = format!("{sink_name}.monitor");

    // Load a null sink so apps in this session have somewhere to output audio.
    let load_result = std::process::Command::new("pactl")
        .args(["load-module", "module-null-sink",
               &format!("sink_name={sink_name}"),
               &format!("sink_properties=device.description=\"Termland\\ {session_id}\"")])
        .output();

    let module_id = match load_result {
        Ok(out) if out.status.success() => {
            let id = String::from_utf8_lossy(&out.stdout).trim().to_string();
            tracing::info!("Loaded PA null sink '{sink_name}' (module {id})");
            Some(id)
        }
        Ok(out) => {
            tracing::warn!("pactl load-module failed: {}", String::from_utf8_lossy(&out.stderr).trim());
            return;
        }
        Err(e) => {
            tracing::warn!("Could not run pactl: {e} — audio disabled");
            return;
        }
    };

    // NOTE: deliberately no `pactl set-default-sink` here. The default sink is
    // global to the user, so pointing it at this session's null sink silences
    // the local desktop for as long as the session runs, and leaves it aimed
    // at a dead sink if the server exits without unloading the module (a kill
    // -9, a crash, or a test harness reaping the child). Session applications
    // are routed with `PULSE_SINK` on the compositor process instead — see
    // `CompositorConfig::audio_sink` — which affects only that process tree.

    // Open a PulseAudio recording stream from the monitor source.
    let spec = pulse::sample::Spec {
        format: pulse::sample::Format::S16le,
        rate: termland_codec::audio::SAMPLE_RATE,
        channels: termland_codec::audio::CHANNELS,
    };

    let pa = match psimple::Simple::new(
        None,
        "termland-server",
        pulse::stream::Direction::Record,
        Some(&monitor_source),
        "session audio capture",
        &spec,
        None,
        None,
    ) {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!("PA record stream failed for '{monitor_source}': {e}");
            cleanup_null_sink(module_id.as_deref());
            return;
        }
    };

    let mut opus_enc = match termland_codec::OpusEncoder::new() {
        Ok(e) => e,
        Err(e) => {
            tracing::error!("Opus encoder init: {e}");
            cleanup_null_sink(module_id.as_deref());
            return;
        }
    };

    tracing::info!("Audio capture started from '{monitor_source}'");

    let frame_samples = termland_codec::audio::FRAME_SIZE * termland_codec::audio::CHANNELS as usize;
    let mut byte_buf = vec![0u8; frame_samples * 2];

    loop {
        if stop_rx.try_recv().is_ok() {
            break;
        }

        if let Err(e) = pa.read(&mut byte_buf) {
            tracing::warn!("PA read error: {e}");
            break;
        }

        let pcm_buf: &[i16] = bytemuck::cast_slice(&byte_buf);
        let is_silence = pcm_buf.iter().all(|&s| s == 0);
        if is_silence {
            continue;
        }

        match opus_enc.encode(&pcm_buf) {
            Ok(opus_data) => {
                let chunk = AudioChunk {
                    timestamp_us: std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_micros() as u64,
                    sample_rate: termland_codec::audio::SAMPLE_RATE,
                    channels: termland_codec::audio::CHANNELS,
                    data: opus_data,
                };
                if audio_tx.blocking_send(chunk).is_err() {
                    break;
                }
            }
            Err(e) => tracing::warn!("Opus encode: {e}"),
        }
    }

    cleanup_null_sink(module_id.as_deref());
    tracing::info!("Audio capture thread exiting");
}

fn cleanup_null_sink(module_id: Option<&str>) {
    if let Some(id) = module_id {
        let _ = std::process::Command::new("pactl")
            .args(["unload-module", id])
            .output();
    }
}

/// What the clipboard watch thread forwards to the session loop: either
/// plain-text clipboard content (unchanged, existing behavior) or a batch of
/// files read from a `text/uri-list` clipboard entry (new). Kept as one
/// channel/enum rather than two channels so the session loop's `select!`
/// doesn't need a third branch just for this.
enum ClipboardEvent {
    Text(ClipboardPayload),
    Files(FileTransferPayload),
}

/// Hash arbitrary bytes with the same (non-cryptographic, "detect content
/// changed" only) hasher used throughout this file's watch threads.
fn hash_bytes(data: &[u8]) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    data.hash(&mut h);
    h.finish()
}

/// Clipboard poll thread: periodically runs `wl-paste` on the session's
/// Wayland display and sends clipboard changes to the client. Polls two
/// independent clipboard mime types each cycle - `text/plain` (original
/// behavior) and `text/uri-list` (clipboard file-paste, new) - each with its
/// own hash-diff so neither one's presence/absence affects the other.
fn clipboard_watch_thread(
    wayland_display: &str,
    // The compositor's ACTUAL runtime dir - see
    // registry::SessionRecord::runtime_dir's doc comment. Every wl-paste/
    // wl-copy invocation in this function must set this explicitly rather
    // than inheriting this (server) process's own XDG_RUNTIME_DIR, or an
    // isolated session's clipboard subprocess would look for the Wayland
    // socket in the wrong directory and simply never find it.
    runtime_dir: &str,
    clip_tx: tokio::sync::mpsc::Sender<ClipboardEvent>,
    mut stop_rx: tokio::sync::oneshot::Receiver<()>,
    client_set_hash: Arc<std::sync::atomic::AtomicU64>,
    client_set_uri_hash: Arc<std::sync::atomic::AtomicU64>,
) {
    // Verify wl-paste is available
    if std::process::Command::new("wl-paste")
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_err()
    {
        tracing::warn!("wl-paste not available — clipboard sync disabled");
        return;
    }

    tracing::info!("Clipboard watch started on {wayland_display}");

    let mut last_hash: u64 = 0;
    let mut last_uri_hash: u64 = 0;

    loop {
        if stop_rx.try_recv().is_ok() {
            break;
        }

        std::thread::sleep(std::time::Duration::from_millis(500));

        // --- text/plain path (unchanged from before file-transfer support) ---
        let output = match std::process::Command::new("wl-paste")
            .arg("--no-newline")
            .env("WAYLAND_DISPLAY", wayland_display)
            .env("XDG_RUNTIME_DIR", runtime_dir)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .output()
        {
            Ok(o) if o.status.success() => o.stdout,
            _ => Vec::new(),
        };

        if !output.is_empty() {
            let hash = hash_bytes(&output);
            if hash != last_hash {
                // Skip content that was set by the client (avoid echo loop)
                if hash == client_set_hash.load(std::sync::atomic::Ordering::Relaxed) {
                    last_hash = hash;
                } else {
                    last_hash = hash;
                    tracing::debug!("Clipboard changed ({} bytes), sending to client", output.len());
                    let payload = ClipboardPayload {
                        mime_type: "text/plain".into(),
                        data: output,
                    };
                    if clip_tx.blocking_send(ClipboardEvent::Text(payload)).is_err() {
                        break;
                    }
                }
            }
        }

        // --- text/uri-list path (clipboard file-paste) ---
        // Only bother invoking `wl-paste --type text/uri-list` at all when
        // that mime type is actually on the clipboard right now - most
        // clipboard changes are plain text, and a file-manager "Copy" is
        // comparatively rare, so this avoids a second subprocess spawn every
        // 500ms in the common case.
        if clipboard_has_uri_list(wayland_display, runtime_dir) {
            let uri_output = std::process::Command::new("wl-paste")
                .args(["--type", "text/uri-list", "--no-newline"])
                .env("WAYLAND_DISPLAY", wayland_display)
                .env("XDG_RUNTIME_DIR", runtime_dir)
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::null())
                .output()
                .ok()
                .filter(|o| o.status.success())
                .map(|o| o.stdout);

            if let Some(uri_bytes) = uri_output.filter(|b| !b.is_empty()) {
                let hash = hash_bytes(&uri_bytes);
                if hash != last_uri_hash {
                    if hash == client_set_uri_hash.load(std::sync::atomic::Ordering::Relaxed) {
                        last_uri_hash = hash;
                    } else {
                        last_uri_hash = hash;
                        let text = String::from_utf8_lossy(&uri_bytes);
                        match read_files_from_uri_list(&text) {
                            Some(payload) => {
                                tracing::debug!(
                                    "Clipboard file list changed ({} file(s)), sending to client",
                                    payload.files.len()
                                );
                                if clip_tx.blocking_send(ClipboardEvent::Files(payload)).is_err() {
                                    break;
                                }
                            }
                            None => {
                                // Either no readable files or the batch was
                                // over the size cap (already logged inside
                                // read_files_from_uri_list) - nothing to send.
                            }
                        }
                    }
                }
            }
        } else if last_uri_hash != 0 {
            // The uri-list entry disappeared (clipboard replaced with
            // something else) - reset so a later re-copy of the same file
            // set is detected as "changed" again instead of being treated
            // as a no-op repeat of stale state.
            last_uri_hash = 0;
        }
    }

    tracing::info!("Clipboard watch thread exiting");
}

/// Does the session's clipboard currently offer a `text/uri-list` entry?
/// Cheap presence check via `wl-paste --list-types`, used to skip the (more
/// expensive, potentially large) `wl-paste --type text/uri-list` read on
/// every poll cycle when there's nothing to read.
fn clipboard_has_uri_list(wayland_display: &str, runtime_dir: &str) -> bool {
    let output = std::process::Command::new("wl-paste")
        .arg("--list-types")
        .env("WAYLAND_DISPLAY", wayland_display)
        .env("XDG_RUNTIME_DIR", runtime_dir)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .output();
    match output {
        Ok(o) if o.status.success() => {
            String::from_utf8_lossy(&o.stdout).lines().any(|l| l.trim() == "text/uri-list")
        }
        _ => false,
    }
}

/// Read the files listed in a `text/uri-list` clipboard payload from the
/// local filesystem and build a [`FileTransferPayload`], subject to
/// [`termland_protocol::MAX_FILE_TRANSFER_BYTES`].
///
/// - A `file://` entry whose file can't be read (permissions, deleted since
///   the clipboard was set, a non-regular-file path, etc.) is skipped with a
///   warning - one bad file in the selection shouldn't sink an otherwise-good
///   batch.
/// - If the *total* size of the successfully-read files would exceed the
///   cap, the whole batch is rejected (clear warning, nothing returned)
///   rather than silently truncated - see `MAX_FILE_TRANSFER_BYTES`'s doc
///   comment for why this MVP doesn't chunk large transfers across messages.
fn read_files_from_uri_list(text: &str) -> Option<FileTransferPayload> {
    let paths = termland_protocol::parse_uri_list(text);
    if paths.is_empty() {
        return None;
    }

    let mut files = Vec::new();
    let mut total: u64 = 0;
    for path in &paths {
        let data = match std::fs::read(path) {
            Ok(d) => d,
            Err(e) => {
                tracing::warn!("clipboard file transfer: skipping unreadable {}: {e}", path.display());
                continue;
            }
        };
        total += data.len() as u64;
        if total > termland_protocol::MAX_FILE_TRANSFER_BYTES {
            tracing::warn!(
                "clipboard file transfer: total size of {} file(s) exceeds the {} MiB cap, rejecting the whole batch",
                paths.len(),
                termland_protocol::MAX_FILE_TRANSFER_BYTES / (1024 * 1024),
            );
            return None;
        }
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "file".to_string());
        files.push(FileEntry { name, data });
    }

    if files.is_empty() { None } else { Some(FileTransferPayload { files }) }
}

/// A short, time-keyed directory name for one clipboard file transfer's
/// scratch subdirectory (see `receive_file_transfer`) - just needs to be
/// unique across successive transfers within a session, not globally unique
/// or unguessable.
fn transfer_subdir_name() -> String {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis().to_string())
        .unwrap_or_else(|_| "0".to_string())
}

/// Handle an inbound `Message::FileTransferSend` from the client: write the
/// files into this session's scratch directory
/// (`registry::clipboard_files_dir`) and point the remote (server-side)
/// desktop's clipboard at them via `wl-copy --type text/uri-list`, so a
/// `Ctrl+V` inside the remote session pastes the real files.
///
/// `uri_hash_flag` is set to the hash of the exact bytes handed to
/// `wl-copy`'s stdin so `clipboard_watch_thread` recognizes this as
/// self-set content and doesn't immediately echo it back to the client as if
/// it were a fresh local clipboard change.
fn receive_file_transfer(
    session_id: &str,
    wayland_display: &str,
    // See clipboard_watch_thread's `runtime_dir` doc comment.
    runtime_dir: &str,
    payload: FileTransferPayload,
    uri_hash_flag: &Arc<std::sync::atomic::AtomicU64>,
) {
    if payload.files.is_empty() {
        return;
    }

    // Defense in depth: the sender is expected to enforce
    // MAX_FILE_TRANSFER_BYTES before sending, but a modified/misbehaving
    // client could send more - re-check here rather than trust the wire.
    let total: u64 = payload.files.iter().map(|f| f.data.len() as u64).sum();
    if total > termland_protocol::MAX_FILE_TRANSFER_BYTES {
        tracing::warn!(
            "rejecting FileTransferSend: {} bytes exceeds the {} MiB cap",
            total,
            termland_protocol::MAX_FILE_TRANSFER_BYTES / (1024 * 1024),
        );
        return;
    }

    // A fresh subdirectory per transfer (keyed by time) rather than writing
    // straight into the session's clipboard_files_dir - otherwise a second
    // paste reusing a filename from an earlier one would overwrite it while
    // the remote clipboard might still reference the earlier path.
    let dir = crate::registry::clipboard_files_dir(session_id).join(transfer_subdir_name());
    if let Err(e) = std::fs::create_dir_all(&dir) {
        tracing::warn!("clipboard file transfer: failed to create scratch dir {}: {e}", dir.display());
        return;
    }

    let mut written_paths = Vec::new();
    for entry in &payload.files {
        // SECURITY: entry.name arrives from the client over the wire - a
        // malicious or buggy client could send "../../etc/passwd" or an
        // absolute path aiming to write outside `dir`. sanitize_filename
        // only ever accepts a bare basename; anything else is dropped (with
        // a warning) rather than partially rewritten.
        let Some(safe_name) = termland_protocol::sanitize_filename(&entry.name) else {
            tracing::warn!("clipboard file transfer: rejecting unsafe filename {:?}", entry.name);
            continue;
        };
        let path = dir.join(&safe_name);
        if let Err(e) = std::fs::write(&path, &entry.data) {
            tracing::warn!("clipboard file transfer: failed to write {}: {e}", path.display());
            continue;
        }
        written_paths.push(path);
    }

    if written_paths.is_empty() {
        tracing::warn!("clipboard file transfer: no files were written (all rejected/failed)");
        return;
    }

    let uri_list = termland_protocol::build_uri_list(&written_paths);
    uri_hash_flag.store(hash_bytes(uri_list.as_bytes()), Ordering::Relaxed);

    use std::io::Write;
    let mut child = match std::process::Command::new("wl-copy")
        .args(["--type", "text/uri-list"])
        .env("WAYLAND_DISPLAY", wayland_display)
        .env("XDG_RUNTIME_DIR", runtime_dir)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!("clipboard file transfer: wl-copy failed: {e}");
            return;
        }
    };
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(uri_list.as_bytes());
    }
    let _ = child.wait();

    tracing::info!(
        "Clipboard file transfer: wrote {} file(s) to {} and set remote clipboard",
        written_paths.len(),
        dir.display()
    );
}

/// Poll interval for cursor shape capture while client-side-cursor mode is
/// active. Much faster than the clipboard's 500ms poll (cursor shape changes
/// need to feel immediate - hovering onto a text field or a window edge)
/// but still far cheaper than doing it every video frame.
const CURSOR_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(50);

/// Idle interval used while overlay_cursor is true (server-side/in-frame
/// cursor mode), so this thread isn't busy-looping doing nothing useful.
const CURSOR_IDLE_INTERVAL: std::time::Duration = std::time::Duration::from_millis(200);

/// Hash a captured cursor's shape (dimensions, hotspot, visibility, pixels)
/// so [`cursor_watch_thread`] can tell "same cursor as last time" from
/// "actually changed" without resending the bitmap on every poll. Mirrors the
/// clipboard watch thread's hash-and-compare pattern above.
fn cursor_hash(width: u32, height: u32, hotspot_x: i32, hotspot_y: i32, visible: bool, rgba: &[u8]) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    width.hash(&mut h);
    height.hash(&mut h);
    hotspot_x.hash(&mut h);
    hotspot_y.hash(&mut h);
    visible.hash(&mut h);
    rgba.hash(&mut h);
    h.finish()
}

/// Cursor-shape watch thread: connects a dedicated ext-image-copy-capture-v1
/// pointer cursor session on the session's Wayland display and, while the
/// client wants client-side cursor rendering (`overlay_cursor == false`),
/// polls it and forwards changes as `Message::CursorUpdate` (diffed by hash,
/// same principle as `clipboard_watch_thread`). When `overlay_cursor == true`
/// the cursor is already baked into the video frame by screencopy, so this
/// just idles instead of capturing - sending it too would be redundant
/// bandwidth for a cursor the client isn't even drawing itself.
///
/// If the compositor doesn't support the (staging) protocol this needs, the
/// connect fails once, we log it, and return - the client keeps whatever
/// generic placeholder it draws locally instead of a real shape.
fn cursor_watch_thread(
    wayland_display: &str,
    // See clipboard_watch_thread's `runtime_dir` doc comment - this thread
    // opens its OWN independent Wayland connection (separate from the one
    // `Compositor` itself holds), so it needs the compositor's actual
    // runtime dir passed in explicitly too.
    runtime_dir: &str,
    cursor_tx: tokio::sync::mpsc::Sender<CursorUpdate>,
    mut stop_rx: tokio::sync::oneshot::Receiver<()>,
    overlay_cursor: Arc<AtomicBool>,
) {
    let runtime_path = std::path::Path::new(runtime_dir);
    let mut capturer = match termland_compositor::CursorCapturer::connect(wayland_display, runtime_path) {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(
                "Cursor shape capture unavailable ({e}) - client-side cursor \
                 mode will fall back to its generic placeholder"
            );
            return;
        }
    };

    tracing::info!("Cursor shape watch started on {wayland_display}");

    let mut last_hash: u64 = 0;

    loop {
        if stop_rx.try_recv().is_ok() {
            break;
        }

        if overlay_cursor.load(Ordering::Relaxed) {
            std::thread::sleep(CURSOR_IDLE_INTERVAL);
            continue;
        }

        match capturer.capture_cursor() {
            Ok(c) => {
                let hash = cursor_hash(c.width, c.height, c.hotspot_x, c.hotspot_y, c.visible, &c.rgba);
                if hash != last_hash {
                    last_hash = hash;
                    let update = CursorUpdate {
                        x: c.x,
                        y: c.y,
                        hotspot_x: c.hotspot_x,
                        hotspot_y: c.hotspot_y,
                        width: c.width,
                        height: c.height,
                        visible: c.visible,
                        image_rgba: c.rgba,
                    };
                    if cursor_tx.blocking_send(update).is_err() {
                        break;
                    }
                }
            }
            Err(e) => {
                tracing::debug!("Cursor capture failed: {e}");
            }
        }

        std::thread::sleep(CURSOR_POLL_INTERVAL);
    }

    tracing::info!("Cursor shape watch thread exiting");
}

#[cfg(test)]
mod owned_by_caller_tests {
    use super::owned_by_caller;

    #[test]
    fn no_auth_sees_everything() {
        // caller=None means --auth isn't on; ownership isn't enforced at all,
        // regardless of what (if anything) the record's owner field says.
        assert!(owned_by_caller(&None, &None));
        assert!(owned_by_caller(&Some("alice".into()), &None));
    }

    #[test]
    fn ownerless_record_is_visible_to_any_authenticated_caller() {
        // A record with no owner predates ownership tracking; it can't
        // actually belong to a different authenticated user under the
        // current code, so it stays visible rather than becoming
        // unreachable after an upgrade.
        assert!(owned_by_caller(&None, &Some("alice".into())));
    }

    #[test]
    fn matching_owner_is_allowed() {
        assert!(owned_by_caller(&Some("alice".into()), &Some("alice".into())));
    }

    #[test]
    fn different_owner_is_refused() {
        // The actual point of this whole feature: bob authenticating
        // successfully as himself must not grant access to alice's session.
        assert!(!owned_by_caller(&Some("alice".into()), &Some("bob".into())));
    }
}

#[cfg(test)]
mod cursor_hash_tests {
    use super::cursor_hash;

    #[test]
    fn identical_inputs_hash_equal() {
        let rgba = vec![10u8, 20, 30, 255, 40, 50, 60, 255];
        let a = cursor_hash(2, 1, 0, 0, true, &rgba);
        let b = cursor_hash(2, 1, 0, 0, true, &rgba);
        assert_eq!(a, b);
    }

    #[test]
    fn pixel_change_changes_hash() {
        let a = cursor_hash(2, 1, 0, 0, true, &[10, 20, 30, 255, 40, 50, 60, 255]);
        let b = cursor_hash(2, 1, 0, 0, true, &[10, 20, 30, 255, 40, 50, 61, 255]);
        assert_ne!(a, b);
    }

    #[test]
    fn hotspot_change_changes_hash() {
        let rgba = vec![1u8, 2, 3, 255];
        let a = cursor_hash(1, 1, 0, 0, true, &rgba);
        let b = cursor_hash(1, 1, 3, 5, true, &rgba);
        assert_ne!(a, b);
    }

    #[test]
    fn visibility_change_changes_hash() {
        let rgba = vec![1u8, 2, 3, 255];
        let a = cursor_hash(1, 1, 0, 0, true, &rgba);
        let b = cursor_hash(1, 1, 0, 0, false, &rgba);
        assert_ne!(a, b);
    }

    #[test]
    fn size_change_changes_hash() {
        let rgba = vec![1u8, 2, 3, 255, 4, 5, 6, 255];
        let a = cursor_hash(2, 1, 0, 0, true, &rgba);
        let b = cursor_hash(1, 2, 0, 0, true, &rgba);
        assert_ne!(a, b);
    }
}

/// Set the remote session's clipboard via wl-copy.
fn set_remote_clipboard(wayland_display: &str, runtime_dir: &str, data: &[u8], mime_type: &str) {
    use std::io::Write;

    let mime_arg = if mime_type.is_empty() { "text/plain" } else { mime_type };

    let mut child = match std::process::Command::new("wl-copy")
        .args(["--type", mime_arg])
        .env("WAYLAND_DISPLAY", wayland_display)
        .env("XDG_RUNTIME_DIR", runtime_dir)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!("wl-copy failed: {e}");
            return;
        }
    };

    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(data);
    }
    let _ = child.wait();
}

/// Write one video frame to the Q2 video uni stream (raw QUIC or WebTransport).
async fn send_video_frame_split(
    planes: &mut crate::media::MediaPlanes,
    frame: &CapturedFrame,
) -> Result<()> {
    let CapturedFrame::Video { data, keyframe, width, height, timestamp_us, codec } = frame;
    let frame_type = if *keyframe { FrameType::Keyframe } else { FrameType::Inter };
    planes
        .send_video(*codec, frame_type, *width, *height, *timestamp_us, data)
        .await
}

/// Convert a captured frame (video) into a protocol message.
fn frame_to_message(frame: &CapturedFrame) -> Message {
    match frame {
        CapturedFrame::Video { data, keyframe, width, height, timestamp_us, codec } => {
            Message::VideoFrame(VideoFrame {
                timestamp_us: *timestamp_us,
                frame_type: if *keyframe { FrameType::Keyframe } else { FrameType::Inter },
                codec: Some(*codec),
                width: *width,
                height: *height,
                data: data.clone(),
            })
        }
    }
}