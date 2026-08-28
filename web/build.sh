#!/usr/bin/env bash
#
# Build the wasm protocol codec, run tests, then compile the TypeScript
# client into web/dist/. The sample page (index.html + app.js) is plain JS
# and is not compiled.
#
# Requires: rustc with wasm32-unknown-unknown, wasm-bindgen-cli 0.2.114,
# Node.js 20+.
#
set -euo pipefail
cd "$(dirname "$0")"

WASM_BINDGEN_VERSION=0.2.114
if ! command -v wasm-bindgen >/dev/null; then
  echo "wasm-bindgen not found. Install with:" >&2
  echo "  cargo install wasm-bindgen-cli --version ${WASM_BINDGEN_VERSION}" >&2
  exit 1
fi

echo "Building crates/termland-web for wasm32…"
cargo build --manifest-path ../crates/termland-web/Cargo.toml \
  --target wasm32-unknown-unknown --release
wasm-bindgen --target web --out-dir ./pkg \
  ../crates/termland-web/target/wasm32-unknown-unknown/release/termland_web.wasm

if [ ! -d node_modules ]; then
  if [ -f package-lock.json ]; then
    npm ci
  else
    npm install
  fi
fi
npm test
npm run build

echo "Built web/pkg/ (wasm protocol) and web/dist/ (TS client)."
echo "Sample page is index.html + app.js. Serve this directory over HTTP."
echo "The page's origin must appear in --webtransport-origin."
