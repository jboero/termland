# Termland Roadmap

Termland is a Rust-based multi-tenant Wayland remote-desktop server and
client, streaming AV1-encoded video from a headless wlroots compositor
over TCP (today) or SSH (tomorrow).

This file tracks what's done, what's in progress, and what needs to
happen before the project is suitable for outside use.

## Current status

- ✅ End-to-end interactive session (video + input) from a KDE laptop
  to a headless z840 over LAN
- ✅ Hardware-accelerated AV1 encode (Intel QSV / NVENC / AMF / VA-API)
  with SVT-AV1 software fallback
- ✅ Hardware-accelerated AV1 decode (QSV / CUVID / dav1d) with runtime
  fallback when the chosen backend fails on the first frame
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
  profiles, connection dialog (see v0.3 stretch goals)

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

## v0.3 / stretch

- Clipboard sync (plain text first, then images)
- File transfer (clipboard paste of files, or drag-and-drop)
- Cursor shape sync — server tracks the compositor's active cursor
  shape (hover, text, wait, resize) and forwards it to the client so
  client-side cursor rendering matches what the remote would show
- Taskbar / window list protocol — plasmashell's task manager widget
  can't see labwc windows because labwc doesn't speak `org_kde_plasma
  _window_management`. Options: launch waybar alongside plasmashell
  (workaround), or patch labwc (upstream work).
- SDDM / greetd integration — proper login screen + session selection
  for multi-user deployments
- Seamless reconnect — drop/reconnect without losing the session
- Foreground session observability — list active sessions from the
  server CLI, kick/disconnect from the CLI
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
