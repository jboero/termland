# Termland iOS POC (M3a/M3b)

This is the native SwiftUI control-plane client for iPhone, iPad, and
Apple-Silicon macOS: saved direct TCP/TLS profiles, Keychain passwords, and a
live resumable-session list. It deliberately does not stream or render
sessions yet; VideoToolbox, input, and audio are M3c+.

## Build

On an Apple-Silicon Mac with Xcode and Rust:

```sh
rustup target add aarch64-apple-ios aarch64-apple-ios-sim aarch64-apple-darwin
./ios/build-rust.sh
xcodebuild -project ios/Termland.xcodeproj -scheme Termland \
  -sdk iphonesimulator -destination 'generic/platform=iOS Simulator' \
  CODE_SIGNING_ALLOWED=NO ARCHS=arm64 ONLY_ACTIVE_ARCH=YES build
```

`build-rust.sh` keeps the Rust target directory, generated UniFFI Swift source,
and the dynamic `TermlandCore.xcframework` in `ios/build/` or
`ios/Termland/Generated/`; none are committed. Open `Termland.xcodeproj` in
Xcode and select either the `Termland` scheme for an iPhone/iPad destination,
or the `Termland macOS` scheme and `My Mac` to run the native desktop app.

The XCTest target's smoke test constructs `TermlandClient` and checks that it
starts disconnected. It can run with Xcode once a simulator runtime is
available.
