#!/bin/sh
set -eu

session="haarness-compact-fork-$$"
test_root="$(mktemp -d "${TMPDIR:-/tmp}/haarness-compact-fork.XXXXXX")"
binary="${HAARNESS_BIN:-$PWD/target/debug/haarness}"

cleanup() {
  tmux kill-session -t "$session" 2>/dev/null || true
  if [ -x "$binary" ] && [ -d "$test_root/ws" ]; then
    env HAARNESS_DATA_DIR="$test_root" \
      "$binary" --workspace "$test_root/ws" serve --stop \
      >/dev/null 2>&1 || true
  fi
  rm -rf "$test_root"
}
trap cleanup EXIT INT TERM

workspace_runtime_path() (
  runtime_data=$1
  runtime_workspace=$(CDPATH= cd -- "$2" && pwd -P)
  runtime_id=$(jq -er --arg path "$runtime_workspace" \
    'first(.workspaces[] | select(.path == $path) | .id)' \
    "$runtime_data/data/workspaces.json")
  printf '%s/cache/workspaces/%s/runtime.json\n' "$runtime_data" "$runtime_id"
)

capture() {
  tmux capture-pane -p -t "$session":0.0 2>/dev/null || true
}

if [ -z "${HAARNESS_BIN:-}" ]; then
  cargo build -q -p haarness-cli
fi
mkdir -p "$test_root/ws"

# A running tmux server starts the pane with its own environment: the pane
# clears every HAARNESS_* setting it has from there before setting its own.
tmux new-session -d -x 110 -y 30 -s "$session" \
  "sh -c 'for name in \$(env | sed -n \"s/^\\(HAARNESS_[A-Za-z0-9_]*\\)=.*/\\1/p\"); do unset \"\$name\"; done; exec \"\$@\"' sh env HAARNESS_TELEMETRY=0 HAARNESS_DATA_DIR='$test_root' HAARNESS_TEST_PROVIDER=1 HAARNESS_DEFAULT_PROVIDER=echo HAARNESS_DEFAULT_MODEL=echo-1 '$binary' --workspace '$test_root/ws'"

# A debug build on a busy machine can take a while to start its server.
attempt=0
while [ "$attempt" -lt 400 ]; do
  pane="$(capture)"
  echo "$pane" | grep -q '/ commands' && break
  attempt=$((attempt + 1))
  sleep 0.1
done
if ! echo "$pane" | grep -q '/ commands'; then
  echo "$pane" >&2
  echo 'TUI did not reach its home screen' >&2
  exit 1
fi

tmux send-keys -l -t "$session":0.0 'FIRST COMPACTION E2E PROMPT'
tmux send-keys -t "$session":0.0 Enter
attempt=0
while [ "$attempt" -lt 150 ]; do
  pane="$(capture)"
  echo "$pane" | grep -q 'Echo: FIRST COMPACTION E2E PROMPT' \
    && echo "$pane" | grep -q 'Ready' \
    && break
  attempt=$((attempt + 1))
  sleep 0.1
done
if ! echo "$pane" | grep -q 'Echo: FIRST COMPACTION E2E PROMPT'; then
  echo "$pane" >&2
  echo 'First prompt did not complete through the real TUI/server/provider path' >&2
  exit 1
fi

tmux send-keys -l -t "$session":0.0 'SECOND FORK E2E PROMPT'
tmux send-keys -t "$session":0.0 Enter
attempt=0
while [ "$attempt" -lt 150 ]; do
  pane="$(capture)"
  echo "$pane" | grep -q 'Echo: SECOND FORK E2E PROMPT' \
    && echo "$pane" | grep -q 'Ready' \
    && break
  attempt=$((attempt + 1))
  sleep 0.1
done
if ! echo "$pane" | grep -q 'Echo: SECOND FORK E2E PROMPT'; then
  echo "$pane" >&2
  echo 'Second prompt did not complete through the real TUI/server/provider path' >&2
  exit 1
fi

runtime="$(workspace_runtime_path "$test_root" "$test_root/ws")"
runtime_url="$(jq -r '.url' "$runtime")"
runtime_token="$(jq -r '.token' "$runtime")"
source_id="$(env HAARNESS_DATA_DIR="$test_root" \
  "$binary" --workspace "$test_root/ws" sessions list --json \
  | jq -er '.[0].id')"

# Bare /compact must ask the server to produce a model summary. A caller-made
# placeholder would not begin with Echo in this deterministic provider gate.
tmux send-keys -l -t "$session":0.0 '/compact'
tmux send-keys -t "$session":0.0 Enter
attempt=0
while [ "$attempt" -lt 150 ]; do
  pane="$(capture)"
  # A reconnect can replace the transient notice before the next capture.
  # The durable model-generated checkpoint proves the UI command completed.
  source_items="$(curl -sf -H "Authorization: Bearer $runtime_token" "$runtime_url/v1/threads/$source_id/items" || true)"
  echo "$source_items" | jq -e 'any(.[]; .data.type == "compaction" and (.data.summary | startswith("Echo:")))' >/dev/null 2>&1 && break
  attempt=$((attempt + 1))
  sleep 0.1
done
if ! echo "$source_items" | jq -e 'any(.[]; .data.type == "compaction" and (.data.summary | startswith("Echo:")))' >/dev/null 2>&1; then
  echo "$pane" >&2
  echo 'Bare /compact did not complete in the attached TUI' >&2
  exit 1
fi

source_items="$(curl -sf -H "Authorization: Bearer $runtime_token" \
  "$runtime_url/v1/threads/$source_id/items")"
if ! echo "$source_items" | jq -e \
  'any(.[]; .data.type == "compaction" and (.data.summary | startswith("Echo:")))' \
  >/dev/null; then
  echo "$source_items" >&2
  echo 'Compaction did not persist a server-generated model summary' >&2
  exit 1
fi

# Message choices are assistant replies, newest first. Choose the first reply
# to retain it and its prompt while excluding the later exchange.
tmux send-keys -l -t "$session":0.0 '/fork'
tmux send-keys -t "$session":0.0 Enter
attempt=0
while [ "$attempt" -lt 100 ]; do
  pane="$(capture)"
  echo "$pane" | grep -q 'Fork session' \
    && echo "$pane" | grep -q 'Fork through this assistant reply' \
    && break
  attempt=$((attempt + 1))
  sleep 0.1
done
if ! echo "$pane" | grep -q 'Fork session'; then
  echo "$pane" >&2
  echo 'Semantic fork picker did not open' >&2
  exit 1
fi
tmux send-keys -t "$session":0.0 Down Down Enter

attempt=0
while [ "$attempt" -lt 150 ]; do
  pane="$(capture)"
  fork_id="$(env HAARNESS_DATA_DIR="$test_root" \
    "$binary" --workspace "$test_root/ws" sessions list --json \
    | jq -r --arg source "$source_id" \
      'first(.[] | select(.parentId == $source) | .id) // empty')"
  [ -n "$fork_id" ] && break
  attempt=$((attempt + 1))
  sleep 0.1
done
if [ -z "$fork_id" ]; then
  echo "$pane" >&2
  echo 'Assistant reply fork did not create a new session' >&2
  exit 1
fi

fork_id="$(env HAARNESS_DATA_DIR="$test_root" \
  "$binary" --workspace "$test_root/ws" sessions list --json \
  | jq -er --arg source "$source_id" \
    'first(.[] | select(.parentId == $source) | .id)')"
fork_items="$(curl -sf -H "Authorization: Bearer $runtime_token" \
  "$runtime_url/v1/threads/$fork_id/items")"
if ! echo "$fork_items" | jq -e \
  'any(.[]; .data.type == "assistant_message" and .data.text == "Echo: FIRST COMPACTION E2E PROMPT")' \
  >/dev/null; then
  echo "$fork_items" >&2
  echo 'Assistant reply fork excluded the selected reply' >&2
  exit 1
fi
if echo "$fork_items" | jq -e \
  'any(.[]; .data.type == "user_message" and .data.text == "SECOND FORK E2E PROMPT")' \
  >/dev/null; then
  echo "$fork_items" >&2
  echo 'Assistant reply fork incorrectly included the later prompt' >&2
  exit 1
fi

# A normal fork is editable, unlike a delegated child. Send through the real
# composer and verify the continuation belongs only to the fork.
tmux send-keys -l -t "$session":0.0 'CONTINUE ASSISTANT FORK'
tmux send-keys -t "$session":0.0 Enter
attempt=0
while [ "$attempt" -lt 150 ]; do
  pane="$(capture)"
  echo "$pane" | grep -q 'Echo: CONTINUE ASSISTANT FORK' && break
  attempt=$((attempt + 1))
  sleep 0.1
done
if ! echo "$pane" | grep -q 'Echo: CONTINUE ASSISTANT FORK'; then
  echo "$pane" >&2
  echo 'Fork is not editable through the TUI composer' >&2
  exit 1
fi
source_items="$(curl -sf -H "Authorization: Bearer $runtime_token" \
  "$runtime_url/v1/threads/$source_id/items")"
if echo "$source_items" | jq -e \
  'any(.[]; .data.type == "user_message" and .data.text == "CONTINUE ASSISTANT FORK")' >/dev/null; then
  echo 'Fork continuation leaked into the source session' >&2
  exit 1
fi
# Resume both conversations through the actual session picker as well as the
# fork picker. The saved thread object carries provenance through this path.
resume_from_picker() {
  picker_id=$1
  picker_expected=$2
  tmux send-keys -l -t "$session":0.0 '/sessions'
  tmux send-keys -t "$session":0.0 Enter
  attempt=0
  while [ "$attempt" -lt 150 ]; do
    capture | grep -q 'Sessions' && break
    attempt=$((attempt + 1))
    sleep 0.1
  done
  if ! capture | grep -q 'Sessions'; then
    capture >&2
    echo 'Session picker did not open' >&2
    exit 1
  fi
  tmux send-keys -l -t "$session":0.0 "$picker_id"
  tmux send-keys -t "$session":0.0 Enter
  attempt=0
  while [ "$attempt" -lt 150 ]; do
    pane="$(capture)"
    echo "$pane" | grep -q "$picker_expected" && break
    attempt=$((attempt + 1))
    sleep 0.1
  done
  if ! echo "$pane" | grep -q "$picker_expected"; then
    echo "$pane" >&2
    echo 'Session picker did not restore the requested conversation' >&2
    exit 1
  fi
}
resume_from_picker "$source_id" 'Echo: SECOND FORK E2E PROMPT'
resume_from_picker "$fork_id" 'Echo: CONTINUE ASSISTANT FORK'

# A fork taken while a call waits for its approval (in Review, a read outside
# the project asks) leaves that call out: the fork's copy would never get a
# result, and providers refuse a call without one, so every turn in the fork
# would fail. The fork answers in the TUI.
api() {
  curl -sf -H "Authorization: Bearer $runtime_token" -H 'Content-Type: application/json' "$@"
}
api -X PUT -d '{"mode":"review"}' "$runtime_url/v1/threads/$source_id/approval-mode" >/dev/null
api -d "{\"requestId\":\"$(uuidgen | tr 'A-Z' 'a-z')\",\"text\":\"!tool read {\\\"path\\\":\\\"/etc/hosts\\\"}\"}" \
  "$runtime_url/v1/threads/$source_id/turns" >/dev/null
attempt=0
while [ "$attempt" -lt 150 ]; do
  permission_id="$(api "$runtime_url/v1/permissions" \
    | jq -r --arg source "$source_id" 'first(.[] | select(.threadId == $source) | .permissionId) // empty')"
  [ -n "$permission_id" ] && break
  attempt=$((attempt + 1))
  sleep 0.1
done
if [ -z "$permission_id" ]; then
  api "$runtime_url/v1/threads/$source_id/items" >&2
  echo 'The waiting call did not ask for its approval' >&2
  exit 1
fi
unanswered='([.[] | .data | select(.type == "tool_call") | .call_id] - [.[] | .data | select(.type == "tool_result") | .call_id]) | length'
if [ "$(api "$runtime_url/v1/threads/$source_id/items" | jq "$unanswered")" != 1 ]; then
  echo 'The source chat does not show its waiting call' >&2
  exit 1
fi
waiting_fork="$(api -d '{"title":"Fork while waiting"}' "$runtime_url/v1/threads/$source_id/fork" | jq -er '.id')"
waiting_items="$(api "$runtime_url/v1/threads/$waiting_fork/items")"
if [ "$(echo "$waiting_items" | jq "$unanswered")" != 0 ]; then
  echo "$waiting_items" >&2
  echo 'The fork copied a call without its result' >&2
  exit 1
fi
resume_from_picker "$waiting_fork" 'Echo: SECOND FORK E2E PROMPT'
tmux send-keys -l -t "$session":0.0 'CONTINUE WAITING FORK'
tmux send-keys -t "$session":0.0 Enter
attempt=0
while [ "$attempt" -lt 150 ]; do
  pane="$(capture)"
  echo "$pane" | grep -q 'Echo: CONTINUE WAITING FORK' && break
  attempt=$((attempt + 1))
  sleep 0.1
done
if ! echo "$pane" | grep -q 'Echo: CONTINUE WAITING FORK'; then
  echo "$pane" >&2
  echo 'The fork taken while a call waited did not answer' >&2
  exit 1
fi
api -d '{"decision":"reject"}' "$runtime_url/v1/permissions/$permission_id" >/dev/null
capture > /tmp/haarness-assistant-fork-tmux.txt
echo 'Compaction and assistant reply fork tmux E2E passed'
