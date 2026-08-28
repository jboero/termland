# WebTransport (browser) transport

Status: **experimental**. A browser establishes an HTTP/3 WebTransport session,
speaks the existing control protocol, and (once a session is attached) receives
Q2 video on a server-opened unidirectional stream for WebCodecs. Audio is not
sent on this path — see below.

Tracking issue: [#20](https://github.com/jboero/termland/issues/20).

## Why a second listener rather than reusing `--quic`

A browser cannot talk to the raw QUIC listener. That endpoint negotiates ALPN
`termland/1` and begins Termland's own framing immediately. Browser
WebTransport is a session layered on HTTP/3: ALPN `h3`, an extended-CONNECT
request carrying `:authority`, `:path` and `Origin`, and only then streams.
No browser API opens a bare QUIC connection.

So `--webtransport` is additive. `--quic` and its Android client are untouched.

## Architecture

```
Browser (web/src TypeScript library + web/app.js demo)
  │  HTTP/3 WebTransport  (`--webtransport`, default port = --port + 1)
  ├─ bidi stream  →  existing handle_session (Hello / auth / Session*)
  └─ uni stream   ←  Q2 video (same 18-byte header as native QUIC)
```

Rust stays on the server. The protocol client is TypeScript because
WebTransport and WebCodecs are JavaScript APIs; compiling the protocol crate
to wasm would not remove that work. `web/src/` is the embeddable client:
WebTransport, the 7-byte `TL` frame, serde's externally-tagged CBOR, codec
probe, Q2/WebCodecs, input, and reconnect. `web/index.html` plus `web/app.js`
is the sample UI (sidebar, connecting overlay, size fields) — plain JavaScript,
not compiled, so the page can be edited by hand. It imports the built library
from `./dist/index.js`. Fixtures in `web/fixtures/` pin the wire format: Rust
encodes and TypeScript decodes, and the other way around.

```ts
import { TermlandClient, VideoPipeline, InputCapture } from 'termland-web-client';
```

or, after `./web/build.sh`, relative `from './dist/index.js'`.
`TermlandClient` emits `status`, `hello`, `session-ready`, `reconnecting`,
`error`, and `video` so a host UI can drive its own spinner.

`crates/termland-web` is the earlier Hello-only wasm spike. It is still
buildable; the page in `web/` no longer loads it.

`handle_session` takes a `MediaConnection` (`None` / `Quic` /
`WebTransport`) instead of `Option<quinn::Connection>`, so `run_session`
can open a video uni stream on either UDP listener.

## Running

```
# TypeScript library → web/dist/; demo page is index.html + app.js
./web/build.sh
python3 -m http.server 8080 --directory web

# Server. The page origin must be listed or every browser is refused.
termland-server --webtransport --webtransport-origin http://localhost:8080
```

Open `http://localhost:8080/`. If the server minted a development certificate
(no `--tls-cert`), paste the SHA-256 it logged into the page.

A disposable echo with no Termland framing:

```
cargo run -p termland-server --example webtransport_echo
```

then `web/spike/echo.html`. That is the certificate-hash / Origin check
before any protocol is involved.

The static files are **not** embedded in `termland-server`. Serving them from
the same process is a later packaging decision.

## Origin, path, certificates

Browser requests carry an `Origin`; native clients do not, and a page cannot
suppress its own. A **missing** origin is treated as "not a browser" and
allowed. A **present** origin must appear in `--webtransport-origin`.

The allowlist is empty by default, which refuses every browser. Without that,
any page a user visits could open a session to a Termland server on their
network — and on a server running without `--auth`, create and drive a
desktop session on it.

`:path` must be `/termland` (trailing slash and query string folded). Anything
else is 404.

WebTransport is secure-context only. Two deployments work:

1. **A normally trusted certificate** — `--tls-cert` / `--tls-key`. Production.
2. **`serverCertificateHashes`** — development and LAN. Browsers accept a hash
   only for a certificate valid **two weeks or less**, so the server's ordinary
   long-lived self-signed certificate cannot be reused. With no certificate
   configured, `--webtransport` mints a 13-day one and logs its SHA-256.

## Ports

`--webtransport-port` defaults to `--port + 1`. HTTP/3 and raw QUIC are both
UDP but negotiate different ALPN, so one socket cannot serve both.

## Browser compatibility

| Browser | WebTransport | WebCodecs `VideoDecoder` | This client |
|---|---|---|---|
| Chromium / Chrome | yes | yes | primary target; `web/test-browser.sh` |
| Firefox (recent) | yes | yes | expected to work; not in CI |
| Safari | incomplete | incomplete | not targeted |

`VideoDecoder.isConfigSupported()` is probed at connect time. Only codecs that
succeed are advertised in `SessionCreate`.

FFmpeg's packets are used as an elementary stream (AV1 OBUs, VP9 frames,
Annex-B H.264/H.265). WebCodecs configs are given **without** a `description`
box. If an encoder emitted avcC/hvcC instead, Chromium would reject the chunk
— advertise a different codec rather than guessing a string.

## Testing

| Test | Covers |
|---|---|
| `webtransport.rs` unit tests | origin matching, path allowlist, header casing, closed-by-default |
| `tests/webtransport_handshake.rs` | HTTP/3 session, Hello/HelloAck, origin and path rejection |
| `tests/webtransport_q2.rs` | real compositor keyframe on the Q2 uni stream |
| `termland-protocol` `web_cross_language` | Rust↔TypeScript CBOR fixtures |
| `web/` vitest | framing (partial headers, 16 MiB cap, LE lengths), Q2 header, evdev map |
| `web/test-browser.sh` | a real Chrome completing the handshake |

One trap: headless Chrome `--virtual-time-budget` fast-forwards timers, the
QUIC handshake never completes, and the browser's `ready` promise hangs with
no error. `test-browser.sh` uses wall-clock time.

## Not done

- **Audio.** Q2's 5-byte datagram header carries sample rate and channels but
  not `AudioChunk.timestamp_us`, which `EncodedAudioChunk` requires. This path
  does not send audio rather than inventing a clock. Extending that header
  must not break the Android Q2 reader.
- **Clipboard, cursor bitmaps, file transfer, window list UI, touch.**
- **Embedding the static build in `termland-server`.**
- **Safari.**
