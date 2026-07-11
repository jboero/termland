# Termland mobile clients (Android + iOS) — design

Status: **plan** (not started). Target: a milestone after v0.5.

Goal: a native-feeling touch client for phones/tablets that connects to a
Termland server, resumes persistent sessions, and decodes video on the device's
hardware — reusing the Rust protocol and codec-negotiation logic we already
have, not reimplementing the wire format.

Mobile is where two things we already built pay off enormously:

- **Codec negotiation** (`supported_codecs` + per-frame codec tag): mobile
  decoders are a patchwork (see the matrix below). The client advertises exactly
  what the device can decode and the server picks accordingly — no guessing.
- **Persistent sessions** (v0.5): mobile links drop constantly (backgrounding,
  Wi-Fi↔cellular, tunnels). Auto-detach on disconnect + one-tap resume is the
  killer feature, and the server already does it.

## Architecture: shared Rust core + thin native UI

```
┌───────────────────────── Android (Kotlin/Compose) ─┐   ┌─── iOS (Swift/SwiftUI) ───┐
│  UI · touch input · soft keyboard · session list   │   │  UI · touch · keyboard    │
│  MediaCodec decode → Surface/SurfaceView           │   │  VideoToolbox → Metal     │
└──────────────▲── encoded packets ──┬───────────────┘   └──────▲────────┬───────────┘
               │  (codec-tagged)     │ decoded frames           │        │
        ┌──────┴─────────────────────┴──────────────────────────┴────────┴──────┐
        │  termland-mobile-core (Rust, exposed via UniFFI → Kotlin + Swift)      │
        │  transport (TLS / embedded SSH) · handshake · SessionCreate/Attach/    │
        │  List/Close · codec negotiation · packet demux · Opus decode · input   │
        │  event construction.  Reuses termland-protocol as-is.                  │
        └────────────────────────────────────────────────────────────────────────┘
```

Key decision: **the mobile core does NOT bundle FFmpeg.** The desktop client
decodes with FFmpeg; on mobile that's huge, battery-hostile, and licensing-messy.
Instead the core does protocol + negotiation + packet routing and hands
**codec-tagged encoded packets** up to the platform decoder (MediaCodec /
VideoToolbox). The core is exposed with **UniFFI** (one Rust API → generated
Kotlin and Swift bindings), keeping the native layers thin.

Reused as-is: `termland-protocol` (serde/CBOR — portable), codec negotiation,
session control (list/attach/close), `--codec` forcing. Replaced per-platform:
video decode (FFmpeg → MediaCodec/VideoToolbox), render (winit/softbuffer →
Surface/Metal), audio out (cpal → AudioTrack/AVAudioEngine).

## Video decode matrix (drives what each client advertises)

| Codec | Android MediaCodec | iOS VideoToolbox |
|-------|--------------------|------------------|
| H.264 | ✅ universal (HW)  | ✅ universal (HW) |
| H.265/HEVC | ✅ very common (HW) | ✅ modern devices (HW) |
| VP9   | ✅ common (HW/SW)  | ❌ not supported |
| VP8   | ✅ common          | ❌ not supported |
| AV1   | ⚠️ HW on newer SoCs (Tensor/SD8g2+, else SW) | ⚠️ HW on A17 Pro / M3+, else ❌ |

The client queries `MediaCodecList` / `VTIsHardwareDecodeSupported` at startup
and advertises only the decodable set. On iOS that means VP8/VP9 are never
offered, so the GV100 server naturally serves **HEVC** (its hardware path) — the
negotiation we built handles this with zero special-casing.

## Transport (the genuinely hard part on mobile)

Desktop spawns `ssh -s host termland`. Mobile has no `ssh` binary and app
sandboxes forbid spawning processes. Options, in recommended order:

1. **Direct TCP + TLS** (already supported server-side) — MVP. Connect to
   `host:7867`, TLS + PAM auth. Needs the TCP port reachable (firewall).
2. **Embedded SSH subsystem** via pure-Rust **`russh`** in the core — opens an
   SSH connection + `termland` subsystem channel in-process, preserving the
   zero-config UX (existing sshd, keys, no extra port). Keys/passwords stored in
   Android Keystore / iOS Keychain. Recommended for parity; `russh`
   cross-compiles to both platforms.
3. **QUIC / WebTransport** (roadmap item) — ideal for lossy mobile links
   (0-RTT reconnect, no head-of-line blocking). Future; pairs naturally with the
   session-resume UX.

## Touch input mapping

- **Direct mode:** tap = left click at point; long-press = right click; drag =
  press-move-release.
- **Trackpad mode:** relative pointer (better for desktop targets); two-finger
  tap = right click; two-finger scroll = wheel.
- **Pinch** = zoom → reuse the Ctrl-scale path (scale the frame locally) vs. a
  server-side `SessionResize` toggle.
- **Keyboard:** soft keyboard + Bluetooth keyboards → `KeyEvent` (needs a
  scancode/keymap layer; IME/compose is a later refinement).
- **Rotation / resolution:** send device size on connect; on rotate, `SessionResize`
  or local scale. Persistence means rotate/background/reconnect just resumes.

## Session-management UI (leans on v0.5)

- Home: saved host profiles; each shows resumable sessions (`SessionList`).
- Tap a session → attach/resume; "New session" button; long-press → close.
- Auto-detach on background/network-loss + one-tap resume is the flagship UX.

## Phasing

- **M1 — core:** `termland-mobile-core` (protocol, connection, session control,
  packet demux, Opus, input), UniFFI bindings, TCP+TLS transport. Headless
  test harness on desktop.
- **M2 — Android:** Compose UI, MediaCodec→Surface, touch + soft keyboard,
  session list/resume. MVP codecs: H.265/H.264 (+VP9 where present).
- **M3 — iOS:** SwiftUI, VideoToolbox→Metal (AVSampleBufferDisplayLayer),
  same feature set (HEVC/H.264).
- **M4 — parity+:** embedded `russh` subsystem transport; audio; trackpad mode;
  AV1 where HW-supported; QUIC.

## Risks / open questions

- **Apple:** needs a paid developer account + code signing + TestFlight; bundling
  crypto/SSH is fine (declare export compliance).
- **AV1 variance:** advertise only when HW-supported; otherwise HEVC/H.264.
- **Battery/latency:** hardware decode is mandatory; QUIC materially helps on
  cellular.
- **Text input:** IME/autocorrect → key events is crude; a text-injection path
  is a later item.
- **Framework choice:** native (Compose/SwiftUI) + shared Rust core via UniFFI is
  recommended over Flutter/RN for native input latency and platform-decoder
  access; a single KMP/Compose-Multiplatform UI is a possible alternative if we
  want one UI codebase, at some latency/decoder-integration cost.
