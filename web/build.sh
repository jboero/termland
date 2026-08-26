#!/usr/bin/env bash
#
# Build the browser client: compile termland-web to wasm and generate the JS
# bindings next to index.html.
#
# Requires:
#   rustup target add wasm32-unknown-unknown   (or the distro's
#                                               rust-std-static-wasm32-unknown-unknown)
#   cargo install wasm-bindgen-cli --version <matching the crate's wasm-bindgen>
#
set -euo pipefail
cd "$(dirname "$0")"
CRATE=../crates/termland-web
PROFILE=${1:-release}

# WebTransport and WebCodecs bindings are still gated in web-sys. The crate's
# own .cargo/config.toml sets this too; it is repeated here because a build run
# from another directory does not pick that up.
export RUSTFLAGS="--cfg=web_sys_unstable_apis"

if [ "$PROFILE" = "release" ]; then
  cargo build --manifest-path "$CRATE/Cargo.toml" --target wasm32-unknown-unknown --release
  WASM="$CRATE/target/wasm32-unknown-unknown/release/termland_web.wasm"
else
  cargo build --manifest-path "$CRATE/Cargo.toml" --target wasm32-unknown-unknown
  WASM="$CRATE/target/wasm32-unknown-unknown/debug/termland_web.wasm"
fi

wasm-bindgen --target web --out-dir ./pkg --no-typescript "$WASM"
echo "Built web/pkg/ — serve this directory over HTTPS and open index.html"
