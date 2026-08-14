#!/bin/bash
# Generate the vendored-crates tarball referenced as Source1 in the RPM specs.
#
# COPR/mock build roots have no network access, so the specs build cargo
# --offline against a bundled `vendor/` directory. Run this to (re)create the
# tarball whenever Cargo.lock changes, i.e. for every release.
#
# Source1 in both specs is a real URL pointing at this file as a GitHub
# release asset (https://github.com/jboero/termland/releases/download/vVERSION/...),
# not a bare filename - so COPR can fetch it itself, and a plain
# `copr-cli build <project> <spec-url>` submission (no local SRPM build/
# upload needed) works. That means this script's output must actually be
# uploaded to the matching GitHub release before submitting a COPR build:
#
#   packaging/make-vendor-tarball.sh              # -> ~/rpmbuild/SOURCES/termland-VERSION-vendor.tar.xz
#   gh release upload vVERSION ~/rpmbuild/SOURCES/termland-VERSION-vendor.tar.xz
#   copr-cli build boeroboy/mawenzy https://github.com/jboero/termland/raw/refs/heads/main/packaging/termland-server.spec
#   copr-cli build boeroboy/mawenzy https://github.com/jboero/termland/raw/refs/heads/main/packaging/termland-client.spec
#
# (Local `rpmbuild -bs`/`-ba` still works too, and still needs this file in
# ~/rpmbuild/SOURCES, same as before - the release upload is only needed for
# the URL-based COPR path.)
#
#   packaging/make-vendor-tarball.sh [OUTDIR]   # default: ~/rpmbuild/SOURCES
set -e

VERSION=0.7.0
SRCDIR="$(cd "$(dirname "$0")/.." && pwd)"
OUT="${1:-$HOME/rpmbuild/SOURCES}"
mkdir -p "$OUT"

cd "$SRCDIR"
echo "==> Vendoring crate dependencies (matches Cargo.lock)..."
cargo vendor --versioned-dirs vendor >/dev/null

echo "==> Writing $OUT/termland-${VERSION}-vendor.tar.xz ..."
tar cJf "$OUT/termland-${VERSION}-vendor.tar.xz" vendor
rm -rf vendor

echo "   done: $(du -h "$OUT/termland-${VERSION}-vendor.tar.xz" | cut -f1)"
