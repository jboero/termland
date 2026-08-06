# RPM spec for termland-client
#
# Local build:
#   cd /path/to/parent && tar czf ~/rpmbuild/SOURCES/termland-0.3.0.tar.gz \
#       --transform='s,^termland,termland-0.3.0,' termland/
#   rpmbuild -ba termland/packaging/termland-client.spec
#
# COPR: upload this spec + source tarball for automated builds.

%global crate_name termland
%global version 0.6.1

Name:           termland-client
Version:        %{version}
Release:        1%{?dist}
Summary:        Termland remote desktop client — view and interact with remote Wayland sessions

License:        LGPL-3.0-or-later
URL:            https://github.com/jboero/termland
Source0:        https://github.com/jboero/termland/archive/v%{version}/%{crate_name}-%{version}.tar.gz
# Vendored crate dependencies, so %%build works offline (COPR/mock build roots
# have no network). Regenerate with: packaging/make-vendor-tarball.sh
Source1:        https://github.com/jboero/termland/releases/download/v%{version}/%{crate_name}-%{version}-vendor.tar.xz

# ─── Build dependencies ──────────────────────────────────────────────────────
# Rust toolchain
BuildRequires:  rust >= 1.85
BuildRequires:  cargo >= 1.85

# FFmpeg (AV1 decoding via dav1d/QSV/CUVID through libavcodec).
# ffmpeg-free-devel is in base Fedora; ffmpeg-devel (RPM Fusion) adds HW decoders.
BuildRequires:  (ffmpeg-free-devel or ffmpeg-devel)

# Opus audio codec (decoding)
BuildRequires:  opus-devel

# Audio playback via ALSA (cpal backend)
BuildRequires:  alsa-lib-devel

# libclang (ffmpeg-sys-next uses bindgen for FFI generation)
BuildRequires:  clang-devel

# TLS (aws-lc-rs build deps)
BuildRequires:  cmake
BuildRequires:  gcc
BuildRequires:  gcc-c++
BuildRequires:  perl-interpreter

# Wayland client (winit backend, keyboard shortcut inhibit)
BuildRequires:  wayland-devel
BuildRequires:  wayland-protocols-devel
BuildRequires:  libxkbcommon-devel

# X11 fallback (winit, softbuffer)
BuildRequires:  libX11-devel
BuildRequires:  libXcursor-devel
BuildRequires:  libXrandr-devel
BuildRequires:  libXi-devel

# ─── Runtime dependencies ────────────────────────────────────────────────────
# FFmpeg runtime (AV1 decoder backends)
Requires:       (ffmpeg-libs or libavcodec-free)

# Audio playback
Requires:       alsa-lib
Requires:       pipewire-alsa
Requires:       opus

# For SSH subsystem mode (connects via ssh command)
Requires:       openssh-clients

# Wayland / X11 display
Requires:       libwayland-client
Requires:       libxkbcommon

%description
Termland client connects to a Termland remote desktop server and displays
the session in a local window with full keyboard, mouse, and audio support.

Features:
  - AV1 video decoding with hardware acceleration (Intel QSV, NVIDIA CUVID)
    and automatic fallback to dav1d software decoder
  - Opus audio playback at 48kHz stereo
  - Live window resize (propagated to remote compositor)
  - Client-side cursor rendering for low-latency mouse interaction
  - SSH subsystem transport (recommended) or direct TCP with TLS

Connection modes:
  SSH (recommended):  termland-client --ssh user@host
  Direct TCP:         termland-client host:7867
  TLS:                termland-client --tls --accept-invalid-certs host:7867

Keyboard shortcuts:
  F10: toggle menubar (data rate, cursor mode, fullscreen, quit)
  F11: toggle fullscreen

%prep
%setup -q -n %{crate_name}-%{version}
# Unpack vendored crate deps (Source1) and point cargo at them so %%build runs
# fully offline — COPR/mock build roots have no network access.
tar -xf %{SOURCE1}
mkdir -p .cargo
cat > .cargo/config.toml <<'CARGOEOF'
[source.crates-io]
replace-with = "vendored-sources"

[source.vendored-sources]
directory = "vendor"
CARGOEOF

%build
cargo build --release --offline --bin termland-client

# Generate shell completions
./target/release/termland-client --completions bash > termland-client.bash
./target/release/termland-client --completions zsh  > _termland-client
./target/release/termland-client --completions fish > termland-client.fish

%install
# Binary
install -Dm755 target/release/termland-client %{buildroot}%{_bindir}/termland-client

# Shell completions
install -Dm644 termland-client.bash %{buildroot}%{_datadir}/bash-completion/completions/termland-client
install -Dm644 _termland-client     %{buildroot}%{_datadir}/zsh/site-functions/_termland-client
install -Dm644 termland-client.fish %{buildroot}%{_datadir}/fish/vendor_completions.d/termland-client.fish

%files
%license LICENSE
%doc README.md
%{_bindir}/termland-client
%{_datadir}/bash-completion/completions/termland-client
%{_datadir}/zsh/site-functions/_termland-client
%{_datadir}/fish/vendor_completions.d/termland-client.fish

%changelog
* Wed Aug 5 2026 John Boero - 0.6.1-1
- Packaging fix only (COPR source-fetch + an aarch64-only server-side
  build fix); no client behavior change.

* Wed Aug 5 2026 John Boero - 0.6.0-1
- Desktop session manager (--manager): saved multi-host connection profiles
  in a real window (egui), with a live per-host session list and
  resume/new/close, replacing the old one-host-per-launch tray-only flow.
- Seamless reconnect: auto-retry with backoff and reattach to the same
  session after an unexpected connection drop, instead of exiting.
- Bidirectional clipboard sync, including file transfer via clipboard paste.
- Real cursor-shape sync in client-side-cursor mode (actual compositor
  cursor bitmap, not a placeholder).
- Embedded SSH (russh) and QUIC transports (--quic), the latter now with Q2
  split video/audio planes.
- --list-sessions and --close gain the new session-owner semantics from
  server-side session isolation.

* Fri Jul 10 2026 John Boero - 0.5.0-1
- Resume persistent sessions: --attach <id> reconnects to a running session;
  --list-sessions and --close <id> manage them from the CLI
- --tray: a global system-tray session manager (StatusNotifierItem via ksni)
  that lists, resumes, closes, and starts sessions for a host

* Fri Jul 10 2026 John Boero - 0.4.2-1
- Fix HEVC/H.264 software decode fallback: use the native "hevc"/"h264" decoders
  (were pointed at encoder-only libx265/libx264); a hardware decoder dying
  mid-session now falls through to software instead of stalling
- Add --codec to force a specific codec (av1/vp9/vp8/h265/h264), with shell
  completion of the codec names
- Hold Ctrl while resizing to scale the frame (local zoom) instead of resizing
  the remote session

* Fri Jul 10 2026 John Boero - 0.4.1-2
- Build offline from vendored crate deps (COPR/mock build roots have no network)

* Fri Jul 10 2026 John Boero - 0.4.1-1
- Fix inverted vertical mouse scroll (winit up = Wayland down)

* Fri Jul 10 2026 John Boero - 0.4.0-1
- v0.4.0 release
- Multi-codec decoding with automatic fallback: AV1, VP9, VP8, H.265/HEVC, H.264
- Codec negotiation: advertises decodable codecs to the server and builds the
  decoder deterministically for the codec announced in SessionReady, instead of
  guessing from the bitstream (falls back to auto-detect with older servers)
- Decoder prefers CUVID/VA-API/V4L2 over Intel QSV on non-Intel systems to avoid
  wasting the opening frames on a doomed QSV context

* Wed Apr 15 2026 John Boero - 0.3.0-1
- v0.3.0 release
- AV1 decoding with auto-fallback (QSV > CUVID > dav1d)
- Decoder reinit on resize (handles CUVID dimension change)
- Opus audio playback at 48kHz stereo via cpal
- SSH subsystem transport + direct TCP with TLS
- Live window resize, client-side cursor, data rate overlay
- Menubar toggle (F10), fullscreen toggle (F11)
- Shell completions for bash, zsh, fish
