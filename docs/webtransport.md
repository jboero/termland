# WebTransport (browser) transport — spike

Status: **transport spike**. A browser establishes a session, opens the control
stream and completes a real `Hello`/`HelloAck` against an unmodified server.
Video, audio, input and session management are not wired up yet.

Tracking issue: [#20](https://github.com/jboero/termland/issues/20).

## Why a second listener rather than reusing `--quic`

A browser cannot talk to the raw QUIC listener. That endpoint negotiates ALPN
`termland/1` and begins Termland's own framing immediately. Browser
WebTransport is a session layered on HTTP/3: ALPN `h3`, an extended-CONNECT
request carrying `:authority`, `:path` and `Origin`, and only then streams.
No browser API opens a bare QUIC connection.

So `--webtransport` is additive. `--quic` and its Android client are untouched.

## What is shared, and what is not

Everything above the transport is shared. Once the control stream exists, its
halves are joined into one `AsyncRead + AsyncWrite` and handed to the same
`handle_session` that TCP, TLS, the SSH subsystem and raw QUIC use. Hello, PAM
auth, the session lifecycle, input and codec negotiation are the existing
implementations.

The browser side shares them too, which is the part worth spelling out. The
usual approach is to reimplement the wire format in JavaScript and then keep
the two in step — the cross-language fixtures proposed in #20 exist precisely
to catch that drift. Instead, `termland-protocol` compiles to
`wasm32-unknown-unknown`, so `crates/termland-web` runs the *same*
`TermlandCodec`, the same `Message` enum and the same serde derives as the
server. There is no second implementation to drift. A protocol change is a
recompile, not a port.

What JavaScript is left holding is genuine browser API surface: obtaining a
`WebTransport`, feeding `VideoDecoder`, painting a canvas.

## Origin checking

Browser requests carry an `Origin`; native clients do not, and a page cannot
suppress its own. The listener therefore treats a **missing** origin as "not a
browser" and allows it, and an origin that is **present** must appear in
`--webtransport-origin`.

The allowlist is empty by default, which refuses every browser. That is
deliberate. Without it, any page a user visits could open a session to a
Termland server on their network — and on a server running without `--auth`,
create and drive a desktop session on it.

The header is read case-insensitively rather than through
`SessionRequest::origin()`, which looks up the exact lowercase key in a map
that does no case folding. A browser always sends it lowercase, so this is
belt-and-braces, but a check that depends on the peer's spelling is not much of
a check.

## Certificates

WebTransport is secure-context only and browsers offer no equivalent of the
native client's `--accept-invalid-certs`. Two deployments work:

1. **A normally trusted certificate** — pass `--tls-cert`/`--tls-key`. Nothing
   else is needed and this is the production path.
2. **`serverCertificateHashes`** — for development and LAN use. Browsers accept
   a hash only for a certificate valid **two weeks or less**, so the server's
   ordinary long-lived self-signed certificate cannot be reused here. With no
   certificate configured, `--webtransport` mints a 13-day one and logs its
   SHA-256 for pasting into the client.

Origin allowlisting applies either way.

## Ports

`--webtransport-port` defaults to `--port + 1`. HTTP/3 and raw QUIC are both
UDP but negotiate different ALPN, so one socket cannot serve both.

## Testing

| Test | Covers |
|---|---|
| `crates/termland-server/src/webtransport.rs` unit tests | origin matching, header casing, the closed-by-default rule |
| `crates/termland-server/tests/webtransport_handshake.rs` | the listener end-to-end via a Rust WebTransport client, including origin rejection |
| `web/test-browser.sh` | a real Chrome completing the handshake |

The Rust client tests can send an arbitrary `Origin` — including none — which a
browser can never do, so they cover the rejection paths a browser cannot reach.
They do **not** show that a browser interoperates; only `test-browser.sh` does.

One trap worth recording: driving headless Chrome with
`--virtual-time-budget` **breaks this**. It fast-forwards timers, the QUIC
handshake never completes, and the symptom is a session that appears
server-side while the browser's `ready` promise hangs forever with no error.
`test-browser.sh` uses wall-clock time and reports its result by fetching a URL,
so the outcome is visible without guessing when the handshake finished.

## Not done

- **Media planes.** `handle_session` is called with `None` for the QUIC
  connection, so video and audio travel as CBOR on the control stream — the
  pre-Q2 arrangement TCP still uses. Splitting them onto a WebTransport uni
  stream and datagrams needs the `Option<quinn::Connection>` coupling in
  `run_session` generalised first.
- **WebCodecs decode, input, session management.** The `web-sys` bindings for
  `VideoDecoder` compile (checked), but nothing is wired up.
- **Audio timestamps.** Q2's 5-byte audio datagram header carries sample rate
  and channels but not `AudioChunk.timestamp_us`, which `EncodedAudioChunk`
  requires. That header needs extending or a WebTransport-specific one, without
  breaking the Android Q2 reader.
- **Safari.** Not supported, and not targeted.
