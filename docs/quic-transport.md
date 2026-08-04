# QUIC transport — design

Status: **Q1 shipped** (drop-in single-stream transport, server `--quic`/
`--quic-port` + mobile core `Transport::Quic`). **Q2 (split planes) not
started.** Benefits desktop over WAN, but the motivation is mobile:
lossy/roaming links, background/foreground churn, fast resume. Pairs directly
with v0.5 session persistence. Verified with a real integration test
(spawns the actual server, opens a real QUIC connection + bidi stream, sends
`Hello`, receives `HelloAck`) plus an independent manual client run — not
just "the code compiles."

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
| Control (Hello/auth/`Session*`, resize, cursor mode) | one **bidi** stream | reliable, ordered |
| Video (frames) | **uni** stream server→client (Q1); **datagrams** for inter-frames + reliable keyframes (Q2) | Q1 reliable; Q2 partial |
| Audio (Opus) | **datagrams** (loss-tolerant, low latency) or a uni stream | unreliable ok |
| Input (key/pointer/text) | one **bidi/uni** stream client→server | reliable, ordered |

The control stream reuses the exact CBOR `Message` framing we already have —
`TermlandCodec` runs over a QUIC stream unchanged.

## Phasing (keep it incremental)

- **Q1 — QUIC as a drop-in byte transport.** Run the *entire* existing protocol
  over a single bidi QUIC stream (swap the byte transport, nothing else). This
  alone buys connection migration + 0-RTT + loss resilience with a small,
  low-risk change. Add a `--quic`/UDP listener on the server next to TCP; the
  client core gains a QUIC transport alongside TLS/SSH.
- **Q2 — split the planes.** Move video/audio onto their own streams/datagrams
  for HOL-free A/V, and adopt app-level pacing/FEC for the datagram video path
  (Moonlight-style) if we want to ride out cellular loss without stalls.

## Server refactor notes

Today `handle_session` runs over one `Framed<T, TermlandCodec>`. Q1 keeps that
(T = a QUIC stream) — minimal change behind a small transport trait alongside the
existing TCP/subsystem entry points. Q2 introduces a `Transport` abstraction that
exposes control + per-plane channels, and `run_session` writes video/audio to
their own streams. Auth stays app-level (`AuthRequest`/`Response`) over the
control stream, or moves to a QUIC client certificate later.

## Auth / security

QUIC carries TLS 1.3. MVP: self-signed server cert (like the TCP+TLS path today,
with `--accept-invalid-certs` / pinning on mobile) + app-level PAM auth over the
control stream. Later: client certificates, and pinning the server key in the
mobile Keychain/Keystore on first connect (TOFU).

## Where it sits in the roadmap

Was a v0.4 stretch item; **pulled forward to accompany the mobile M4** because it
is where QUIC's migration/0-RTT/loss properties matter most. Desktop WAN users
get the win for free.
