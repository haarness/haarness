#!/bin/sh
set -eu
cd "$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)"
if [ -z "${HAARNESS_BIN:-}" ]; then
  cargo build -p haarness-cli
  export HAARNESS_BIN="$PWD/target/debug/haarness"
fi
npm --prefix sdks/typescript ci --ignore-scripts
npm --prefix sdks/typescript run build
npm --prefix packages/browser-driver ci
npm run driver:test
npm --prefix apps/desktop ci
npm --prefix apps/desktop run build
npm --prefix apps/desktop test
if [ "$(uname -s)" = Linux ]; then
  scripts/linux-desktop-session.sh npm --prefix apps/desktop run test:e2e
else
  npm --prefix apps/desktop run test:e2e
fi
