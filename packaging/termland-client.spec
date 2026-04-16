# RPM spec for termland-client
#
# Local build:
#   cd /path/to/parent && tar czf ~/rpmbuild/SOURCES/termland-0.3.0.tar.gz \
#       --transform='s,^termland,termland-0.3.0,' termland/
#   rpmbuild -ba termland/packaging/termland-client.spec
#
# COPR: upload this spec + source tarball for automated builds.

%global crate_name termland
%global version 0.3.0

Name:           termland-client
Version:        %{version}
Release:        1%{?dist}
Summary:        Termland remote desktop client — view and interact with remote Wayland sessions

License:        LGPL-3.0-or-later
URL:            https://github.com/jboero/termland
Source0:        https://github.com/jboero/termland/archive/v%{version}/%{crate_name}-%{version}.tar.gz

# ─── Build dependencies ──────────────────────────────────────────────────────
# Rust toolchain
BuildRequires:  rust >= 1.85
BuildRequires:  cargo >= 1.85

# FFmpeg (AV1 decoding via dav1d/QSV/CUVID through libavcodec).
# Prefers ffmpeg-devel (RPM Fusion) for hardware decoder support (QSV, CUVID).
# Falls back to ffmpeg-free-devel (Fedora) for software-only (dav1d).
BuildRequires:  (ffmpeg-devel or ffmpeg-free-devel)

# Opus audio codec (decoding)
BuildRequires:  opus-devel

# Audio playback via ALSA (cpal backend)
BuildRequires:  alsa-lib-devel

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

%build
cargo build --release --bin termland-client

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
* Wed Apr 15 2026 John Boero - 0.3.0-1
- v0.3.0 release
- AV1 decoding with auto-fallback (QSV > CUVID > dav1d)
- Decoder reinit on resize (handles CUVID dimension change)
- Opus audio playback at 48kHz stereo via cpal
- SSH subsystem transport + direct TCP with TLS
- Live window resize, client-side cursor, data rate overlay
- Menubar toggle (F10), fullscreen toggle (F11)
- Shell completions for bash, zsh, fish
