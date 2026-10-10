#!/bin/sh
set -eu

session="haarness-pty-e2e-$$"
test_root="$(mktemp -d)"
token="haarness-pty-e2e-token"
cleanup() {
  tmux kill-session -t "$session" 2>/dev/null || true
  rm -rf "$test_root"
}
trap cleanup EXIT INT TERM

if [ -n "${HAARNESS_BIN:-}" ]; then
  binary="$HAARNESS_BIN"
else
  cargo build -q -p haarness-cli
  binary="$PWD/target/debug/haarness"
fi
case "$binary" in
  /*) ;;
  *) binary="$PWD/${binary#./}" ;;
esac
[ -x "$binary" ] || { echo "Haarness binary is not executable: $binary" >&2; exit 1; }
mkdir -p "$test_root/workspace"
port="$(python3 -c 'import socket; s=socket.socket(); s.bind(("127.0.0.1", 0)); print(s.getsockname()[1]); s.close()')"
base="http://127.0.0.1:$port"

tmux new-session -d -s "$session" -x 100 -y 30 \
  "'$binary' --data-dir '$test_root/data' --workspace '$test_root/workspace' --trust-project serve --host 127.0.0.1 --port '$port' --token '$token'"

ready=0
for _ in $(seq 1 100); do
  if curl -fsS -H "Authorization: Bearer $token" "$base/v1/info" >/dev/null 2>&1; then
    ready=1
    break
  fi
  sleep 0.05
done
if [ "$ready" -ne 1 ]; then
  echo "PTY test server did not become ready" >&2
  exit 1
fi

tmux new-window -t "$session" -n tui \
  "'$binary' --data-dir '$test_root/data' --workspace '$test_root/workspace' --trust-project attach '$base' --token '$token'"
for _ in $(seq 1 100); do
  pane="$(tmux capture-pane -p -t "$session":tui.0)"
  echo "$pane" | grep -q 'Ready\|Connected' && break
  sleep 0.05
done
tmux send-keys -l -t "$session":tui.0 '/terminal'
tmux send-keys -t "$session":tui.0 Enter
sleep 0.5
tmux send-keys -l -t "$session":tui.0 "printf 'TMUX_TUI_PTY\\n'"
tmux send-keys -t "$session":tui.0 Enter
found=0
for _ in $(seq 1 100); do
  pane="$(tmux capture-pane -p -t "$session":tui.0)"
  if echo "$pane" | grep -q 'TMUX_TUI_PTY'; then
    found=1
    break
  fi
  sleep 0.05
done
if [ "$found" -ne 1 ]; then
  echo "The full-screen TUI terminal did not render child PTY output" >&2
  exit 1
fi

# Detach to chat, re-open, and require the server replay to reconstruct the
# child terminal. Killing the client pane afterwards must still leave the PTY.
# tmux sends Ctrl+] as the legacy GS byte, which must still lead back.
tmux send-keys -t "$session":tui.0 'C-]'
detached=0
for _ in $(seq 1 200); do
  pane="$(tmux capture-pane -p -t "$session":tui.0)"
  if echo "$pane" | grep -q 'Terminal detached'; then
    detached=1
    break
  fi
  sleep 0.05
done
if [ "$detached" -ne 1 ]; then
  echo "$pane" >&2
  echo "Ctrl+] did not return from the terminal to the chat" >&2
  exit 1
fi
tmux send-keys -l -t "$session":tui.0 '/terminal'
tmux send-keys -t "$session":tui.0 Enter
sleep 0.5
pane="$(tmux capture-pane -p -t "$session":tui.0)"
if ! echo "$pane" | grep -q 'TMUX_TUI_PTY'; then
  echo "The TUI did not reconstruct PTY output after detach/reconnect" >&2
  exit 1
fi
tmux kill-pane -t "$session":tui.0

python3 "$PWD/scripts/test_pty_ws.py" "$base" "$token"

for _ in $(seq 1 100); do
  if ! curl -fsS "$base/healthz" >/dev/null 2>&1; then
    echo "PTY tmux end-to-end test passed"
    exit 0
  fi
  sleep 0.05
done
echo "PTY server did not stop after cleanup test" >&2
exit 1
