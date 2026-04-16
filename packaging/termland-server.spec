# RPM spec for termland-server
#
# Local build:
#   cd /path/to/parent && tar czf ~/rpmbuild/SOURCES/termland-0.3.0.tar.gz \
#       --transform='s,^termland,termland-0.3.0,' termland/
#   rpmbuild -ba termland/packaging/termland-server.spec
#
# COPR: upload this spec + source tarball for automated builds.

%global crate_name termland
%global version 0.3.1

Name:           termland-server
Version:        %{version}
Release:        1%{?dist}
Summary:        Termland remote desktop server — stream Wayland sessions via AV1/Opus

License:        LGPL-3.0-or-later
URL:            https://github.com/jboero/termland
Source0:        https://github.com/jboero/termland/archive/v%{version}/%{crate_name}-%{version}.tar.gz

# ─── Build dependencies ──────────────────────────────────────────────────────
# Rust toolchain (cargo, rustc)
BuildRequires:  rust >= 1.85
BuildRequires:  cargo >= 1.85

# FFmpeg development libraries (AV1 encoding via libavcodec/libavformat).
# ffmpeg-free-devel is in base Fedora; ffmpeg-devel (RPM Fusion) adds HW encoders.
BuildRequires:  (ffmpeg-free-devel or ffmpeg-devel)

# Opus audio codec
BuildRequires:  opus-devel

# PulseAudio client libraries (audio capture from session)
BuildRequires:  pulseaudio-libs-devel

# PAM development headers (authentication)
BuildRequires:  pam-devel

# libclang (ffmpeg-sys-next uses bindgen for FFI generation)
BuildRequires:  clang-devel

# TLS (aws-lc-rs / ring build deps)
BuildRequires:  cmake
BuildRequires:  gcc
BuildRequires:  gcc-c++
BuildRequires:  perl-interpreter

# Wayland client libraries (screencopy, input injection, output management)
BuildRequires:  wayland-devel
BuildRequires:  wayland-protocols-devel

# ─── Runtime dependencies ────────────────────────────────────────────────────
# Headless Wayland compositors — at least one required:
#   labwc: multi-window desktop sessions (recommended)
#   cage:  single-app kiosk mode
Requires:       (labwc or cage)

# FFmpeg runtime (AV1 encoder backends: QSV, NVENC, VA-API, SVT-AV1)
Requires:       (ffmpeg-libs or libavcodec-free)

# PulseAudio API (audio capture per session via null sink monitor).
# Modern Fedora uses pipewire-pulseaudio; classic PulseAudio also works.
Requires:       (pipewire-pulseaudio or pulseaudio)
Requires:       pulseaudio-utils

# Opus codec runtime
Requires:       opus

# PAM runtime (authentication)
Requires:       pam

# Wayland tools for clipboard, cursor, etc.
Requires:       wl-clipboard

# For SSH subsystem mode
Requires:       openssh-server

%description
Termland is a multi-tenant Wayland remote desktop server that streams
interactive desktop and application sessions using modern codecs.

Video is encoded as AV1 using hardware acceleration when available
(Intel QSV, NVIDIA NVENC, AMD AMF/VA-API) with SVT-AV1 software
fallback. Audio is forwarded via Opus over PulseAudio.

Transport modes:
  - SSH subsystem: zero-config, piggybacks on existing sshd (recommended)
  - Direct TCP with TLS + PAM authentication

Each session runs an isolated headless Wayland compositor (labwc for
desktop, cage for single-app kiosk) with its own screen capture,
input injection, and audio sink.

%prep
%setup -q -n %{crate_name}-%{version}

%build
cargo build --release --bin termland-server

# Generate shell completions
./target/release/termland-server --completions bash > termland-server.bash
./target/release/termland-server --completions zsh  > _termland-server
./target/release/termland-server --completions fish > termland-server.fish

%install
# Binary
install -Dm755 target/release/termland-server %{buildroot}%{_bindir}/termland-server

# Systemd service + environment config
install -Dm644 packaging/termland-server.service %{buildroot}%{_unitdir}/termland-server.service
install -Dm644 packaging/termland-server.env     %{buildroot}%{_sysconfdir}/sysconfig/termland-server

# PAM service
install -Dm644 packaging/termland.pam %{buildroot}%{_sysconfdir}/pam.d/termland

# SSH subsystem drop-in (sshd_config.d)
install -Dm644 packaging/50-termland.conf %{buildroot}%{_sysconfdir}/ssh/sshd_config.d/50-termland.conf

# Shell completions
install -Dm644 termland-server.bash %{buildroot}%{_datadir}/bash-completion/completions/termland-server
install -Dm644 _termland-server     %{buildroot}%{_datadir}/zsh/site-functions/_termland-server
install -Dm644 termland-server.fish %{buildroot}%{_datadir}/fish/vendor_completions.d/termland-server.fish

%post
%systemd_post termland-server.service

# Hint about setup
echo ""
echo "  Termland server installed. Two ways to run:"
echo ""
echo "  1) SSH subsystem (recommended — auto-configured):"
echo "     An sshd drop-in was installed at /etc/ssh/sshd_config.d/50-termland.conf"
echo "     Restart sshd to activate: systemctl restart sshd"
echo "     Clients connect with: termland-client --ssh user@host"
echo ""
echo "  2) Standalone TCP service (with TLS + PAM auth):"
echo "     Edit /etc/sysconfig/termland-server, then:"
echo "       systemctl enable --now termland-server"
echo "     Clients connect with: termland-client [--tls] host:7867"
echo ""

%preun
%systemd_preun termland-server.service

%postun
%systemd_postun_with_restart termland-server.service

%files
%license LICENSE
%doc README.md ROADMAP.md
%{_bindir}/termland-server
%{_unitdir}/termland-server.service
%config(noreplace) %{_sysconfdir}/sysconfig/termland-server
%config(noreplace) %{_sysconfdir}/pam.d/termland
%config(noreplace) %{_sysconfdir}/ssh/sshd_config.d/50-termland.conf
%{_datadir}/bash-completion/completions/termland-server
%{_datadir}/zsh/site-functions/_termland-server
%{_datadir}/fish/vendor_completions.d/termland-server.fish

%changelog
* Wed Apr 15 2026 John Boero - 0.3.0-1
- v0.3.0 release
- AV1 video encoding (QSV/NVENC/AMF/VA-API/SVT-AV1 auto-detect)
- AV1 decoding (QSV/CUVID/dav1d auto-fallback)
- Opus audio forwarding via PulseAudio null sink per session
- SSH subsystem transport (sshd drop-in config included)
- Direct TCP with TLS (auto-generated self-signed certs) + PAM auth
- Desktop mode (labwc) and app kiosk mode (cage)
- Live window resize with compositor + encoder reinit
- Client-side cursor, data rate overlay, menubar (F10)
- Shell completions for bash, zsh, fish
- systemd service + documented environment config
