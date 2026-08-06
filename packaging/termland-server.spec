# RPM spec for termland-server
#
# Local build:
#   cd /path/to/parent && tar czf ~/rpmbuild/SOURCES/termland-0.3.0.tar.gz \
#       --transform='s,^termland,termland-0.3.0,' termland/
#   rpmbuild -ba termland/packaging/termland-server.spec
#
# COPR: upload this spec + source tarball for automated builds.

%global crate_name termland
%global version 0.6.1
# find-debuginfo can produce an empty debugsourcefiles.list for a Rust
# binary (unlike typical C sources), which newer Fedora's rpmbuild tolerates
# but EL8's treats as a hard error ("Empty %%files file ... debugsourcefiles
# .list"). Disable debuginfo package generation outright rather than fight
# find-debuginfo's Rust handling per-distro - standard practice for Rust RPM
# packages, and this project doesn't rely on -debuginfo/-debugsource
# subpackages for anything.
%global debug_package %{nil}

Name:           termland-server
Version:        %{version}
Release:        2%{?dist}
Summary:        Termland remote desktop server — stream Wayland sessions via AV1/VP9/HEVC/H.264/Opus

License:        LGPL-3.0-or-later
URL:            https://github.com/jboero/termland
Source0:        https://github.com/jboero/termland/archive/v%{version}/%{crate_name}-%{version}.tar.gz
# Vendored crate dependencies, so %%build works offline (COPR/mock build roots
# have no network). Regenerate with: packaging/make-vendor-tarball.sh
Source1:        https://github.com/jboero/termland/releases/download/v%{version}/%{crate_name}-%{version}-vendor.tar.xz

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
# File-based, not `perl-interpreter`: that virtual-provide name is a
# Fedora/RHEL convention Mageia doesn't recognize ("No match for argument:
# perl-interpreter"). Requiring the actual binary path resolves identically
# via file-provides on every RPM-based distro regardless of how they've
# packaged/split Perl.
BuildRequires:  /usr/bin/perl

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
cargo build --release --offline --bin termland-server

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
* Thu Aug 6 2026 John Boero - 0.6.1-2
- Disable debuginfo package generation (%%global debug_package %%{nil}):
  find-debuginfo produces an empty debugsourcefiles.list for this Rust
  binary, which EL8's rpmbuild treats as a hard error.
- BuildRequires the real /usr/bin/perl binary instead of the
  perl-interpreter virtual-provide, which is a Fedora/RHEL-only naming
  convention Mageia doesn't recognize.

* Wed Aug 5 2026 John Boero - 0.6.1-1
- Fix aarch64 build failure in session isolation: getpwnam_r's buffer was
  declared Vec<i8>, but libc's buffer type follows the platform's c_char
  (signed on x86_64, unsigned on aarch64) - failed to compile at all on
  aarch64. Now Vec<libc::c_char>.
- Source1 (vendored crates, needed for the offline COPR build) is now a
  real URL (a GitHub release asset) instead of a bare filename, so a plain
  spec-URL COPR build submission works without a local SRPM upload.

* Wed Aug 5 2026 John Boero - 0.6.0-1
- Session isolation: setuid into the PAM-authenticated user after --auth,
  instead of every session running as the server's own (root) user; sessions
  now also record and enforce an owner so one authenticated user cannot
  list, attach to, or close another's session.
- Embedded SSH (russh) and QUIC transports, the latter now with Q2 (video on
  its own reliable QUIC stream, audio on datagrams) so a lost video packet
  no longer head-of-line-blocks control/input.
- Bidirectional clipboard sync, including file transfer via clipboard paste.
- Real cursor-shape sync (actual compositor cursor bitmap, not a placeholder).
- Seamless reconnect: auto-retry with backoff and reattach to the same
  session after an unexpected connection drop.
- --list-sessions / --close-session admin CLI (registry-only, no running
  server process required).
- Desktop session manager (--manager): saved multi-host connection profiles
  with a live per-host session list, resume/new/close.
- First Android client (termland-mobile-core + native Kotlin/Compose app):
  hardware keyboard/mouse as primary input, touch as secondary, audio
  playback, session resume list.

* Fri Jul 10 2026 John Boero - 0.5.0-1
- Persistent detachable sessions (X2Go/NX-style): a session keeps running on the
  server after the client disconnects and can be resumed later. Daemon-free —
  each session is a detached compositor (setsid) tracked in a filesystem
  registry under $XDG_RUNTIME_DIR/termland/sessions, so even a stateless SSH
  subsystem connection can list and resume it.
- Session control protocol: list / attach / close; session_id in SessionReady.
- Disconnect detaches (session persists); explicit close terminates it.

* Fri Jul 10 2026 John Boero - 0.4.2-1
- Version bump in lockstep with the client (client-side decode/codec/UX fixes;
  no server behavior changes)
- Shared codec crate: fixed HEVC/H.264 software decode fallback

* Fri Jul 10 2026 John Boero - 0.4.1-2
- Build offline from vendored crate deps (COPR/mock build roots have no network)

* Fri Jul 10 2026 John Boero - 0.4.1-1
- Version bump to keep server/client in lockstep (client scroll-direction fix;
  no server-side changes)

* Fri Jul 10 2026 John Boero - 0.4.0-1
- v0.4.0 release
- Multi-codec video with automatic fallback: AV1, VP9, VP8, H.265/HEVC, H.264
- Codec negotiation: client advertises decodable codecs in SessionCreate;
  server picks the best its hardware supports and announces it in SessionReady;
  every VideoFrame is codec-tagged (backward compatible with older peers)
- Encoder/decoder probe is hardware-first (open-source codecs first within each
  tier), so GPUs without AV1 encode (e.g. Volta/GV100) use hardware HEVC/H.264
  instead of falling back to software AV1
- Decoder skips the Intel QSV backend ahead of CUVID/VA-API/V4L2 to avoid
  wasting the opening frames on a doomed QSV context on non-Intel systems

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
