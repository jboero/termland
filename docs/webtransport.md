# WebTransport (browser) transport

Status: **experimental**. Tracking: [#20](https://github.com/jboero/termland/issues/20).

A browser opens an HTTP/3 WebTransport session, speaks the existing control
protocol on a bidirectional stream, and receives Q2 video on a server-opened
unidirectional stream for WebCodecs. Audio is not sent on this path.

## Why a second listener

`--quic` negotiates ALPN `termland/1` and starts Termland framing immediately.
Browser WebTransport is a session on HTTP/3: ALPN `h3`, an extended-CONNECT
request, then streams. No browser API opens a bare QUIC connection, so
`--webtransport` is additive. `--quic` and the Android client are unchanged.

## Architecture

```
Browser (web/src library + web/app.js demo)
  │  HTTP/3 WebTransport  (`--webtransport`, default port = --port + 1)
  ├─ bidi stream  →  handle_session (Hello / auth / Session*)
  └─ uni stream   ←  Q2 video (same 18-byte header as native QUIC)
```

The protocol client is TypeScript because WebTransport and WebCodecs are
JavaScript APIs. `web/src/` is the embeddable client; `web/index.html` plus
`web/app.js` is the sample UI. Fixtures in `web/fixtures/` pin CBOR both ways
against Rust.

`handle_session` takes a `MediaConnection` so `run_session` can open a video
uni stream on either UDP listener.

Static files are not embedded in `termland-server`.

## Running

```
./web/build.sh
python3 -m http.server 8080 --directory web
termland-server --webtransport --webtransport-origin http://localhost:8080
```

Open `http://localhost:8080/`. If the server minted a development certificate
(no `--tls-cert`), paste the SHA-256 it logged.

## Origin, path, certificates

A missing `Origin` is treated as a non-browser and allowed. A present origin
must appear in `--webtransport-origin`. The allowlist is empty by default:
without that, any page a user visits could open a session to a Termland
server on their network — and, without `--auth`, create and drive a desktop.

`:path` must be `/termland` (trailing slash and query string folded).

WebTransport is secure-context only:

1. A normally trusted certificate — `--tls-cert` / `--tls-key`.
2. `serverCertificateHashes` for development. Browsers accept a hash only for
   certificates valid two weeks or less, so with no certificate configured
   `--webtransport` mints a 13-day one and logs its SHA-256.

## Ports

`--webtransport-port` defaults to `--port + 1`. HTTP/3 and raw QUIC are both
UDP but negotiate different ALPN, so one socket cannot serve both.

## Browser compatibility

Chromium is the primary target (`web/test-browser.sh`). Recent Firefox is
expected to work. Safari is not targeted.

`VideoDecoder.isConfigSupported()` is probed at connect time. FFmpeg packets
are used as an elementary stream (no `description` box).

## Testing

Origin and path checks are unit-tested in `webtransport.rs`. Integration
tests cover the HTTP/3 handshake and a real Q2 keyframe.
`termland-protocol` `web_cross_language` plus `web/` vitest pin the
TypeScript wire format.

Headless Chrome `--virtual-time-budget` fast-forwards timers and hangs the
QUIC handshake; `test-browser.sh` uses wall-clock time.

## Not done

- **Audio.** Q2's 5-byte datagram header has no `AudioChunk.timestamp_us`,
  which `EncodedAudioChunk` requires. Extending that header must not break
  the Android Q2 reader.
- Clipboard, cursor bitmaps, file transfer, window list UI, touch.
- Embedding the static build in `termland-server`.
- Safari.
