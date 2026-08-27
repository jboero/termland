#!/usr/bin/env bash
#
# Build the browser client: TypeScript into web/dist/.
#
# Requires Node.js 20+.
#
set -euo pipefail
cd "$(dirname "$0")"

if [ ! -d node_modules ]; then
  npm install
fi
npm test
npm run build

echo "Built web/dist/ — serve this directory over HTTP and open index.html"
echo "The page's origin must appear in --webtransport-origin."
