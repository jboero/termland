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
- ✅ Embedded SSH (`russh`) and QUIC (Q1) transports, on both the desktop
  server and the Android core
- ✅ Native Android client (M1 core + M2 app) — see "Mobile clients" below

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

- Session isolation: `setuid` into authenticated user after PAM auth
  (currently sessions run as server user)
- GUI client rewrite: Qt6 native menubar, session manager with saved
  profiles, connection dialog (now folded into the v0.5 milestone below)

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
a `.desktop`/autostart entry for the tray; per-session idle-timeout policy;
and, if ever wanted, a richer cxx-qt UI (the ksni tray covers the core need).

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

### C. Qt6 tray + session manager (client chrome)

- **One global systray icon** (StatusNotifierItem) for all open connections;
  its menu lists active/resumable sessions with attach/close actions.
- **Session manager window:** saved connection profiles, per-host session
  list, new-session dialog. Build with **`cxx-qt`** (Rust-driven Qt6/QML) so
  the existing decode/render/input engine stays in Rust and the viewer
  window simply becomes "attach to session N".
- Supersedes the deferred v0.2 "Qt6 GUI client rewrite" item, and folds in
  the "seamless reconnect" and "foreground session observability" items from
  the stretch list below.

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
- Android audio playback (AudioTrack) is a documented no-op stub — the core
  already delivers `onAudioPacket`, only the Kotlin-side player is
  unimplemented. iOS (M3) has not been started.
- Not runtime-verified: no device or emulator was available while building
  this. Everything above is confirmed at the build/compile/unit-test level
  (including two independent live QUIC handshake proofs — see below — and a
  real assembled APK with both native-lib ABIs inspected), not by an actual
  tablet talking to an actual server.

### QUIC transport — Q1 SHIPPED, Q2 not started

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
- Not started: **Q2 — split the planes** onto their own streams/datagrams for
  HOL-free A/V.
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
- File transfer (clipboard paste of files, or drag-and-drop)
- Taskbar / window list protocol — plasmashell's task manager widget
  can't see labwc windows because labwc doesn't speak `org_kde_plasma
  _window_management`. Options: launch waybar alongside plasmashell
  (workaround), or patch labwc (upstream work).
- SDDM / greetd integration — proper login screen + session selection
  for multi-user deployments
- Seamless reconnect — drop/reconnect without losing the session
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
