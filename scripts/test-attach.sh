#!/bin/sh
set -eu

repo=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
binary=${HAARNESS_BIN:-"$repo/target/debug/haarness"}
temporary=$(mktemp -d "${TMPDIR:-/tmp}/haarness-attach-e2e.XXXXXX")
data="$temporary/data"
server_workspace="$temporary/server-workspace"
client_workspace="$temporary/client-workspace"
session="haarness-attach-$$"
token="attach-e2e-$$-token"
pid=""
evidence=${HAARNESS_EVIDENCE_DIR:-""}
[ -z "$evidence" ] || mkdir -p "$evidence"

workspace_runtime_path() (
    runtime_data=$1
    runtime_workspace=$(CDPATH= cd -- "$2" && pwd -P)
    runtime_id=$(jq -er --arg path "$runtime_workspace" \
        'first(.workspaces[] | select(.path == $path) | .id)' \
        "$runtime_data/data/workspaces.json")
    printf '%s/cache/workspaces/%s/runtime.json\n' "$runtime_data" "$runtime_id"
)

cleanup() {
    tmux kill-session -t "$session" >/dev/null 2>&1 || true
    if [ -x "$binary" ] && [ -d "$server_workspace" ]; then
        env HAARNESS_DATA_DIR="$data" HAARNESS_SERVER_TOKEN="$token" \
            "$binary" --workspace "$server_workspace" serve --stop >/dev/null 2>&1 || true
    fi
    if [ -n "$pid" ] && kill -0 "$pid" >/dev/null 2>&1; then
        kill "$pid" >/dev/null 2>&1 || true
    fi
    rm -rf -- "$temporary"
}
trap cleanup EXIT HUP INT TERM

mkdir -p "$data" "$server_workspace" "$client_workspace"
server_workspace=$(CDPATH= cd -- "$server_workspace" && pwd -P)
client_workspace=$(CDPATH= cd -- "$client_workspace" && pwd -P)

if [ ! -x "$binary" ]; then
    cargo build -q -p haarness-cli
fi
command -v curl >/dev/null
command -v jq >/dev/null
command -v tmux >/dev/null

env HAARNESS_DATA_DIR="$data" HAARNESS_SERVER_TOKEN="$token" HAARNESS_TEST_PROVIDER=1 \
    "$binary" --workspace "$server_workspace" --yolo serve --daemon \
    --host 0.0.0.0 --port 0 >/dev/null

runtime=$(workspace_runtime_path "$data" "$server_workspace")
attempt=0
while [ "$attempt" -lt 80 ]; do
    [ -s "$runtime" ] && break
    attempt=$((attempt + 1))
    sleep 0.05
done
test -s "$runtime"
pid=$(jq -r '.pid' "$runtime")
advertised=$(jq -r '.url' "$runtime")
url=$(printf '%s\n' "$advertised" | sed 's#://0\.0\.0\.0:#://127.0.0.1:#')
kill -0 "$pid"

status=$(curl -sS -o "$temporary/no-auth" -w '%{http_code}' "$url/v1/info")
test "$status" = 401
status=$(curl -sS -o "$temporary/wrong-auth" -w '%{http_code}' \
    -H 'Authorization: Bearer wrong-token' "$url/v1/info")
test "$status" = 401
status=$(curl -sS -o "$temporary/cross-origin" -w '%{http_code}' \
    -H "Authorization: Bearer $token" -H 'Origin: https://unrelated.example' "$url/v1/info")
test "$status" = 403
status=$(curl -sS -o "$temporary/info" -w '%{http_code}' \
    -H "Authorization: Bearer $token" "$url/v1/info")
test "$status" = 200
jq -e '.name == "haarness"' "$temporary/info" >/dev/null
jq -e '.workspace == $workspace' --arg workspace "$server_workspace" "$temporary/info" >/dev/null
jq -e '.capabilities | index("network.attach") != null' "$temporary/info" >/dev/null
jq -e '.capabilities | index("attachments.inline-upload") != null' "$temporary/info" >/dev/null

run() {
    env HAARNESS_DATA_DIR="$data" HAARNESS_SERVER_TOKEN="$token" HAARNESS_TEST_PROVIDER=1 \
        "$binary" --workspace "$client_workspace" "$@"
}
api() {
    curl -sS -H "Authorization: Bearer $token" "$url$1"
}

output=$(run run --attach "$url" --provider echo --model echo-1 \
    --title 'Attach source' 'first attached message')
printf '%s\n' "$output" | grep -F 'Echo: first attached message' >/dev/null
source_id=$(api /v1/threads | jq -r '.[] | select(.title == "Attach source") | .id' | head -1)
test -n "$source_id"

output=$(run --continue run --attach "$url" --provider echo --model echo-1 \
    'continued attached message')
printf '%s\n' "$output" | grep -F 'Echo: continued attached message' >/dev/null
api "/v1/threads/$source_id/items" > "$temporary/source-items"
grep -F 'first attached message' "$temporary/source-items" >/dev/null
grep -F 'continued attached message' "$temporary/source-items" >/dev/null

before=$(api /v1/threads | jq 'length')
output=$(run --session "$source_id" run --attach "$url" --fork \
    --provider echo --model echo-1 'fork-only attached message')
printf '%s\n' "$output" | grep -F 'Echo: fork-only attached message' >/dev/null
after=$(api /v1/threads | jq 'length')
test "$after" -eq $((before + 1))
api "/v1/threads/$source_id/items" > "$temporary/source-after-fork"
if grep -F 'fork-only attached message' "$temporary/source-after-fork" >/dev/null; then
    echo 'Attach fork mutated the source session' >&2
    exit 1
fi
fork_id=$(api /v1/threads | jq -r --arg source "$source_id" \
    '.[] | select(.id != $source) | select(.title | startswith("Fork of Attach source")) | .id' | head -1)
test -n "$fork_id"
api "/v1/threads/$fork_id/items" | grep -F 'fork-only attached message' >/dev/null

output=$(run --yolo run --attach "$url" --provider echo --model echo-1 \
    --title 'Server workspace proof' \
    '!tool write {"path":"server-owned.txt","content":"server side"}')
printf '%s\n' "$output" | grep -F 'Tool write =>' >/dev/null
test -f "$server_workspace/server-owned.txt"
test ! -e "$client_workspace/server-owned.txt"

# A real 1x1 PNG: attachments are checked to be images Haarness can read.
printf '%s' 'iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==' |
    base64 -d > "$client_workspace/client image.png"
output=$(run run --attach "$url" --provider echo --model echo-1 \
    --title 'Attached media proof' --file "$client_workspace/client image.png" 'media probe')
printf '%s\n' "$output" | grep -F '[attachments: 1]' >/dev/null
media_id=$(api /v1/threads | jq -r '.[] | select(.title == "Attached media proof") | .id' | head -1)
test -n "$media_id"
api "/v1/threads/$media_id/items" > "$temporary/media-items"
grep -F 'data:image/png;name=client%20image.png;base64,' "$temporary/media-items" >/dev/null

set +e
wrong=$(env HAARNESS_DATA_DIR="$data" HAARNESS_SERVER_TOKEN='wrong-token' HAARNESS_TEST_PROVIDER=1 \
    "$binary" --workspace "$client_workspace" run --attach "$url" \
    --provider echo --model echo-1 'must not run' 2>&1)
wrong_status=$?
set -e
test "$wrong_status" -ne 0
printf '%s\n' "$wrong" | grep -F 'HTTP 401 Unauthorized' >/dev/null
if printf '%s\n' "$wrong" | grep -F 'wrong-token' >/dev/null; then
    echo 'Attach authentication error leaked the provided token' >&2
    exit 1
fi

launch="env HAARNESS_DATA_DIR='$data' HAARNESS_SERVER_TOKEN='$token' HAARNESS_TEST_PROVIDER=1 HAARNESS_DEFAULT_PROVIDER=echo HAARNESS_DEFAULT_MODEL=echo-1 '$binary' --workspace '$client_workspace' --session '$source_id' attach '$url'"
tmux new-session -d -x 100 -y 30 -s "$session" "$launch"
attempt=0
while [ "$attempt" -lt 100 ]; do
    pane=$(tmux capture-pane -p -t "$session":0.0 2>/dev/null || true)
    printf '%s\n' "$pane" | grep -q 'h a a r n e s s' && break
    attempt=$((attempt + 1))
    sleep 0.05
done
printf '%s\n' "$pane" | grep -q 'h a a r n e s s'
tmux send-keys -l -t "$session":0.0 '::mdslow'
tmux send-keys -t "$session":0.0 Enter
attempt=0
while [ "$attempt" -lt 100 ]; do
    pane=$(tmux capture-pane -p -t "$session":0.0 2>/dev/null || true)
    printf '%s\n' "$pane" | grep -q 'Live markdown' && break
    attempt=$((attempt + 1))
    sleep 0.05
done
printf '%s\n' "$pane" | grep -q 'Live markdown'
[ -z "$evidence" ] || tmux capture-pane -p -t "$session":0.0 > "$evidence/attach-streaming.txt"
tmux kill-session -t "$session"
kill -0 "$pid"

attempt=0
while [ "$attempt" -lt 80 ]; do
    api "/v1/threads/$source_id/items" > "$temporary/detached-items"
    grep -F 'streams while working' "$temporary/detached-items" >/dev/null && break
    attempt=$((attempt + 1))
    sleep 0.05
done
grep -F 'streams while working' "$temporary/detached-items" >/dev/null

launch="env HAARNESS_DATA_DIR='$data' HAARNESS_SERVER_TOKEN='$token' HAARNESS_TEST_PROVIDER=1 HAARNESS_DEFAULT_PROVIDER=echo HAARNESS_DEFAULT_MODEL=echo-1 '$binary' --workspace '$client_workspace' --session '$source_id' tui --attach '$url'"
tmux new-session -d -x 100 -y 30 -s "$session" "$launch"
attempt=0
while [ "$attempt" -lt 100 ]; do
    pane=$(tmux capture-pane -p -t "$session":0.0 2>/dev/null || true)
    printf '%s\n' "$pane" | grep -q 'streams while working' && \
        printf '%s\n' "$pane" | grep -q 'Connected' && break
    attempt=$((attempt + 1))
    sleep 0.05
done
printf '%s\n' "$pane" | grep -q 'streams while working'
printf '%s\n' "$pane" | grep -q 'Connected'
[ -z "$evidence" ] || tmux capture-pane -p -t "$session":0.0 > "$evidence/attach-restored.txt"
tmux send-keys -l -t "$session":0.0 '/exit'
tmux send-keys -t "$session":0.0 Enter
sleep 0.2
tmux kill-session -t "$session" >/dev/null 2>&1 || true

before=$(api /v1/threads | jq 'length')
fork_launch="env HAARNESS_DATA_DIR='$data' HAARNESS_SERVER_TOKEN='$token' HAARNESS_TEST_PROVIDER=1 HAARNESS_DEFAULT_PROVIDER=echo HAARNESS_DEFAULT_MODEL=echo-1 '$binary' --workspace '$client_workspace' --session '$source_id' attach '$url' --fork"
tmux new-session -d -x 100 -y 30 -s "$session" "$fork_launch"
attempt=0
while [ "$attempt" -lt 100 ]; do
    after=$(api /v1/threads | jq 'length')
    [ "$after" -eq $((before + 1)) ] && break
    attempt=$((attempt + 1))
    sleep 0.05
done
test "$after" -eq $((before + 1))
tmux kill-session -t "$session"
kill -0 "$pid"

env HAARNESS_DATA_DIR="$data" HAARNESS_SERVER_TOKEN="$token" \
    "$binary" --workspace "$server_workspace" serve --stop >/dev/null
attempt=0
while [ "$attempt" -lt 80 ]; do
    if ! kill -0 "$pid" >/dev/null 2>&1 && [ ! -e "$runtime" ]; then
        break
    fi
    attempt=$((attempt + 1))
    sleep 0.05
done
if kill -0 "$pid" >/dev/null 2>&1 || [ -e "$runtime" ]; then
    echo 'Attach E2E server did not stop cleanly' >&2
    exit 1
fi
pid=""

if [ -n "$evidence" ]; then
    cp "$temporary/detached-items" "$evidence/detached-items.json"
    cp "$temporary/source-after-fork" "$evidence/source-after-fork.json"
    cp "$temporary/media-items" "$evidence/media-items.json"
fi
printf '%s\n' 'attach E2E passed: auth + network + run + fork + media + TUI detach/reattach + cleanup'
