#!/bin/bash
set -e

VERSION=0.3.0
SRCDIR="$(cd "$(dirname "$0")/.." && pwd)"
SPECDIR="$SRCDIR/packaging"

mkdir -p ~/rpmbuild/{SOURCES,SPECS,BUILD,RPMS,SRPMS}

echo "==> Creating source tarball..."
# Resolve symlinks and get the real directory path + its parent
REALDIR="$(realpath "$SRCDIR")"
PARENT="$(dirname "$REALDIR")"
DIRNAME="$(basename "$REALDIR")"

tar czf ~/rpmbuild/SOURCES/termland-${VERSION}.tar.gz \
    -C "$PARENT" \
    --transform="s,^${DIRNAME},termland-${VERSION}," \
    --exclude='target' \
    --exclude='.git' \
    -h \
    "${DIRNAME}"

echo "   $(tar tzf ~/rpmbuild/SOURCES/termland-${VERSION}.tar.gz | wc -l) files in tarball"

echo "==> Building server and client RPMs in parallel..."
rpmbuild -ba "$SPECDIR/termland-server.spec" &
PID_SERVER=$!
rpmbuild -ba "$SPECDIR/termland-client.spec" &
PID_CLIENT=$!

FAIL=0
wait $PID_SERVER || { echo "!!! Server RPM build failed"; FAIL=1; }
wait $PID_CLIENT || { echo "!!! Client RPM build failed"; FAIL=1; }

if [ $FAIL -eq 0 ]; then
    echo ""
    echo "==> RPMs built successfully:"
    ls -1 ~/rpmbuild/RPMS/x86_64/termland-*${VERSION}*.rpm
    echo ""
    echo "Install with:"
    echo "  sudo dnf install ~/rpmbuild/RPMS/x86_64/termland-{server,client}-${VERSION}*.rpm"
fi

exit $FAIL
