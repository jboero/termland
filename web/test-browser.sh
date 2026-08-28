#!/usr/bin/env bash
#
# End-to-end browser check: start a server with the WebTransport listener,
# serve the web client, drive a real Chrome at it, and confirm the
# handshake completed.
#
# This is the only test that proves a *browser* interoperates. The Rust
# integration tests cover the listener and origin checks, but a Rust
# WebTransport client is not a browser.
#
#   ./web/build.sh && ./web/test-browser.sh
#
set -euo pipefail
cd "$(dirname "$0")/.."

# chromium-browser is what Fedora (and so the CI container) installs; the
# other two are what a developer machine is likely to have.
CHROME=${CHROME:-$(command -v chromium-browser \
                || command -v google-chrome \
                || command -v chromium \
                || true)}
[ -n "$CHROME" ] || { echo "no chromium/chrome found; set CHROME=..." >&2; exit 1; }
[ -f web/dist/index.js ] || { echo "run ./web/build.sh first" >&2; exit 1; }

WORK=$(mktemp -d)
SERVER_PORT=28810 WT_PORT=28811 HTTP_PORT=8099
ORIGIN="http://localhost:$HTTP_PORT"
cleanup() {
  [ -n "${SRV:-}" ] && kill "$SRV" 2>/dev/null || true
  [ -n "${WWW:-}" ] && kill "$WWW" 2>/dev/null || true
  rm -rf "$WORK"
}
trap cleanup EXIT

./target/debug/termland-server --bind 127.0.0.1 --port "$SERVER_PORT" \
  --webtransport --webtransport-port "$WT_PORT" \
  --webtransport-origin "$ORIGIN" > "$WORK/server.log" 2>&1 &
SRV=$!
sleep 3

HASH=$(grep -oE '([0-9a-f]{2}:){31}[0-9a-f]{2}' "$WORK/server.log" | head -1)
[ -n "$HASH" ] || { echo "server printed no certificate hash:" >&2; cat "$WORK/server.log" >&2; exit 1; }

cat > web/headless.html <<HTML
<!doctype html><meta charset="utf-8"><div id="r">pending</div>
<script type="module">
  const beacon = (s) => { document.getElementById('r').textContent = s;
                          fetch('/RESULT/' + encodeURIComponent(s.slice(0,200))).catch(()=>{}); };
  window.addEventListener('unhandledrejection', e => beacon('REJECTION ' + e.reason));
  import { TermlandClient } from './dist/index.js';
  const client = new TermlandClient(
    { url: 'https://127.0.0.1:$WT_PORT/termland', certHashHex: '$HASH' },
    (ev) => {
      if (ev.type === 'hello') beacon('OK server=' + ev.server_name + ' auth=' + ev.auth_required);
      if (ev.type === 'error') beacon('FAIL ' + ev.error);
    },
  );
  client.start().catch(e => beacon('FAIL ' + e));
</script>
HTML
trap 'rm -f web/headless.html; cleanup' EXIT

( cd web && python3 -m http.server "$HTTP_PORT" --bind 127.0.0.1 ) > "$WORK/www.log" 2>&1 &
WWW=$!
sleep 2

# Deliberately NOT --virtual-time-budget. It fast-forwards timers, which
# breaks the QUIC handshake: the session is established server-side and the
# browser's ready promise never resolves. Wall-clock time it is.
timeout 60 "$CHROME" --headless --disable-gpu --no-sandbox \
  --user-data-dir="$WORK/profile" "$ORIGIN/headless.html" >/dev/null 2>&1 &
CHROME_PID=$!
sleep 25
kill "$CHROME_PID" 2>/dev/null || true

RESULT=$(grep -oE 'GET /RESULT/[^ ]*' "$WORK/www.log" | tail -1 | sed 's|GET /RESULT/||' \
         | python3 -c 'import sys,urllib.parse; print(urllib.parse.unquote(sys.stdin.read().strip()))')

echo "browser: ${RESULT:-<no result>}"
grep -q "Client hello: termland-web" "$WORK/server.log" \
  && echo "server:  received Hello from termland-web" \
  || { echo "server:  never saw the Hello" >&2; tail -5 "$WORK/server.log" >&2; exit 1; }

case "$RESULT" in
  OK*) echo "PASS" ;;
  *)   echo "FAIL" >&2; exit 1 ;;
esac
