# Termland

**A Wayland remote desktop server that actually works.**

Termland streams full interactive desktop and application sessions over the network using AV1 video, Opus audio, and modern transport security. It exists because Wayland broke remote desktop workflows and nobody fixed them.

## Why This Exists

For over two decades, X11 gave Linux a simple, reliable remote desktop story. X forwarding, NX/X2Go, and FreeNX let you run graphical sessions on remote servers as easily as SSH. Entire organizations built terminal server infrastructure on this — thin clients, shared workstations, remote development environments.

Then Wayland happened.

Wayland's architecture deliberately removed the network transparency that made all of this possible. The display protocol became local-only by design. X forwarding? Gone. NX protocol? Dead. X2Go? Broken on any modern desktop that defaults to Wayland.

The replacements are inadequate:
- **xrdp** only works through X11 compatibility layers, defeating the purpose
- **GNOME Remote Desktop** is GNOME-only, limited, and requires PipeWire plumbing
- **KDE's Krfb/Krdc** lost most functionality in the Wayland transition
- **VNC-over-Wayland** solutions are slow, lack proper input handling, and don't support audio
- **Chrome Remote Desktop** requires a Google account and only works with Chrome

If you run any modern Linux desktop on Wayland — KDE Plasma, GNOME, sway, Hyprland, or anything else — your remote desktop options range from limited to nonexistent. KDE is hit hardest (Krfb/Krdc essentially stopped working), but the gap affects every Wayland compositor.

**Termland fills this gap.** It works with any Wayland-compatible desktop environment or application.

## What It Does

Each session runs an isolated headless Wayland compositor with its own screen capture, input injection, and audio sink. The video stream is AV1-encoded with hardware acceleration when available, and audio is forwarded via Opus. The whole thing runs over SSH or direct TCP with TLS.

```
  Client (laptop/thin client)              Server (workstation/server)
  +----------------------------+           +----------------------------------+
  |  termland-client           |           |  termland-server                 |
  |  - AV1 decode (HW/SW)     |  SSH or   |  - Headless Wayland compositor   |
  |  - Opus audio playback     |<--------->|  - AV1 encode (HW/SW)           |
  |  - Keyboard/mouse capture  |  TCP+TLS  |  - Opus audio capture            |
  |  - Live window resize      |           |  - PAM authentication            |
  +----------------------------+           +----------------------------------+
                                                        |
                                                  Wayland apps
                                              (Plasma, Firefox, etc.)
```

## Features

### Video
- **AV1 encoding** with automatic hardware detection:
  Intel QSV, NVIDIA NVENC, AMD AMF, AMD VA-API, SVT-AV1 software fallback
- **AV1 decoding** with automatic fallback:
  Intel QSV, NVIDIA CUVID, dav1d software
- **Adaptive quality**: configurable bitrate, CRF, encoder preset
- **Live resize**: drag the client window and the remote compositor resizes to match
- Typical data rate: **~2 KB/s** for a still 4K desktop, scaling with motion

### Audio
- **Opus codec** at 48kHz stereo, 32kbps with DTX and FEC
- Per-session PulseAudio null sink (session-isolated audio)
- Silence detection skips encoding when nothing is playing

### Transport
- **SSH subsystem** (recommended): zero-config, piggybacks on existing sshd.
  Uses your SSH keys, LDAP, Kerberos, 2FA — whatever sshd is configured for.
  Install the RPM, restart sshd, done.
- **Direct TCP with TLS**: auto-generated self-signed certs or bring your own.
  PAM authentication for any backend your system supports.

### Session Modes
- **Desktop**: full multi-window session via labwc (Plasma, GNOME, sway, etc.)
- **App**: single fullscreen application via cage (kiosk mode)

### Client
- Client-side cursor rendering for low-latency mouse interaction
- Data rate overlay, fullscreen toggle (F11), menubar toggle (F10)
- Shell tab completion for bash, zsh, fish

## Quick Start

### SSH Mode (Recommended)

On the server, install the RPM (or copy the binary) and restart sshd:

```bash
# The RPM installs an sshd drop-in automatically:
#   /etc/ssh/sshd_config.d/50-termland.conf
sudo systemctl restart sshd
```

On the client:

```bash
termland-client --ssh user@server
```

That's it. SSH handles authentication and encryption.

### Direct TCP Mode

On the server:

```bash
# With TLS + PAM auth (recommended for non-SSH deployments)
termland-server --tls --auth --bind 0.0.0.0

# Or plaintext on localhost (behind SSH tunnel)
termland-server
```

On the client:

```bash
# TLS with self-signed cert
termland-client --tls --accept-invalid-certs server:7867

# With authentication
termland-client --tls --accept-invalid-certs --user john --password xxx server:7867

# Plaintext (localhost/tunnel only)
termland-client localhost:7867
```

### Options

```
# Video quality (1-100, default 75)
termland-client -q 50 --ssh user@server

# Enable audio
termland-client --audio --ssh user@server

# App mode (single app, kiosk)
termland-client --mode app:firefox --ssh user@server

# Custom desktop shell
termland-client --desktop-shell "dbus-run-session sway" --ssh user@server

# Encoder tuning (SVT-AV1)
termland-client --preset 8 --crf 30 --ssh user@server
```

## Building from Source

### Dependencies

Fedora/RHEL:
```bash
sudo dnf install rust cargo ffmpeg-free-devel opus-devel pulseaudio-libs-devel \
    pam-devel wayland-devel wayland-protocols-devel alsa-lib-devel \
    libxkbcommon-devel libX11-devel cmake gcc gcc-c++
```

### Build

```bash
cargo build --release
# Binaries in target/release/termland-server and target/release/termland-client
```

### RPM Packages

```bash
# Create source tarball
cd /path/to/parent
tar czf ~/rpmbuild/SOURCES/termland-0.3.0.tar.gz \
    --transform='s,^termland,termland-0.3.0,' termland/

# Build RPMs
rpmbuild -ba termland/packaging/termland-server.spec
rpmbuild -ba termland/packaging/termland-client.spec
```

## Architecture

```
termland/
  crates/
    termland-protocol/    Wire protocol, CBOR serialization, framing
    termland-compositor/  Headless Wayland compositor (labwc/cage), screencopy, input
    termland-codec/       AV1 encode/decode, Opus encode/decode
    termland-server/      Session broker, PAM auth, TLS, capture + encode pipeline
    termland-client/      winit window, softbuffer renderer, decode + playback
```

**Wire protocol**: length-delimited binary framing (`[Magic "TL"][MsgID][Length][CBOR]`) carrying control messages (handshake, auth, session lifecycle, resize, ping) and data messages (AV1 video, Opus audio, cursor, clipboard, input events).

**Encoder pipeline**: compositor buffer capture via wlr-screencopy-unstable-v1, RGBA-to-YUV conversion respecting ffmpeg's 32-byte row alignment, hardware encoder probing at startup with automatic fallback.

**Decoder pipeline**: hardware decoder probing with confirmation-before-trust (first successful decode confirms the backend), automatic reinit on dimension change (handles CUVID's SPS binding), automatic fallback to next backend on failure.

## Packaging

The server RPM installs:
- `/usr/bin/termland-server`
- `/etc/ssh/sshd_config.d/50-termland.conf` — SSH subsystem registration
- `/usr/lib/systemd/system/termland-server.service` — systemd unit
- `/etc/sysconfig/termland-server` — documented environment config
- `/etc/pam.d/termland` — PAM service for auth
- Shell completions for bash, zsh, fish

The client RPM installs:
- `/usr/bin/termland-client`
- Shell completions for bash, zsh, fish

## Roadmap

- [ ] Qt6 client with native menubar and session manager
- [ ] Clipboard sync (protocol messages defined, implementation pending)
- [ ] QUIC/WebTransport for UDP video stream
- [ ] Per-session privilege separation (fork + setuid after PAM auth)
- [ ] Audio bitrate configuration
- [ ] Multi-monitor support
- [ ] Web client (WebCodecs + WebTransport)

## License

MIT OR Apache-2.0
