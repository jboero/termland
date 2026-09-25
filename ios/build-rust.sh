#!/usr/bin/env bash
# Build Termland's UniFFI core for an iPhone/iPad, an Apple-Silicon simulator,
# and Apple-Silicon macOS, then make the dynamic XCFramework consumed by
# Termland.xcodeproj.
#
# All output stays below ios/build/.  In particular this does not share the
# workspace target/ directory: an iOS build must never invalidate a desktop
# Rust build (or vice versa).
set -euo pipefail

# Xcode run-script phases do not source the user's shell profile, so a
# rustup-managed toolchain in ~/.cargo/bin is not on PATH when building from
# the Xcode GUI (it is from a terminal, which hides the problem).
export PATH="${CARGO_HOME:-${HOME}/.cargo}/bin:${PATH}"
for tool in rustup cargo; do
  command -v "${tool}" >/dev/null || {
    echo "error: ${tool} not found on PATH (${PATH}). Install Rust via rustup or set CARGO_HOME." >&2
    exit 1
  }
done

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

for target in aarch64-apple-ios aarch64-apple-ios-sim aarch64-apple-darwin; do
  if ! rustup target list --installed | grep -qx "${target}"; then
    echo "Missing Rust target ${target}. Install it with:" >&2
    echo "  rustup target add aarch64-apple-ios aarch64-apple-ios-sim aarch64-apple-darwin" >&2
    exit 1
  fi
done

export CARGO_TARGET_DIR="${TARGET_DIR}"
mkdir -p "${OUTPUT_DIR}" "${SWIFT_DIR}"

for target in aarch64-apple-ios aarch64-apple-ios-sim aarch64-apple-darwin; do
  if [[ "${CONFIGURATION}" == "Release" ]]; then
    cargo build --locked -p "${CORE_PACKAGE}" --target "${target}" --release
  else
    cargo build --locked -p "${CORE_PACKAGE}" --target "${target}"
  fi
done

DEVICE_LIB="${TARGET_DIR}/aarch64-apple-ios/${PROFILE_DIR}/${LIB_NAME}"
SIMULATOR_LIB="${TARGET_DIR}/aarch64-apple-ios-sim/${PROFILE_DIR}/${LIB_NAME}"
MACOS_LIB="${TARGET_DIR}/aarch64-apple-darwin/${PROFILE_DIR}/${LIB_NAME}"
for library in "${DEVICE_LIB}" "${SIMULATOR_LIB}" "${MACOS_LIB}"; do
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
  local layout="${4:-shallow}"
  local framework="${parent}/${name}.framework"
  # iOS frameworks are shallow (everything at the top level); macOS frameworks
  # must be versioned, with top-level symlinks into Versions/Current, or Xcode
  # rejects the embedded bundle.
  local root="${framework}" resources="${framework}" install_name="@rpath/${name}.framework/${name}"
  if [[ "${layout}" == "deep" ]]; then
    root="${framework}/Versions/A"
    resources="${root}/Resources"
    install_name="@rpath/${name}.framework/Versions/A/${name}"
  fi
  rm -rf "${framework}"
  mkdir -p "${root}/Headers" "${root}/Modules" "${resources}"
  cp "${library}" "${root}/${name}"
  # cargo records the absolute target/deps path as the install name; the app
  # would then try to load the dylib from this build machine at launch
  # instead of from the embedded framework.
  install_name_tool -id "${install_name}" "${root}/${name}"
  cp "${SWIFT_DIR}/TermlandCoreFFI.h" "${root}/Headers/TermlandCoreFFI.h"
  cat > "${root}/Modules/module.modulemap" <<EOF
// UniFFI's generated Swift source imports this module name. The binary stays
// TermlandCore so the dynamically embedded framework has a stable product
// name, while this module exposes its C ABI to the generated source.
framework module TermlandCoreFFI {
  header "../Headers/TermlandCoreFFI.h"
  export *
  link "${name}"
}
EOF
  # installd rejects an embedded iOS framework without MinimumOSVersion; it
  # must match the app's IPHONEOS_DEPLOYMENT_TARGET.
  local min_os=""
  if [[ "${layout}" == "shallow" ]]; then
    min_os="<key>MinimumOSVersion</key><string>17.0</string>"
  fi
  cat > "${resources}/Info.plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>CFBundleDevelopmentRegion</key><string>en</string>
  <key>CFBundleExecutable</key><string>${name}</string>
  <key>CFBundleIdentifier</key><string>dev.termland.${name}</string>
  <key>CFBundleInfoDictionaryVersion</key><string>6.0</string>
  <key>CFBundleName</key><string>${name}</string>
  <key>CFBundlePackageType</key><string>FMWK</string>
  <key>CFBundleShortVersionString</key><string>0.8.0</string>
  <key>CFBundleVersion</key><string>1</string>
  ${min_os}
</dict></plist>
EOF
  if [[ "${layout}" == "deep" ]]; then
    ln -s A "${framework}/Versions/Current"
    for entry in "${name}" Headers Modules Resources; do
      ln -s "Versions/Current/${entry}" "${framework}/${entry}"
    done
  fi
}

INPUT_DIR="${OUTPUT_DIR}/xcframework-input"
rm -rf "${INPUT_DIR}"
make_framework TermlandCore "${DEVICE_LIB}" "${INPUT_DIR}/ios"
make_framework TermlandCore "${SIMULATOR_LIB}" "${INPUT_DIR}/simulator"
make_framework TermlandCore "${MACOS_LIB}" "${INPUT_DIR}/macos" deep
rm -rf "${OUTPUT_DIR}/TermlandCore.xcframework"
xcodebuild -create-xcframework \
  -framework "${INPUT_DIR}/ios/TermlandCore.framework" \
  -framework "${INPUT_DIR}/simulator/TermlandCore.framework" \
  -framework "${INPUT_DIR}/macos/TermlandCore.framework" \
  -output "${OUTPUT_DIR}/TermlandCore.xcframework"
rm -rf "${INPUT_DIR}"

echo "Built ${OUTPUT_DIR}/TermlandCore.xcframework and generated Swift bindings."
