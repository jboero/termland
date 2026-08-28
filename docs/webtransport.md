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
Browser
  web/app.js          UI (sidebar, session list, canvas chrome)
  web/src             WebTransport, WebCodecs, input, reconnect
  crates/termland-web wasm protocol codec (same termland-protocol as the server)
    │  HTTP/3 WebTransport  (`--webtransport`, default port = --port + 1)
    ├─ bidi stream  →  handle_session (Hello / auth / Session*)
    └─ uni stream   ←  Q2 video (same 18-byte header as native QUIC)
```

`web/src/` is the embeddable client; `index.html` + `app.js` is the sample UI.
The wasm crate is the protocol codec so encode/decode cannot drift.
`handle_session` takes a `MediaConnection` so both UDP listeners open Q2 video
the same way. Static files are not embedded in `termland-server`.

## Running

```
./web/build.sh
python3 -m http.server 8080 --directory web
termland-server --webtransport --webtransport-origin http://localhost:8080
```

`./web/build.sh` builds `crates/termland-web` to `web/pkg/` (`wasm-bindgen
--target web`) and the TypeScript client to `web/dist/`. Serve the `web/`
directory so `app.js` can import both.

Open `http://localhost:8080/`. If the server minted a development certificate
(no `--tls-cert`), paste the SHA-256 it logged.

Requires a `wasm32-unknown-unknown` target and `wasm-bindgen-cli` 0.2.114
(`cargo install wasm-bindgen-cli --version 0.2.114`).

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
`termland-protocol` still builds for `wasm32-unknown-unknown`. Host tests in
`crates/termland-web` round-trip frames without a browser. `web/` vitest
byte-compares wasm `encodePayload` against `web/fixtures/from-rust/`.

Headless Chrome `--virtual-time-budget` fast-forwards timers and hangs the
QUIC handshake; `test-browser.sh` uses wall-clock time.

A frozen background tab is recovered in the TypeScript client (rebuild
decoder, release stuck buttons, re-attach after a long hide).

## Not done

- **Audio.** Q2's 5-byte datagram header has no `AudioChunk.timestamp_us`,
  which `EncodedAudioChunk` requires. Extending that header must not break
  the Android Q2 reader. A WebTransport client that asks for audio is
  refused at negotiation (no Pulse capture thread).
- Clipboard, cursor bitmaps, file transfer, window list UI, touch.
- Embedding the static build in `termland-server`.
- Safari.

## Two browser clients

There are two, deliberately, and they talk to the same listener:

| | `web/src` + `web/app.js` | `crates/termland-web-client` |
|---|---|---|
| Page | `web/index.html` | `web/wasm/index.html` |
| Build | `./web/build.sh` | `./web/build-wasm.sh` |
| Browser test | `./web/test-browser.sh` | `./web/test-browser-wasm.sh` |
| Protocol | wasm (`crates/termland-web`) | wasm (`termland-protocol` directly) |
| Session logic, input, video pump | TypeScript | Rust |
| Client name on the wire | `termland-web` | `termland-wasm` |

The Rust client exists because the remaining TypeScript still restates things
the server already knows — most visibly the evdev scancode table, which was
maintained by hand in both `termland-client/src/display.rs` and
`web/src/input.ts`. That table now lives once, in
`termland_protocol::input::browser_code_to_evdev`, and the Rust client uses it
directly.

### What "Rust client" does and does not mean

It does not mean no JavaScript. Every WebTransport, WebCodecs, canvas and DOM
call is reached through wasm-bindgen, which *generates* JavaScript glue —
about 42 KB of it next to a 600 KB `.wasm`. What goes away is the hand-written
TypeScript layer that had to be kept in step with the Rust by hand, not the
script layer itself.

It is also not a performance change, and should not be adopted expecting one.
Decode and paint are native browser work in both clients, and the per-frame
work either client does is small. If anything the Rust client copies slightly
more: frame bytes cross into wasm linear memory and back out to reach
WebCodecs, where the TypeScript client hands the browser's own buffer straight
to `EncodedVideoChunk`.

### Painting without requestAnimationFrame

The Rust client draws from the `VideoDecoder` output callback rather than
parking the frame for `requestAnimationFrame`. rAF stops firing in a
backgrounded tab, which is what stranded the TypeScript client's paint loop
until it grew explicit recovery; with no latch, there is nothing to unstick. A
decoder the browser tears down during a freeze is rebuilt on the next keyframe.

### Verifying it end to end

`./web/test-browser-wasm.sh` drives a real headless browser through the
handshake — that is what CI runs, since it needs no compositor. Adding
`--with-video` also creates a session and samples the canvas, which exercises
the whole path:

```console
$ ./web/build-wasm.sh && ./web/test-browser-wasm.sh --with-video
browser: OK painted nonblack=230340 distinct=100 | session s3d8e116fcc49 — 640x360 H.264
server:  received Hello from termland-wasm
PASS
```

A solid fill would count as painted, so the check also requires several
distinct colours. Sessions the run creates are closed on the way out, including
on failure.
