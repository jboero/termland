#!/bin/bash
# Generate the vendored-crates tarball referenced as Source1 in the RPM specs.
#
# COPR/mock build roots have no network access, so the specs build cargo
# --offline against a bundled `vendor/` directory. Run this before rpmbuild
# (or before uploading an SRPM to COPR) to (re)create the tarball whenever
# Cargo.lock changes.
#
#   packaging/make-vendor-tarball.sh [OUTDIR]   # default: ~/rpmbuild/SOURCES
set -e

VERSION=0.5.0
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
