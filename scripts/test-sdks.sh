#!/bin/sh
# Full SDK gate; no publication, no user credentials, private server fixtures.
set -eu
cd "$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)"
if [ -z "${HAARNESS_BIN:-}" ]; then
  cargo build -p haarness-cli
  export HAARNESS_BIN="$PWD/target/debug/haarness"
fi
npm --prefix sdks/typescript ci --ignore-scripts
uv sync --project sdks/python --frozen
python3 sdks/tests/server.py --check
python3 sdks/tests/files_binary.py
python3 sdks/generate.py --check
npm --prefix sdks/typescript run build
npm --prefix sdks/typescript test
uv run --project sdks/python --frozen mypy sdks/python/src/haarness_sdk
uv run --project sdks/python --frozen pytest -q sdks/python/tests
uv run --project sdks/python --frozen python sdks/tests/e2e.py
uv run --project sdks/python --frozen python sdks/tests/lifecycle.py
uv build --project sdks/python
mkdir -p sdks/artifacts
npm pack ./sdks/typescript --pack-destination sdks/artifacts
uv run --project sdks/python --frozen python sdks/tests/artifacts.py
