# Termland iOS / macOS client (M3a–M3c)

Native SwiftUI client for iPhone, iPad, and Apple-Silicon macOS, on the shared
Rust core (`termland-mobile-core`, via UniFFI):

- **Control plane (M3a/M3b):** saved direct TCP/TLS profiles, Keychain
  passwords, the resumable-session list, closing sessions.
- **Streaming (M3c):** **New Session** creates a desktop session; tapping a
  listed session resumes it. Video is H.264/HEVC decoded by VideoToolbox and
  shown on an `AVSampleBufferDisplayLayer`. Disconnecting *detaches*: the
  session keeps running on the server and can be resumed.

Not yet: audio and clipboard (M3e), AV1, QUIC/SSH transports in the UI, iPad
pointer lock, a trackpad-mode toggle.

## Build

On an Apple-Silicon Mac with Xcode and Rust:

```sh
rustup target add aarch64-apple-ios aarch64-apple-ios-sim aarch64-apple-darwin
./ios/build-rust.sh
xcodebuild -project ios/Termland.xcodeproj -scheme Termland \
  -sdk iphonesimulator -destination 'generic/platform=iOS Simulator' \
  CODE_SIGNING_ALLOWED=NO build
```

`build-rust.sh` keeps the Rust target directory, generated UniFFI Swift source,
and the dynamic `TermlandCore.xcframework` in `ios/build/` or
`ios/Termland/Generated/`; none are committed. Open `Termland.xcodeproj` in
Xcode and select either the `Termland` scheme for an iPhone/iPad destination,
or the `Termland macOS` scheme and `My Mac` to run the native desktop app. On
macOS each session opens in its own window.

## How streaming works

- **Codec negotiation** (`CodecSupport.swift`): HEVC is advertised only when
  `VTIsHardwareDecodeSupported` says so; H.264 always. AV1 is not advertised
  (hardware only on A17 Pro / M3+, and VideoToolbox has no software fallback).
- **Bitstream** (`AnnexB.swift`): the server sends Annex B with in-band
  parameter sets. They are moved into a `CMVideoFormatDescription`, and the
  remaining NAL units are length-prefixed (AVCC/HVCC). AUDs are dropped.
- **Decode** (`VideoDecoder.swift`): `VTDecompressionSession` with temporal
  processing, because libx264/libx265 at their default presets emit B-frames.
  Frames come out in presentation order and are shown immediately. Everything
  is dropped until a keyframe, after a decode error, and when more than 8
  packets back up; the protocol has no keyframe request, so recovery waits for
  the encoder's next GOP. After resyncing at an HEVC CRA (libx265's default
  open-GOP keyframe), its RASL pictures are skipped.
- **Remote size** is the view size in *points*, rounded to even numbers: the
  remote compositor has no HiDPI scaling. Window or rotation changes send a
  debounced resize.
- **Cursor:** the client asks the server to draw the cursor into the video
  (the core drops client-side cursor updates) and hides the local pointer.

## Input

| | macOS | iPhone / iPad |
|---|---|---|
| Keyboard | Scancodes from `NSEvent.keyCode`; Ctrl→Ctrl, Option→Alt, Command→Super. Command-Q/W still act locally. | Hardware keys by HID usage (`UIKey.keyCode`); soft keyboard sends committed text (`TextInput`), Backspace as a key |
| Pointer | Mouse moves/buttons (incl. middle/back/forward), precise scrolling | Tap = click, long-press = right-click, drag = left-drag, two-finger drag = scroll; iPad mouse/trackpad hover, secondary click and scroll |
| Extras | — | Collapsible bar: Esc, Tab, sticky Ctrl/Alt/Super, arrows, keyboard toggle |

Keys still held when focus is lost or the session detaches are released, so
modifiers do not stay stuck on the remote side. iOS detaches when the app goes
to the background; **Resume** reattaches.

## Tests

`TermlandTests` (run on an iOS simulator) covers:

- the Annex B converter,
- both key tables,
- letterboxed pointer mapping,
- VideoToolbox decode, end to end, of real libx264/libx265 output with
  B-frames (`TermlandTests/Fixtures`, recipe in the test file). It checks that
  every frame decodes in presentation order, that frames are dropped until a
  keyframe, and that RASL pictures are skipped when joining at a CRA.

```sh
xcodebuild -project ios/Termland.xcodeproj -scheme Termland \
  -destination 'platform=iOS Simulator,name=iPhone 17' test
```
