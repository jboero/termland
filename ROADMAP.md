# Termland Roadmap

Termland is a Rust-based multi-tenant Wayland remote-desktop server and
client, streaming AV1-encoded video from a headless wlroots compositor
over TCP, TLS, SSH, or QUIC. A native Android client (thin-client focus:
physical keyboard + mouse on a tablet) ships alongside the desktop client.

This file tracks what's done, what's in progress, and what needs to
happen before the project is suitable for outside use.

## Current status

- ✅ End-to-end interactive session (video + input) from a KDE laptop
  to a headless z840 over LAN
- ✅ Hardware-accelerated AV1 encode (Intel QSV / NVENC / AMF / VA-API)
  with SVT-AV1 software fallback
- ✅ Hardware-accelerated AV1 decode (QSV / CUVID / dav1d) with runtime
  fallback when the chosen backend fails on the first frame
- ✅ Multi-codec video with negotiation and fallback (v0.4.x) — AV1, VP9,
  VP8, H.265/HEVC, H.264; hardware-first probe (e.g. a Volta/GV100 without
  AV1 encode uses its hardware HEVC), the client builds its decoder from the
  codec the server announces and falls back to software if no hardware
  decoder is available; `--codec` forces a specific codec
- ✅ Ctrl-drag to scale the frame locally (zoom) instead of resizing the
  remote session
- ✅ cage backend for single-app kiosk sessions (`--mode app:<cmd>`)
- ✅ labwc backend for multi-window desktop sessions (`--mode desktop`)
- ✅ Auto-detects plasmashell when KDE is available and launches a
  basic Plasma-ish desktop inside labwc
- ✅ Full keyboard/mouse/scroll forwarding with modifier-aware injection
- ✅ `zwp_keyboard_shortcuts_inhibit` on the client captures Ctrl/Alt/
  Super/Alt-F4 etc. so they reach the remote session
- ✅ Client-side cursor rendering (lower latency over WAN) toggleable
  via menubar
- ✅ Live bandwidth display (toggle in menubar) + window title
- ✅ Live session resize — drag the client window and the remote
  compositor + AV1 encoder reconfigure automatically (zwlr_output
  _manager_v1 + encoder reinit)
- ✅ Configurable encoder tuning (--preset / --crf / --svt-params)
- ✅ Multi-session server (many clients → many independent compositor
  instances in one `termland-server`)
- ✅ Bidirectional clipboard sync, real cursor-shape sync, session
  observability CLI (`--list-sessions`/`--close-session`)
- ✅ Desktop session manager (`--manager`, egui): saved multi-host profiles,
  live per-host session list, resume/new/close
- ✅ Embedded SSH (`russh`) and QUIC (Q1 + Q2: split video/audio planes)
  transports, on both the desktop server and the Android core
- ✅ Native Android client (M1 core + M2 app) — see "Mobile clients" below
- ✅ Session isolation (setuid into the PAM-authenticated user), clipboard
  file transfer, seamless reconnect, Android audio playback

## v0.2 — SHIPPED (v0.3.1)

All v0.2 blockers have been resolved:

- ✅ **Audio**: Opus 48kHz stereo via per-session PulseAudio null sink,
  silence detection + DTX, cpal playback on client (`--audio`)
- ✅ **TLS**: rustls with auto-generated self-signed certs (`--tls`),
  custom cert/key paths, client `--accept-invalid-certs`
- ✅ **PAM auth**: manual FFI bindings (no bindgen dep), falls back to
  "login" service, 3s delay on failure (`--auth`)
- ✅ **SSH subsystem**: zero-config via sshd drop-in config, client
  `--ssh` with `--ssh-opt` for custom SSH args
- ✅ **Security hardening**: command injection prevention (shell
  metacharacter validation), password zeroing, max 32 concurrent
  sessions, plaintext auth warnings
- ✅ **RPM packaging**: server + client specs for COPR, systemd unit,
  env config, PAM service, shell completions (bash/zsh/fish)

### Remaining v0.2 items (deferred)

- ✅ **Session isolation**: `setuid` into the PAM-authenticated user after
  auth, instead of every session running as the server's own user (root).
  `initgroups`→`setgid`→`setuid` in a `pre_exec` closure after resolving the
  target user via thread-safe `getpwnam_r`; fails closed on an unknown
  username or a resolved uid of 0. Also closes a gap an adversarial review
  found before this was accepted: OS-level uid separation alone didn't stop
  one authenticated user from listing/attaching to/closing a *different*
  authenticated user's session — `SessionRecord` now tracks an `owner` and
  `SessionList`/`SessionAttach`/`SessionClose` enforce it. Integrating this
  also surfaced (and fixed) a real regression: several already-shipped
  Wayland-connecting code paths (clipboard sync, cursor-shape sync, and —
  most seriously — keyboard/mouse input injection) resolved the compositor's
  socket via this *process's own* `XDG_RUNTIME_DIR`, which silently breaks
  once an isolated session's compositor runs under a different uid's
  `/run/user/<uid>`. Fixed by threading the compositor's actual resolved
  runtime dir end to end instead of assuming it.
- ✅ ~~GUI client rewrite: Qt6 native menubar, session manager with saved
  profiles, connection dialog~~ — shipped as `--manager` (egui, not Qt6; see
  v0.5 section C below for why).

## v0.4 / GPU rendering + zero-copy capture

### GPU-accelerated rendering inside sessions

Currently sessions render via `llvmpipe` (CPU software rasterizer) because
the headless wlroots backend has no GPU context. This means OpenGL/Vulkan
apps run but are slow — fine for desktops and terminals, inadequate for
3D apps, CAD, Blender, Shadertoy, or games.

The goal: **full GPU rendering inside the session, with zero-copy handoff
to the hardware AV1 encoder**. This would make Termland competitive with
cloud gaming solutions (Sunshine/Moonlight, Parsec) — something no
traditional remote desktop protocol (RDP, NX, X2Go, VNC) has achieved.

Pipeline today (CPU render, CPU readback, HW encode):
```
App → llvmpipe (CPU) → wlr-screencopy → memcpy → AV1 HW encode → wire
```

Target pipeline (GPU render, zero-copy, HW encode):
```
App → GPU EGL/Vulkan → DMA-BUF → VA-API/NVENC AV1 encode → wire
```

Implementation path:
1. **DRM render node allocation** — expose a GPU render node to the
   headless compositor via `WLR_RENDERER=vulkan` or `WLR_RENDERER=gles2`
   with a real DRM device (not the headless shim)
2. **DMA-BUF screencopy** — use `zwlr_screencopy_manager_v1` with
   `wl_buffer` backed by DMA-BUF instead of SHM, so the captured frame
   stays in GPU memory
3. **Zero-copy encode** — feed the DMA-BUF directly to VA-API / NVENC
   AV1 encoder without CPU readback. FFmpeg supports DMA-BUF input via
   `hwframe` contexts
4. **DRM lease for multi-GPU** — on systems with multiple GPUs (e.g.
   iGPU + dGPU), lease a render node from the discrete GPU for the
   session while the iGPU drives local display

Benefits:
- 3D/Vulkan apps run at native GPU speed inside remote sessions
- No CPU copies in the capture→encode path (currently the bottleneck)
- Enables 4K 60fps streaming for GPU-intensive workloads
- Feature parity with cloud gaming, exceeding what RDP/NX/X2Go ever offered

### QUIC / WebTransport

Replace TCP with QUIC for the video/audio data stream. Benefits:
- UDP-based: no head-of-line blocking from lost packets
- 0-RTT reconnection for session resume
- Independent streams for video, audio, and control (no priority inversion)
- WebTransport variant enables a future browser-based client

Now designed and pulled forward alongside the mobile work — see
[docs/quic-transport.md](docs/quic-transport.md).

## v0.5 — SHIPPED (v0.5.0): Persistent sessions + tray session manager

X2Go/NX-style **detachable sessions** and a global tray manager — done and
tested on the Z840 (GV100).

- ✅ **Daemon-free persistence.** Each session is a *detached* compositor
  (`setsid` + stdio→logfile) that outlives the connection process — even a
  stateless SSH-subsystem one — recorded in a filesystem registry at
  `$XDG_RUNTIME_DIR/termland/sessions/<id>.session`. Any new connection reads
  the dir to list/validate/attach; no daemon needed.
- ✅ **Detach vs close.** Disconnect leaves the compositor running (resumable);
  an explicit close (or `--close`) terminates it.
- ✅ **Control protocol.** `SessionList`/`SessionListResult`/`SessionInfo`,
  `SessionAttach`, `SessionClose`; `session_id` on `SessionReady`.
- ✅ **Client.** `--attach <id>` resumes; `--list-sessions` / `--close`
  manage from the CLI; `--tray` runs a global systray manager.
- ✅ **Tray.** A StatusNotifierItem via **ksni** (pure-Rust zbus — no
  cxx-qt/Qt6 build burden, and it satisfies the "one global icon + session
  list" goal). Lists sessions with Resume/Close, plus New session.

Verified: create session → disconnect (compositor + app keep running) → kill
the server process (compositor survives via `setsid`) → fresh server process →
attach resumes and decodes. Tray registers and lists live sessions.

Deferred to a later pass: session-sink audio continuity across detach/attach;
a `.desktop`/autostart entry for the tray; per-session idle-timeout policy.

The "richer UI" this note said might never be needed turned out to be
needed: `--tray` requires a server address on the CLI every launch, with no
saved multi-host profiles at all. See section C below — now ✅ shipped, and
not with cxx-qt.

### A. Server-side session persistence (the milestone)

- **Decouple session lifetime from the connection.** Today
  [`transport.rs`](crates/termland-server/src/transport.rs) spawns a
  compositor + capture per connection and tears it down on disconnect.
  Instead, a long-lived session daemon owns compositors in a registry keyed
  by session id.
- **Detach on disconnect:** keep the compositor + capture running; stop the
  encoder stream but preserve session state.
- **Attach / resume:** a reconnecting client attaches by session id; the
  server reinitializes the encoder and sends a fresh keyframe so the client
  renders immediately.
- **SSH-subsystem implication (important):** `ssh -s … termland` spawns a
  fresh, *stateless* server process per connection, so persistence requires a
  background **daemon** (systemd service) that the per-connection subsystem
  process proxies to over a local Unix socket. TCP mode embeds or proxies to
  the same daemon.
- **Lifecycle:** sessions live until explicitly closed (client "quit
  session" or a server idle/timeout policy), not until disconnect.

### B. Control-plane protocol for sessions

- New messages to **list / create / attach / close** sessions (a small
  extension to the `Message` enum), returning id, mode, resolution, age, and
  codec.
- Client caches known sessions per host to populate the resume list.

### C. Session manager (client chrome) — SHIPPED, not with Qt6

- ✅ **One global systray icon** (StatusNotifierItem via ksni) for all open
  connections; its menu lists active/resumable sessions with attach/close
  actions, plus a "Manage profiles…" entry into the window below.
- ✅ **Session manager window** (`termland-client --manager`): saved
  connection profiles (`~/.config/termland/profiles.json`), per-host session
  list, new-session/resume/close actions. The plan called for **`cxx-qt`**
  (Rust-driven Qt6/QML); that doesn't work in this environment — a real,
  reproducible incompatibility where cxx-qt-build/qt-build-utils 0.7.3 emits
  no Qt6 link flags at all against the installed Qt 6.11.1, confirmed with a
  from-scratch scratch-crate build. Also matches this codebase's own earlier
  choice, in the paragraph above, of pure-Rust `ksni` over cxx-qt for the
  tray for the same "build burden" reason. Built with **`egui`/`eframe`**
  instead (pure Rust, confirmed building cleanly on the first try) — the
  existing decode/render/input engine still stays entirely in
  `termland-client`'s normal windowed mode; the manager only ever shells out
  to it (`std::env::current_exe()` + `Command`, same pattern the tray already
  used), never runs it in-process, since winit's event loop and egui's can't
  share a process.
- Supersedes the deferred v0.2 "Qt6 GUI client rewrite" item, and closes the
  "foreground session observability" desktop-client half of that stretch-list
  item (the server-side half — `--list-sessions`/`--close-session` on
  `termland-server` — shipped separately). "Seamless reconnect" is still
  open — the manager makes resuming a dropped session one click, not
  automatic.

### Suggested order

1. Session daemon + registry + detach/attach (A) — the long pole.
2. Control-plane messages (B) — small, unblocks the UI.
3. Tray + manager (C) — build on the working attach/resume flow.

## Mobile clients — Android SHIPPED (M1 + M2), iOS not started

Native touch clients for phones/tablets. Full design in
[docs/mobile-clients.md](docs/mobile-clients.md); this section tracks what's
actually built.

- ✅ **M1 — shared Rust core** (`termland-mobile-core`, exposed via UniFFI →
  Kotlin) reuses `termland-protocol`, codec negotiation, and v0.5 session
  control as-is. Does protocol + negotiation + packet routing;
  **does not bundle FFmpeg** — video decode is MediaCodec, entirely on the
  Kotlin side.
- ✅ **M2 — Android app**: Kotlin + Jetpack Compose. Profile/session screen
  (DataStore-backed, lists resumable v0.5 sessions with resume/new/close),
  `SurfaceView` + MediaCodec decoding straight to the `Surface`, codec
  capability probed via `MediaCodecList` at startup and passed as
  `supported_codecs` so negotiation picks correctly per device. Primary use
  case (per the project's own aim) is a tablet as a thin client with a
  physical keyboard and mouse: real `onKeyDown`/`onKeyUp` → evdev
  scancode/keysym mapping (Ctrl+C, Alt+Tab etc. reach the remote), real
  `onGenericMotionEvent` mouse handling (hover motion, button state → evdev
  `BTN_*`, scroll axes) plus pointer capture for a relative/trackpad mode.
  Touch (tap/long-press/drag/two-finger-scroll) and an on-screen modifier bar
  cover the secondary touch-only case. A soft-keyboard IME path commits
  Unicode text via `TextInput` rather than synthesizing scancodes.
  `./gradlew assembleDebug` cross-compiles the core for arm64-v8a + x86_64 via
  cargo-ndk and generates the Kotlin bindings automatically — verified
  building a real APK, not just compiling.
- ✅ **Transport, in priority order:** embedded SSH (`russh`, pure-Rust — the
  in-process equivalent of the desktop client's `ssh -s host termland`, since
  mobile sandboxes forbid spawning the `ssh` binary) → **QUIC** (see below) →
  TCP+TLS → plain TCP.
- ✅ **Keyboard/text**, the hard part (IME, autocorrect, CJK, emoji don't map
  to scancodes): shipped as a **dynamic-xkb-keymap** approach rather than the
  originally-planned input-method-v2 — it works against every surface
  including terminals/games that don't participate in text-input, needs no
  compositor IME support, and reuses the virtual-keyboard path already in
  place. `TextInput` (Unicode) alongside the existing `KeyEvent` path
  (editing/shortcut keys) plus an on-screen modifier bar. Verified against
  real libxkbcommon, not just "it compiles."
- ✅ **v0.5 persistence UX**: resumable-session list is the app's home screen,
  per the "mobile links drop constantly" rationale this was built for.
- ✅ **Android audio playback** — async MediaCodec (`audio/opus`) → AudioTrack,
  the audio-side twin of the video decoder's shape. The fiddly part: MediaCodec
  expects Ogg/WebM-Opus container CSD, but the stream is headerless raw Opus,
  so the mandatory 19-byte OpusHead (RFC 7845 §5.1) is synthesized by hand from
  the two fixed, never-negotiated stream parameters (48kHz stereo).
  iOS (M3) has not been started.
- Not runtime-verified: no device or emulator was available while building
  this. Everything above is confirmed at the build/compile/unit-test level
  (including two independent live QUIC handshake proofs — see below — and a
  real assembled APK with both native-lib ABIs inspected), not by an actual
  tablet talking to an actual server.

### QUIC transport — Q1 + Q2 SHIPPED

Full design in [docs/quic-transport.md](docs/quic-transport.md). QUIC gives
**connection migration** (survive Wi-Fi↔cellular), **0-RTT resume**, and
**no head-of-line blocking** across video/audio/input — exactly what lossy,
roaming mobile links need, and it pairs with v0.5 resume. `quinn` (pure-Rust)
on both server (`--quic`/`--quic-port`, alongside the existing TCP listener)
and the mobile core (`Transport::Quic`).

- ✅ **Q1 — drop-in single-stream transport.** The entire existing protocol
  over one QUIC bidi stream; `handle_session` was already generic over any
  `AsyncRead + AsyncWrite`, so nothing downstream changed. Verified with a
  real integration test (spawns the actual server binary, opens a real QUIC
  connection, sends `Hello`, gets back a real `HelloAck`) plus a second,
  independent manual client against a running `--quic` server.
- ✅ **Q2 — split planes.** Video moved to its own reliable server-opened QUIC
  uni stream (fixed 18-byte binary header, not CBOR — this stream only ever
  carries one shape of message); audio moved to QUIC datagrams (one Opus
  chunk per datagram, no fragmentation needed at real-world sizes). Control
  (including input) stays on the one bidi stream, byte-for-byte unchanged.
  `termland-mobile-core` is the only QUIC client that exists, so Q2 replaces
  Q1's single-stream contract outright rather than negotiating between the
  two. `handle_session`/`run_session` gained one new `Option<quinn::Connection>`
  parameter (`None` for every non-QUIC transport — zero behavior change
  there) rather than a bigger transport-abstraction rewrite. Verified live,
  not just unit-tested: a real end-to-end integration test gets a genuine
  AV1-encoded keyframe off the video stream and a genuine Opus datagram (fed
  from real non-silent audio via `pacat`) off the audio plane, both parsed
  back and checked against what the server actually produced. The harder
  remaining piece — fragmenting individual frames across datagrams with
  FEC/pacing so a lost packet costs part of a frame instead of stalling —
  is now called out as its own **Q3** in
  [docs/quic-transport.md](docs/quic-transport.md), not started, and only
  worth doing once Q2's simpler reliable-stream video is shown to actually
  stall under real cellular loss.
- Along the way, fixed a real pre-existing bug: `--tls` could panic at
  runtime (`Could not automatically determine the process-level
  CryptoProvider`) once a full workspace build unified two crates'
  differing rustls crypto-provider choices. Now pinned explicitly.

## v0.3 / stretch

- ✅ **Clipboard sync** (plain text) — bidirectional via `wl-copy`/`wl-paste`
  subprocesses on both ends, hash-diffed to avoid echo loops. Images/files
  remain open.
- ✅ **Cursor shape sync** — the server captures the real compositor cursor
  bitmap (`ext-image-copy-capture-v1`'s pointer cursor session, not a
  semantic shape name — that negotiation is internal to labwc and isn't
  observable externally) and forwards it via the `CursorUpdate` message
  (defined from the start, previously unwired). Client-side-cursor mode now
  renders the real remote cursor instead of a generic placeholder dot.
- ✅ **Foreground session observability** — `termland-server --list-sessions`
  / `--close-session <id>` read/signal the v0.5 registry directly (no
  network round-trip, no running server process required).
- ✅ **File transfer** — scoped to clipboard paste of files (copy files on one
  side, real files land on the other's clipboard); full drag-and-drop between
  windows remains open, a separate and much bigger Wayland DnD integration.
  New `FileTransferData`/`FileTransferSend` messages, capped at 12 MiB total
  and sent unchunked — deliberately "a few documents or images," not a
  general transfer protocol. Filenames arriving over the wire are sanitized
  to a bare basename (reject, not strip), closing the path-traversal surface
  a crafted `../../etc/passwd` entry would otherwise open.
- ✅ **Seamless reconnect** — an unexpected connection loss (vs. a real
  server-sent `SessionEnd`) now auto-retries with capped exponential backoff
  (1s→2s→4s→8s→16s→30s, indefinitely; `--no-reconnect` opts out), reattaching
  to the *same* session rather than creating a new one, with a "Reconnecting…
  (attempt N)" banner over the frozen last frame instead of exiting. Verified
  live: killed a running server mid-session, watched the client retry and
  never exit, restarted the server, confirmed reattachment to the identical
  session id.
- Taskbar / window list protocol — plasmashell's task manager widget
  can't see labwc windows because labwc doesn't speak `org_kde_plasma
  _window_management`. Options: launch waybar alongside plasmashell
  (workaround), or patch labwc (upstream work).
- SDDM / greetd integration — proper login screen + session selection
  for multi-user deployments
- Native Windows / macOS clients — currently Linux only; the server
  is Wayland-specific by design but the client can be cross-platform

## Architecture notes

### Crates

- `termland-protocol` — wire format (CBOR over length-delimited framing)
- `termland-codec` — FFmpeg encoder/decoder wrapping + hardware probe
- `termland-compositor` — cage/labwc launcher, wlr-screencopy capture,
  virtual input injection, zwlr_output_manager resize driver
- `termland-server` — session broker, capture/encode loop, transport
- `termland-client` — winit + softbuffer + FFmpeg decode, overlay UI

### Known limitations

- `kwin_wayland` as the compositor is a dead end for our capture
  pipeline — it doesn't expose `zwlr_screencopy_manager_v1`,
  `zwp_virtual_keyboard_v1`, or `zwlr_virtual_pointer_v1`. It only
  speaks KDE-specific protocols + `xdg-desktop-portal`. This is why
  we use labwc for "desktop" mode and run plasmashell inside it.
- plasmashell's task manager widget is empty inside labwc because
  labwc doesn't implement `org_kde_plasma_window_management`. See
  v0.3 notes.
