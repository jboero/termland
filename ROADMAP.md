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

## v0.2 blockers

These need to land before I'd tag a 0.2 or invite outside users.

### Audio (Opus over PipeWire)

Capture the compositor's per-session PulseAudio/PipeWire output,
encode with `libopus`, stream as a new `AudioChunk` message. Client
decodes and plays via `cpal`.

Open questions: per-session PipeWire namespace so multi-session audio
doesn't cross-contaminate; handshake for sample rate/channels; sync
with video timestamps.

### Security: auth + TLS

Currently *any* TCP client on the LAN can start a session. Before this
leaves the dev LAN we need:

- PAM-based username/password auth (`pam::Authenticator`) during the
  Hello handshake
- `rustls`-wrapped TLS transport for direct TCP mode, with a simple
  self-signed-cert path for LAN use and pinned-cert verification on
  the client
- Session isolation: on successful auth, `setuid` into the target
  user's account so compositor + apps run as them, not as the server
  user
- Connection rate-limiting + fail2ban-style lockout

SSH subsystem mode will remain the zero-config secure option for
power users.

### GUI client rewrite (GTK4 / Qt6 / tao+muda)

The current client is `winit + softbuffer` with a hand-drawn bitmap-
font menubar. Functional but "retro". Before a public release the
client should have a **real native menubar** and proper toolkit
integration, via one of:

- **GTK4-rs** — most native-feeling on Linux, `GtkDrawingArea` or
  `GLArea` for the video surface, `GtkPopoverMenuBar` for menus
- **Qt6 + `cxx-qt`** — more polish, heavier, slightly awkward from
  Rust
- **`tao + muda`** — stays close to winit, gets native menus, but
  muda on Wayland is still rough

Whichever toolkit we pick we need to preserve:

- `zwp_keyboard_shortcuts_inhibit` so modifier keys still reach the
  remote session (toolkit must give us a raw `wl_surface`)
- Cursor overlay + local cursor rendering
- The menubar items we already have (data rate, client cursor toggle,
  fullscreen, quit)
- Transparent resize integration with our `SessionResize` protocol

Rough estimate: 1–2 days of focused work, plus testing.

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
