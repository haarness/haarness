#!/bin/sh
set -eu

sh ./scripts/check-commercial-boundary.sh --self-test
sh ./scripts/check-commercial-boundary.sh
cargo fmt --all -- --check
cargo run -q -p xtask -- architecture
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo build --release -p haarness-cli
# The release servers the scripts below start never report usage statistics to Haarness Cloud.
export HAARNESS_TELEMETRY="${HAARNESS_TELEMETRY:-0}"
python3 vendor/ripgrep/verify.py
HAARNESS_BIN=./target/release/haarness python3 ./scripts/test-ripgrep.py
HAARNESS_BIN=./target/release/haarness python3 ./scripts/test-extensions.py
HAARNESS_BIN=./target/release/haarness ./scripts/test-ecosystem-smoke.sh
HAARNESS_BIN=./target/release/haarness ./scripts/test-attach.sh
HAARNESS_BIN=./target/release/haarness ./scripts/test-session-archive.sh
./target/release/haarness doctor
HAARNESS_BIN=./target/release/haarness ./scripts/test-pty-tmux.sh
./scripts/test-tmux.sh
HAARNESS_BIN=./target/release/haarness ./scripts/test-tui-connect.sh

HAARNESS_BIN=./target/release/haarness python3 ./scripts/test-tui-parity.py
HAARNESS_BIN=./target/release/haarness python3 ./scripts/test-tui-connections-security.py

HAARNESS_BIN=./target/release/haarness python3 ./scripts/test-input-queue-api.py
HAARNESS_BIN=./target/release/haarness python3 ./scripts/test-input-queue-tmux.py

HAARNESS_BIN=./target/release/haarness python3 ./scripts/test-tui-bugfixes.py
