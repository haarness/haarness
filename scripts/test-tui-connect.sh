#!/bin/sh
# First-run onboarding in the terminal: with nothing connected the connect
# picker is the welcome screen; `/connect openai` signs in with a device code
# against a fixture issuer; `/connect anthropic` stores a pasted key; `@` lists
# agents next to files. Runs the real binary, daemon and TUI under tmux.
set -eu

binary=${HAARNESS_BIN:-"$PWD/target/debug/haarness"}
session="haarness-connect-$$"
test_root="$(mktemp -d)"
issuer_pid=''
cleanup() {
  tmux kill-session -t "$session" 2>/dev/null || true
  if [ -x "$binary" ] && [ -d "$test_root/ws" ]; then
    env HAARNESS_DATA_DIR="$test_root" \
      "$binary" --workspace "$test_root/ws" serve --stop >/dev/null 2>&1 || true
  fi
  if [ -n "$issuer_pid" ]; then
    kill "$issuer_pid" 2>/dev/null || true
  fi
  rm -rf "$test_root"
}
trap cleanup EXIT INT TERM

if [ -z "${HAARNESS_BIN:-}" ]; then
  cargo build -q -p haarness-cli
fi

wait_for() (
  label=$1
  shift
  attempt=0
  while [ "$attempt" -lt 150 ]; do
    pane="$(tmux capture-pane -p -t "$session":0.0 2>/dev/null || true)"
    ok=1
    for needle in "$@"; do
      echo "$pane" | grep -Fq -- "$needle" || ok=0
    done
    [ "$ok" -eq 1 ] && exit 0
    attempt=$((attempt + 1))
    sleep 0.2
  done
  echo "$pane" >&2
  echo "$label" >&2
  exit 1
)

# A stand-in for auth.openai.com: one device code, one refused poll, tokens.
cat > "$test_root/issuer.py" <<'PY'
import json, sys
from http.server import BaseHTTPRequestHandler, HTTPServer
polls = {"count": 0}
class Handler(BaseHTTPRequestHandler):
    def log_message(self, *_):
        pass
    def _json(self, status, value):
        body = json.dumps(value).encode()
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)
    def do_POST(self):
        length = int(self.headers.get("Content-Length") or 0)
        self.rfile.read(length)
        if self.path == "/api/accounts/deviceauth/usercode":
            return self._json(200, {"device_auth_id": "fixture", "user_code": "ABCD-EFGH", "interval": "1"})
        if self.path == "/api/accounts/deviceauth/token":
            polls["count"] += 1
            if polls["count"] == 1:
                return self._json(403, {"error": "authorization_pending"})
            return self._json(200, {"authorization_code": "code-1", "code_verifier": "verifier-1"})
        if self.path == "/oauth/token":
            return self._json(200, {"access_token": "fixture-access", "refresh_token": "fixture-refresh", "expires_in": 3600})
        self._json(404, {"error": "not found"})
    def do_GET(self):
        self._json(404, {"error": "not found"})
server = HTTPServer(("127.0.0.1", 0), Handler)
sys.stdout.write(str(server.server_address[1]) + "\n")
sys.stdout.flush()
server.serve_forever()
PY
python3 "$test_root/issuer.py" > "$test_root/issuer.port" &
issuer_pid=$!
attempt=0
while [ ! -s "$test_root/issuer.port" ] && [ "$attempt" -lt 50 ]; do
  attempt=$((attempt + 1))
  sleep 0.1
done
issuer="http://127.0.0.1:$(cat "$test_root/issuer.port")"

mkdir -p "$test_root/ws" "$test_root/config"
# A developer machine may run Ollama or LM Studio, which count as connected;
# the first-run story needs a genuinely empty catalog.
printf -- '[providers.ollama]\nenabled = false\n[providers.lmstudio]\nenabled = false\n' > "$test_root/config/config.toml"
# Nothing connected: no test provider, no keys, discovery pointed at the
# fixture so no test ever reaches a real provider. SSH_TTY keeps the TUI from
# opening a real browser for the sign-in link.
launch="env HAARNESS_DATA_DIR='$test_root' HAARNESS_OPENAI_OAUTH_ISSUER='$issuer' OPENAI_BASE_URL='$issuer/v1' ANTHROPIC_BASE_URL='$issuer/v1' SSH_TTY=fixture '$binary' --workspace '$test_root/ws'"
tmux new-session -d -x 110 -y 34 -s "$session" "$launch"

# 1. First run: the picker is the welcome screen and the home hint names it.
wait_for "The connect picker did not open on first run" "Connect a provider" "OpenAI" "Anthropic"
tmux send-keys -t "$session":0.0 Escape
wait_for "Home did not explain how to connect" "no provider connected" "/connect" "files · agents"

# 2. `/connect openai` offers the sign-in methods; the code flow completes
#    against the fixture without any typing beyond choosing it.
tmux send-keys -t "$session":0.0 "/connect openai" Enter
wait_for "The method picker did not open for OpenAI" "Connect OpenAI" "Sign in with ChatGPT" "Sign in with a code" "Use an API key"
pane="$(tmux capture-pane -p -t "$session":0.0)"
if ! echo "$pane" | grep -Fq "Details"; then :; else
  echo "$pane" >&2
  echo "A generic Details row leaked into the connect flow" >&2
  exit 1
fi
# Over SSH the device method is preselected; Enter chooses it.
tmux send-keys -t "$session":0.0 Enter
wait_for "The sign-in screen did not show the code" "Sign in to OpenAI" "ABCD-EFGH" "Waiting for you to finish"
wait_for "Signing in did not land in the model picker" "Select model" "openai/"
tmux send-keys -t "$session":0.0 Escape
# The session now runs on the provider that was just connected; the band
# says so even after later notices (a configuration reload) replace the
# status text.
wait_for "The session did not switch to the connected provider" "openai/" "Ask anything"

# 3. `/connect anthropic` is a single-method provider: straight to the masked
#    key prompt, with the console link as a hint, then connected.
tmux send-keys -t "$session":0.0 "/connect anthropic" Enter
wait_for "The key prompt did not open for Anthropic" "Connect Anthropic" "console.anthropic.com" "ANTHROPIC_API_KEY" "enter connect"
tmux send-keys -l -t "$session":0.0 "sk-ant-fixture-key"
sleep 0.3
pane="$(tmux capture-pane -p -t "$session":0.0)"
if echo "$pane" | grep -Fq "sk-ant-fixture"; then
  echo "$pane" >&2
  echo "The key prompt echoed the secret" >&2
  exit 1
fi
tmux send-keys -t "$session":0.0 Enter
wait_for "Saving the key did not land in the model picker" "Select model" "anthropic/"
tmux send-keys -t "$session":0.0 Escape
if ! grep -q '"anthropic"' "$test_root/auth.json" 2>/dev/null && ! find "$test_root" -name auth.json -exec grep -l '"anthropic"' {} + | grep -q .; then
  echo "The Anthropic key was not stored in auth.json" >&2
  exit 1
fi

# 4. `@` completes agents as well as files.
tmux send-keys -l -t "$session":0.0 "@ex"
wait_for "@ did not offer the explore agent" "@explore" "Delegate to agent"
tmux send-keys -t "$session":0.0 Escape
tmux send-keys -t "$session":0.0 C-c

# 5. Everything above went through the manager too: the provider list names
#    the connection state instead of a raw id.
tmux send-keys -t "$session":0.0 "/providers" Enter
wait_for "The provider manager did not open" "Providers" "Connect a provider" "Connected"
tmux send-keys -t "$session":0.0 Escape

echo "TUI connect flow: ok"
