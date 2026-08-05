# QUIC transport — design

Status: **Q1 shipped and superseded by Q2.** **Q2 (split planes) shipped**:
video moved to its own server-opened QUIC uni stream (reliable, fixed
18-byte binary header + raw frame bytes — not CBOR, not datagram-fragmented)
and audio moved to QUIC datagrams (one Opus chunk per datagram, 5-byte
header). The control plane (Hello/auth/`Session*`/input/clipboard/cursor)
is unchanged: one bidi stream, the same CBOR `Message`/`TermlandCodec`
framing as Q1 and every other transport. Since `termland-mobile-core` is the
only QUIC client, Q2 replaces Q1's single-stream wire contract outright
rather than versioning/negotiating between the two — `--quic` now means Q2.
Benefits desktop over WAN, but the motivation is mobile: lossy/roaming
links, background/foreground churn, fast resume. Pairs directly with v0.5
session persistence. Verified with real integration tests (spawns the
actual server, opens a real QUIC connection, does the full
control-stream handshake plus SessionCreate, and confirms real encoded
`VideoFrame`-shaped bytes arrive on the video uni stream in the Q2 header
format) plus an independent manual client run — not just "the code
compiles."

## Why QUIC (vs the TCP/SSH transports today)

- **Connection migration.** A QUIC connection is identified by a connection ID,
  not the 4-tuple, so it survives the client's IP changing (Wi-Fi↔cellular,
  NAT rebind). On mobile the connection often *doesn't even drop* on a network
  switch — and when it does, v0.5 resume takes over.
- **0-RTT resume.** Re-attach to a session on the first packet after a
  reconnect, instead of a TCP+TLS round-trip dance.
- **No head-of-line blocking across streams.** A lost video packet doesn't stall
  audio, input, or control. TCP blocks everything behind the loss.
- **TLS 1.3 is built in.** One handshake for encryption + transport; replaces the
  separate rustls-over-TCP path.

## Library

**`quinn`** — pure-Rust QUIC on tokio, cross-compiles to Android/iOS, integrates
with the existing rustls stack. Server and the mobile Rust core share it.

## Stream model

Map the protocol onto QUIC streams so each plane is independent:

| Plane | QUIC object | Reliability |
|-------|-------------|-------------|
| Control (Hello/auth/`Session*`, resize, cursor mode, input, clipboard) | one **bidi** stream | reliable, ordered |
| Video (frames) | **uni** stream, server→client, own dedicated stream since Q2 | reliable, ordered |
| Audio (Opus) | **datagrams**, one Opus chunk per datagram, since Q2 | unreliable (loss-tolerant, low latency) |

Q2 keeps video fully reliable on its own uni stream rather than datagrams —
splitting individual frames across datagrams with FEC/pacing so a lost
fragment only costs part of a frame (Moonlight-style) is real added
complexity around loss-tolerant keyframe/interframe reconstruction, and is
deliberately left as a later refinement (see Phasing below) rather than
folded into Q2. What Q2 already buys is the important part: video no longer
shares a stream with control/input, so a slow/lost video packet can't
head-of-line-block them.

The control stream reuses the exact CBOR `Message` framing we already have —
`TermlandCodec` runs over a QUIC stream unchanged.

## Phasing (keep it incremental)

- **Q1 — QUIC as a drop-in byte transport.** Run the *entire* existing protocol
  over a single bidi QUIC stream (swap the byte transport, nothing else). This
  alone buys connection migration + 0-RTT + loss resilience with a small,
  low-risk change. Add a `--quic`/UDP listener on the server next to TCP; the
  client core gains a QUIC transport alongside TLS/SSH.
- **Q2 — split the planes (shipped).** Video moved to its own reliable
  server-opened uni stream; audio moved to unreliable datagrams. Control
  (including input) stays on the one bidi stream, unchanged. This alone
  removes video/audio from the control plane's head-of-line, without
  touching video's reliability model.
- **Q3 (future, not started) — datagram video + FEC.** Split individual
  video frames across datagrams with app-level pacing/forward error
  correction (Moonlight-style) so a lost packet costs part of a frame
  instead of stalling the whole stream waiting for retransmission — for
  riding out cellular loss without visible stalls. Meaningfully more complex
  than Q2 (keyframe/interframe-aware reconstruction, reordering, loss
  concealment) and only worth it once Q2's simpler reliable-stream video is
  shown to actually stall under real cellular loss.

## Server refactor notes

`handle_session`/`run_session` still run the control plane over one
`Framed<T, TermlandCodec>` exactly as Q1 left it (T = a QUIC stream for
`--quic`, a TCP/TLS socket or SSH channel otherwise) — Q2 didn't need a new
`Transport` abstraction to add the video/audio planes, just one extra
parameter: `handle_session`/`run_session` take an `Option<quinn::Connection>`
(`None` for every non-QUIC transport, a one-line no-op change at each of
their call sites) that `run_session` uses to `open_uni()` the video stream
and hold the `Connection` for `send_datagram()` (audio), once the encoder
has produced a first frame. Everywhere else in `run_session`'s existing
session/control-plane lifecycle (auth, registry, input threads, clipboard,
cursor watch, resize) is untouched. Auth stays app-level
(`AuthRequest`/`Response`) over the control stream, or moves to a QUIC
client certificate later.

## Auth / security

QUIC carries TLS 1.3. MVP: self-signed server cert (like the TCP+TLS path today,
with `--accept-invalid-certs` / pinning on mobile) + app-level PAM auth over the
control stream. Later: client certificates, and pinning the server key in the
mobile Keychain/Keystore on first connect (TOFU).

## Where it sits in the roadmap

Was a v0.4 stretch item; **pulled forward to accompany the mobile M4** because it
is where QUIC's migration/0-RTT/loss properties matter most. Desktop WAN users
get the win for free.
