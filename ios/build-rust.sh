#!/usr/bin/env bash
# Build Termland's UniFFI core for an iPhone/iPad and an Apple-Silicon
# simulator, then make the dynamic XCFramework consumed by Termland.xcodeproj.
#
# All output stays below ios/build/.  In particular this does not share the
# workspace target/ directory: an iOS build must never invalidate a desktop
# Rust build (or vice versa).
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CORE_PACKAGE="termland-mobile-core"
LIB_NAME="libtermland_mobile_core.dylib"
TARGET_DIR="${SCRIPT_DIR}/build/cargo-target"
OUTPUT_DIR="${SCRIPT_DIR}/build"
SWIFT_DIR="${SCRIPT_DIR}/Termland/Generated"
CONFIGURATION="${CONFIGURATION:-Debug}"

case "${CONFIGURATION}" in
  Release) PROFILE_DIR=release ;;
  *) PROFILE_DIR=debug ;;
esac

for target in aarch64-apple-ios aarch64-apple-ios-sim; do
  if ! rustup target list --installed | grep -qx "${target}"; then
    echo "Missing Rust target ${target}. Install it with:" >&2
    echo "  rustup target add aarch64-apple-ios aarch64-apple-ios-sim" >&2
    exit 1
  fi
done

export CARGO_TARGET_DIR="${TARGET_DIR}"
mkdir -p "${OUTPUT_DIR}" "${SWIFT_DIR}"

for target in aarch64-apple-ios aarch64-apple-ios-sim; do
  if [[ "${CONFIGURATION}" == "Release" ]]; then
    cargo build --locked -p "${CORE_PACKAGE}" --target "${target}" --release
  else
    cargo build --locked -p "${CORE_PACKAGE}" --target "${target}"
  fi
done

DEVICE_LIB="${TARGET_DIR}/aarch64-apple-ios/${PROFILE_DIR}/${LIB_NAME}"
SIMULATOR_LIB="${TARGET_DIR}/aarch64-apple-ios-sim/${PROFILE_DIR}/${LIB_NAME}"
for library in "${DEVICE_LIB}" "${SIMULATOR_LIB}"; do
  test -f "${library}" || { echo "Expected library was not built: ${library}" >&2; exit 1; }
done

# Bindgen reads UniFFI metadata from the Mach-O image; it does not load it, so
# the device slice works on an Apple-Silicon development Mac.
cargo run --locked -p "${CORE_PACKAGE}" --bin uniffi-bindgen -- \
  generate --library "${DEVICE_LIB}" --language swift --out-dir "${SWIFT_DIR}"
test -f "${SWIFT_DIR}/TermlandCore.swift"
test -f "${SWIFT_DIR}/TermlandCoreFFI.h"
test -f "${SWIFT_DIR}/TermlandCoreFFI.modulemap"

make_framework() {
  local name="$1"
  local library="$2"
  local parent="$3"
  local framework="${parent}/${name}.framework"
  rm -rf "${framework}"
  mkdir -p "${framework}/Headers" "${framework}/Modules"
  cp "${library}" "${framework}/${name}"
  cp "${SWIFT_DIR}/TermlandCoreFFI.h" "${framework}/Headers/TermlandCoreFFI.h"
  cat > "${framework}/Modules/module.modulemap" <<EOF
// UniFFI's generated Swift source imports this module name. The binary stays
// TermlandCore so the dynamically embedded framework has a stable product
// name, while this module exposes its C ABI to the generated source.
framework module TermlandCoreFFI {
  header "../Headers/TermlandCoreFFI.h"
  export *
  link "${name}"
}
EOF
  cat > "${framework}/Info.plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>CFBundleDevelopmentRegion</key><string>en</string>
  <key>CFBundleExecutable</key><string>${name}</string>
  <key>CFBundleIdentifier</key><string>dev.termland.${name}</string>
  <key>CFBundleInfoDictionaryVersion</key><string>6.0</string>
  <key>CFBundleName</key><string>${name}</string>
  <key>CFBundlePackageType</key><string>FMWK</string>
  <key>CFBundleShortVersionString</key><string>0.7.0</string>
  <key>CFBundleVersion</key><string>1</string>
</dict></plist>
EOF
}

INPUT_DIR="${OUTPUT_DIR}/xcframework-input"
rm -rf "${INPUT_DIR}"
make_framework TermlandCore "${DEVICE_LIB}" "${INPUT_DIR}/ios"
make_framework TermlandCore "${SIMULATOR_LIB}" "${INPUT_DIR}/simulator"
rm -rf "${OUTPUT_DIR}/TermlandCore.xcframework"
xcodebuild -create-xcframework \
  -framework "${INPUT_DIR}/ios/TermlandCore.framework" \
  -framework "${INPUT_DIR}/simulator/TermlandCore.framework" \
  -output "${OUTPUT_DIR}/TermlandCore.xcframework"
rm -rf "${INPUT_DIR}"

echo "Built ${OUTPUT_DIR}/TermlandCore.xcframework and generated Swift bindings."
