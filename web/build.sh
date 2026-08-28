#!/usr/bin/env bash
#
# Build the embeddable TypeScript client into web/dist/.
# The sample page (index.html + app.js) is plain JS and is not compiled.
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

echo "Built web/dist/ — library entry is dist/index.js; sample page is index.html + app.js"
echo "Serve this directory over HTTP. The page's origin must appear in --webtransport-origin."
