#!/usr/bin/env bash
#
# End-to-end browser check for the Rust/wasm client, mirroring
# test-browser.sh for the TypeScript one: start a server with the
# WebTransport listener, serve the page, drive a real browser at it, and
# confirm the handshake completed.
#
# The Rust integration tests drive the listener with a wtransport client,
# which is not a browser and cannot exercise wasm-bindgen glue, WebCodecs
# or serverCertificateHashes.
#
#   ./web/build-wasm.sh && ./web/test-browser-wasm.sh
#
# With --with-video it also creates a real session and samples the canvas, so
# the whole path (WebTransport -> Q2 -> WebCodecs -> canvas) is checked rather
# than just the handshake. That needs a compositor and an encoder on this host,
# which is why it is opt-in and not what CI runs.
#
set -euo pipefail
cd "$(dirname "$0")/.."

WITH_VIDEO=0
[ "${1:-}" = "--with-video" ] && WITH_VIDEO=1

# chromium-browser is what Fedora (and so the CI container) installs; the
# other two are what a developer machine is likely to have.
CHROME=${CHROME:-$(command -v chromium-browser \
                || command -v google-chrome \
                || command -v chromium \
                || true)}
[ -n "$CHROME" ] || { echo "no chromium/chrome found; set CHROME=..." >&2; exit 1; }
[ -f web/wasm/pkg/termland_web_client.js ] || {
  echo "run ./web/build-wasm.sh first" >&2; exit 1;
}

WORK=$(mktemp -d)
SERVER_PORT=28820 WT_PORT=28821 HTTP_PORT=8098
ORIGIN="http://localhost:$HTTP_PORT"
# Sessions outlive the server process, so a --with-video run that dies partway
# would strand a desktop on the developer's machine. Reap anything this run
# created, whatever the exit path.
SESSIONS_BEFORE=$(./target/debug/termland-server --list-sessions 2>/dev/null | awk 'NR>1{print $1}' | sort)
cleanup() {
  [ -n "${SRV:-}" ] && kill "$SRV" 2>/dev/null || true
  [ -n "${WWW:-}" ] && kill "$WWW" 2>/dev/null || true
  if [ "$WITH_VIDEO" = 1 ]; then
    for s in $(./target/debug/termland-server --list-sessions 2>/dev/null | awk 'NR>1{print $1}' | sort); do
      echo "$SESSIONS_BEFORE" | grep -qx "$s" || {
        echo "[cleanup] closing session $s"
        ./target/debug/termland-server --close-session "$s" >/dev/null 2>&1 || true
      }
    done
  fi
  rm -f web/wasm/headless.html
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

# Same shape as the TypeScript harness: the page beacons its outcome back
# through the static file server's access log, which needs no extra service.
cat > web/wasm/headless.html <<HTML
<!doctype html><meta charset="utf-8">
<div id="status">pending</div><div id="sessions"></div>
<canvas id="screen" width="640" height="360"></canvas>
<script type="module">
  const WITH_VIDEO = $WITH_VIDEO;
  const beacon = (s) => fetch('/RESULT/' + encodeURIComponent(s.slice(0, 300))).catch(() => {});
  window.addEventListener('unhandledrejection', e => beacon('REJECTION ' + e.reason));
  try {
    const { default: init, WebClient } = await import('./pkg/termland_web_client.js');
    await init();
    const client = new WebClient('screen', 'status', 'sessions');
    const statusEl = document.getElementById('status');
    let asked = false;
    // The status element is the client's own output; the text is written from
    // inside wasm, so seeing it means that code ran.
    const watch = new MutationObserver(async () => {
      const t = statusEl.textContent || '';
      if (t.startsWith('error:')) { watch.disconnect(); beacon('FAIL ' + t); return; }
      if (t.startsWith('connected to')) {
        if (!WITH_VIDEO) { watch.disconnect(); beacon('OK ' + t); return; }
        if (!asked) { asked = true; client.create_session(640, 360); }
      }
      if (WITH_VIDEO && t.startsWith('session ')) {
        watch.disconnect();
        // Give the encoder time to send a keyframe and the decoder to paint.
        await new Promise(r => setTimeout(r, 8000));
        const c = document.getElementById('screen');
        const px = c.getContext('2d').getImageData(0, 0, c.width, c.height).data;
        let nonblack = 0; const colours = new Set();
        for (let i = 0; i < px.length; i += 4) {
          if (px[i] || px[i+1] || px[i+2]) nonblack++;
          if (i % 4000 === 0) colours.add(px[i] + ',' + px[i+1] + ',' + px[i+2]);
        }
        // A solid fill would be "painted" but not a desktop; require variety.
        const verdict = (nonblack > 1000 && colours.size > 3) ? 'OK' : 'FAIL';
        beacon(verdict + ' painted nonblack=' + nonblack + ' distinct=' + colours.size + ' | ' + t);
      }
    });
    watch.observe(statusEl, { childList: true, characterData: true, subtree: true });
    client.connect('https://127.0.0.1:$WT_PORT/termland', '$HASH').catch(e => beacon('FAIL ' + e));
  } catch (e) { beacon('FAIL bootstrap ' + e); }
</script>
HTML

( cd web && python3 -m http.server "$HTTP_PORT" --bind 127.0.0.1 ) > "$WORK/www.log" 2>&1 &
WWW=$!
sleep 2

# Deliberately NOT --virtual-time-budget. It fast-forwards timers, which
# breaks the QUIC handshake: the session is established server-side and the
# browser's ready promise never resolves. Wall-clock time it is.
timeout 60 "$CHROME" --headless --disable-gpu --no-sandbox \
  --user-data-dir="$WORK/profile" "$ORIGIN/wasm/headless.html" >/dev/null 2>&1 &
CHROME_PID=$!
[ "$WITH_VIDEO" = 1 ] && sleep 45 || sleep 25
kill "$CHROME_PID" 2>/dev/null || true

RESULT=$(grep -oE 'GET /RESULT/[^ ]*' "$WORK/www.log" | tail -1 | sed 's|GET /RESULT/||' \
         | python3 -c 'import sys,urllib.parse; print(urllib.parse.unquote(sys.stdin.read().strip()))')

echo "browser: ${RESULT:-<no result>}"
grep -q "Client hello: termland-wasm" "$WORK/server.log" \
  && echo "server:  received Hello from termland-wasm" \
  || { echo "server:  never saw the Hello" >&2; tail -5 "$WORK/server.log" >&2; exit 1; }

case "$RESULT" in
  OK*) echo "PASS" ;;
  *)   echo "FAIL" >&2; exit 1 ;;
esac
