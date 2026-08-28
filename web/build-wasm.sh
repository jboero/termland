#!/usr/bin/env bash
#
# Build the Rust browser client (crates/termland-web-client) into web/wasm/pkg/.
#
# Separate from build.sh, which builds the TypeScript client: the two are
# parallel implementations and either can be built without the other.
#
# Requires: rustc with wasm32-unknown-unknown, wasm-bindgen-cli.
#
# NOTE the `cd` into the crate. WebTransport and WebCodecs need
# `--cfg=web_sys_unstable_apis`, which the crate declares in its own
# .cargo/config.toml -- and cargo discovers that file relative to the working
# directory, not to --manifest-path. Building from anywhere else silently drops
# the flag and the web-sys bindings vanish.
set -euo pipefail
cd "$(dirname "$0")"
WEB_DIR=$PWD
CRATE_DIR=$WEB_DIR/../crates/termland-web-client

locked_wasm_bindgen() {
  awk '/^name = "wasm-bindgen"$/ { getline; gsub(/[",]/, "", $3); print $3; exit }' \
    "$CRATE_DIR/Cargo.lock"
}

WASM_BINDGEN_VERSION=$(locked_wasm_bindgen)
[ -n "$WASM_BINDGEN_VERSION" ] || {
  echo "could not read the wasm-bindgen version from termland-web-client's Cargo.lock" >&2
  exit 1
}

if ! command -v wasm-bindgen >/dev/null; then
  echo "wasm-bindgen not found. Install with:" >&2
  echo "  cargo install wasm-bindgen-cli --version ${WASM_BINDGEN_VERSION}" >&2
  exit 1
fi

INSTALLED=$(wasm-bindgen --version | awk '{print $2}')
if [ "$INSTALLED" != "$WASM_BINDGEN_VERSION" ]; then
  echo "wasm-bindgen-cli ${INSTALLED} is installed, but Cargo.lock pins ${WASM_BINDGEN_VERSION}." >&2
  echo "The bindgen schema must match exactly. Either:" >&2
  echo "  cargo install wasm-bindgen-cli --version ${WASM_BINDGEN_VERSION}" >&2
  echo "or move the lockfile to your CLI and commit the result:" >&2
  echo "  cargo update --manifest-path crates/termland-web-client/Cargo.toml -p wasm-bindgen --precise ${INSTALLED}" >&2
  exit 1
fi

echo "Building crates/termland-web-client for wasm32…"
( cd "$CRATE_DIR" && cargo build --target wasm32-unknown-unknown --release )
wasm-bindgen --target web --out-dir "$WEB_DIR/wasm/pkg" \
  "$CRATE_DIR/target/wasm32-unknown-unknown/release/termland_web_client.wasm"

echo "Built web/wasm/pkg/. Serve web/ over HTTP and open /wasm/."
echo "The page's origin must appear in --webtransport-origin."
