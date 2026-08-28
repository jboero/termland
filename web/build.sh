#!/usr/bin/env bash
#
# Build the wasm protocol codec, run tests, then compile the TypeScript
# client into web/dist/. The sample page (index.html + app.js) is plain JS
# and is not compiled.
#
# Requires: rustc with wasm32-unknown-unknown, wasm-bindgen-cli, Node.js 20+.
#
# wasm-bindgen's generated glue and its CLI must be the exact same version --
# the bindgen schema is not stable across releases. The crate's Cargo.lock is
# the single source of truth for which one that is, so the required version is
# read from there rather than hardcoded here (a hardcoded pin goes stale and
# then demands a downgrade of a globally installed tool).
set -euo pipefail
cd "$(dirname "$0")"

# The wasm-bindgen version Cargo.lock resolves to: the line after `name =
# "wasm-bindgen"` in that package's stanza.
locked_wasm_bindgen() {
  awk '/^name = "wasm-bindgen"$/ { getline; gsub(/[",]/, "", $3); print $3; exit }' \
    ../crates/termland-web/Cargo.lock
}

WASM_BINDGEN_VERSION=$(locked_wasm_bindgen)
[ -n "$WASM_BINDGEN_VERSION" ] || {
  echo "could not read the wasm-bindgen version from crates/termland-web/Cargo.lock" >&2
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
  echo "  cargo update --manifest-path crates/termland-web/Cargo.toml -p wasm-bindgen --precise ${INSTALLED}" >&2
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
