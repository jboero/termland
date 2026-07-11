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

## Keyboard & text input (the hard problem)

Desktop input injection sends **scancodes** (`KeyEvent` → evdev scancode →
virtual keyboard, on a fixed xkb keymap). That's fine for physical keys, but
mobile soft keyboards + IMEs produce **Unicode text** with autocorrect,
prediction, emoji, and CJK composition — none of which map to fixed scancodes.
So we need *two* input channels, and a smarter server-side injector.

### Two channels

1. **Committed text** (what the IME finalizes) → a new `TextInput { text: String }`
   message (already-composed Unicode). Handles Latin+autocorrect, emoji, CJK.
2. **Editing / navigation / shortcut keys** (Backspace, Enter, Tab, Esc, arrows,
   Ctrl/Alt/Super combos like Ctrl+C) → the existing `KeyEvent` (scancode/keysym)
   path. These are *not* text and must stay as key events.

The mobile UI decides which channel: printable committed text → `TextInput`;
everything else → `KeyEvent`. Because desktop targets need modifiers that phones
lack, the UI adds an **on-screen modifier bar** (Ctrl / Alt / Super / Esc / Tab /
arrows / Fn) — standard in mobile terminal/remote apps.

### Client capture per platform

- **Android:** attach a hidden view with a custom `InputConnection`.
  `commitText()` → `TextInput`; `deleteSurroundingText`/`sendKeyEvent(KEYCODE_DEL)`
  → Backspace `KeyEvent`; hardware keys via `onKeyDown`. Composing text
  (`setComposingText`) shown locally; sent on commit.
- **iOS:** implement `UIKeyInput` (MVP) then `UITextInput` (full IME) on the
  render view. `insertText:` → `TextInput`; `deleteBackward` → Backspace;
  hardware keyboards via `pressesBegan`/`UIPress`; marked (composing) text via
  `UITextInput`.
- **MVP simplification:** send only *committed* text (ignore intermediate
  composition). Works for Latin + autocorrect-on-commit; live CJK/marked-text
  composition is a later refinement (K3).

### Server-side: injecting Unicode into Wayland

This is the interesting part. Options, best first:

- **Primary — Wayland input-method-v2.** Run a `zwp_input_method_v2` client in
  the server that receives `TextInput` and **commits the UTF-8 string directly to
  the focused surface** — literally "an IME committing text to the focused app".
  This is the *correct* Wayland abstraction: full Unicode, no keymap hacking,
  and the app's own text field handles it. wlroots-based labwc/cage/sway support
  input-method-v2, so this works in our compositors.
- **Fallback — dynamic xkb keymap (wtype-style).** For apps/surfaces that don't
  participate in text-input (some terminals, games), synthesize a temporary xkb
  keymap that maps spare keycodes to the exact keysyms/codepoints in the string,
  upload it via the virtual keyboard, "press" them in order, then restore. Robust
  but fiddly; used only when input-method-v2 isn't accepted.
- **Editing/shortcut keys** always use the existing virtual-keyboard scancode
  path regardless — they're real keys, not text.

So the server grows a small "text injector" with two backends
(input-method-v2 → dynamic-keymap fallback), selected per focused surface.

### Keyboard phasing

- **K1** — on-screen modifier bar + `KeyEvent` (works today for hardware
  keyboards and editing keys).
- **K2** — `TextInput` message + server input-method-v2 committer → soft-keyboard
  Unicode text, emoji, autocorrect.
- **K3** — live composing/marked text (CJK IME) + dynamic-keymap fallback for
  non-text-input apps.

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
