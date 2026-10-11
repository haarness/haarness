#!/bin/sh
set -eu

repo=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
binary=${HAARNESS_BIN:-"$repo/target/debug/haarness"}
fixture="$repo/tests/fixtures/ecosystem-plugin"
root_fixture="$repo/tests/fixtures/root-source-marketplace"
temporary=$(mktemp -d "${TMPDIR:-/tmp}/haarness-ecosystem-smoke.XXXXXX")
data="$temporary/data"
workspace="$temporary/workspace"
test_home="$temporary/home"
mkdir -p "$data" "$workspace" "$test_home"

workspace_runtime_path() (
    runtime_data=$1
    runtime_workspace=$(CDPATH= cd -- "$2" && pwd -P)
    runtime_id=$(jq -er --arg path "$runtime_workspace" \
        'first(.workspaces[] | select(.path == $path) | .id)' \
        "$runtime_data/data/workspaces.json")
    printf '%s/cache/workspaces/%s/runtime.json\n' "$runtime_data" "$runtime_id"
)

cleanup() {
    "$binary" --data-dir "$data" --workspace "$workspace" serve --stop >/dev/null 2>&1 || true
    rm -rf -- "$temporary"
}
trap cleanup EXIT HUP INT TERM

if [ ! -x "$binary" ]; then
    cargo build -p haarness-cli
fi
command -v jq >/dev/null

export HAARNESS_DATA_DIR="$data"
export HAARNESS_TEST_PROVIDER=1
export HOME="$test_home"

"$binary" --workspace "$workspace" plugin install "$fixture" >/dev/null

root_market="$temporary/root-source-marketplace"
mkdir -p "$root_market"
cp -R "$root_fixture/." "$root_market/"
ln -s SKILL.md "$root_market/skills/root-source/ALIAS.md"
mkdir -p "$root_market/.git"
touch "$root_market/.git/not-plugin-content"
"$binary" --workspace "$workspace" plugin marketplace add "$root_market" >/dev/null
"$binary" --workspace "$workspace" plugin install \
    root-source-plugin@root-source-market >/dev/null

"$binary" --workspace "$workspace" --trust-project agent create managed-reviewer \
    --description "Managed smoke agent" --prompt "Preserve managed markers." \
    --model echo/echo-1 --mode all --tool bash=false \
    --permission 'bash:*=deny' --project >/dev/null
"$binary" --workspace "$workspace" --trust-project command create managed-check \
    --description "Managed smoke command" --template 'MANAGED_COMMAND_OK $ARGUMENTS' \
    --agent managed-reviewer --model echo/echo-1 --project >/dev/null
"$binary" --workspace "$workspace" --trust-project skill create managed-inspect \
    --description "Managed smoke skill" --instructions "MANAGED_SKILL_OK" \
    --project >/dev/null

managed_agent=$("$binary" --workspace "$workspace" --trust-project agent get managed-reviewer)
printf '%s\n' "$managed_agent" | grep -F '"model": "echo/echo-1"' >/dev/null
printf '%s\n' "$managed_agent" | grep -F '"bash": false' >/dev/null
printf '%s\n' "$managed_agent" | grep -F '"pattern": "bash:*"' >/dev/null
printf '%s\n' "$managed_agent" | grep -F '"decision": "deny"' >/dev/null
managed_command=$("$binary" --workspace "$workspace" --trust-project command get managed-check)
printf '%s\n' "$managed_command" | grep -F "MANAGED_COMMAND_OK" >/dev/null
managed_skill=$("$binary" --workspace "$workspace" --trust-project skill get managed-inspect)
printf '%s\n' "$managed_skill" | grep -F "MANAGED_SKILL_OK" >/dev/null

plugin_output=$("$binary" --workspace "$workspace" plugin list)
printf '%s\n' "$plugin_output" | grep -F "  ●  ecosystem-smoke  haarness" >/dev/null
printf '%s\n' "$plugin_output" | grep -F "  ●  root-source-plugin  haarness" >/dev/null
printf '%s\n' "$plugin_output" | grep -F "  root-source-market  " >/dev/null

root_skill=$("$binary" --workspace "$workspace" skill get root-source-skill)
printf '%s\n' "$root_skill" | grep -F "ROOT_SOURCE_SKILL_OK" >/dev/null
python3 "$repo/tests/fixtures/assert_root_marketplace.py" "$data" "$root_market" >/dev/null

agent_output=$("$binary" --workspace "$workspace" agent list --all)
printf '%s\n' "$agent_output" | grep -F "ecosystem-smoke:reviewer" >/dev/null

command_output=$("$binary" --workspace "$workspace" command list)
printf '%s\n' "$command_output" | grep -F "ecosystem-smoke:ship" >/dev/null

skill_output=$("$binary" --workspace "$workspace" skill list)
printf '%s\n' "$skill_output" | grep -F "ecosystem-smoke-inspect" >/dev/null

skill_body=$("$binary" --workspace "$workspace" skill get ecosystem-smoke-inspect)
printf '%s\n' "$skill_body" | grep -F "preserve it exactly" >/dev/null

mcp_output=$("$binary" --workspace "$workspace" mcp list)
printf '%s\n' "$mcp_output" | grep -F "ecosystem-smoke" | grep -F "connected" >/dev/null

runtime=$(workspace_runtime_path "$data" "$workspace")
python3 "$repo/tests/fixtures/assert_custom_agent.py" "$runtime" >/dev/null

tool_output=$("$binary" --workspace "$workspace" mcp tools ecosystem-smoke)
printf '%s\n' "$tool_output" | grep -F "mcp__ecosystem-smoke__echo" >/dev/null

call_output=$("$binary" --workspace "$workspace" mcp call ecosystem-smoke echo --arguments '{"message":"plugin-smoke"}')
printf '%s\n' "$call_output" | grep -F "MCP_ECHO_OK:plugin-smoke" >/dev/null

resource_output=$("$binary" --workspace "$workspace" mcp resources ecosystem-smoke)
printf '%s\n' "$resource_output" | grep -F "smoke://fixture/readme" >/dev/null
template_output=$("$binary" --workspace "$workspace" mcp resource-templates ecosystem-smoke)
printf '%s\n' "$template_output" | grep -F 'smoke://fixture/{name}' >/dev/null
read_output=$("$binary" --workspace "$workspace" mcp read ecosystem-smoke smoke://fixture/readme)
printf '%s\n' "$read_output" | grep -F "MCP_RESOURCE_OK" >/dev/null
prompt_output=$("$binary" --workspace "$workspace" mcp prompts ecosystem-smoke)
printf '%s\n' "$prompt_output" | grep -F '"name": "review"' >/dev/null
resolved_prompt=$("$binary" --workspace "$workspace" mcp prompt ecosystem-smoke review --arguments '{"target":"plugin-smoke"}')
printf '%s\n' "$resolved_prompt" | grep -F "MCP_PROMPT_OK:plugin-smoke" >/dev/null

run_output=$("$binary" --workspace "$workspace" run smoke-payload --provider echo --model echo-1 --agent ecosystem-smoke:reviewer --command ecosystem-smoke:ship)
printf '%s\n' "$run_output" | grep -F "PLUGIN_COMMAND_OK smoke-payload" >/dev/null

tool_policy_output=$("$binary" --workspace "$workspace" run \
    '!tool bash {"command":"touch agent-tool-policy-breach"}' \
    --provider echo --model echo-1 --agent ecosystem-smoke:reviewer)
printf '%s\n' "$tool_policy_output" | grep -E "tool bash was not advertised|tool bash is disabled" >/dev/null
test ! -e "$workspace/agent-tool-policy-breach"

permission_output=$("$binary" --workspace "$workspace" run \
    '!tool write {"path":"agent-permission-breach","content":"breach"}' \
    --provider echo --model echo-1 --agent ecosystem-smoke:reviewer)
printf '%s\n' "$permission_output" | grep -E "tool write was not advertised|tool write is disabled" >/dev/null
test ! -e "$workspace/agent-permission-breach"

# A disabled definition avoids touching the host scheduler while still
# exercising the durable child-runner boundary. The internal trigger emits
# compact JSON for native logs; the public command must print its formatted
# result exactly once.
schedule_output=$("$binary" --workspace "$workspace" schedule add \
    --name ecosystem-schedule --prompt "scheduled ecosystem smoke" \
    --model echo/echo-1 --every 1m --disabled)
schedule_id=$(printf '%s\n' "$schedule_output" | awk '{print $1}')
schedule_run=$("$binary" --workspace "$workspace" schedule run "$schedule_id" --json)
schedule_id_fields=$(printf '%s\n' "$schedule_run" | awk '/"id":/ { count += 1 } END { print count + 0 }')
test "$schedule_id_fields" -eq 1
printf '%s\n' "$schedule_run" | grep -F '"status": "success"' >/dev/null
printf '%s\n' "$schedule_run" | grep -F '"response": "Echo: scheduled ecosystem smoke"' >/dev/null
schedule_history=$("$binary" --workspace "$workspace" schedule history "$schedule_id" --json)
printf '%s\n' "$schedule_history" | grep -F '"source": "manual"' >/dev/null
"$binary" --workspace "$workspace" schedule remove "$schedule_id" >/dev/null

disconnect_output=$("$binary" --workspace "$workspace" mcp disconnect ecosystem-smoke)
printf '%s\n' "$disconnect_output" | grep -F "disconnected" >/dev/null

reconnect_output=$("$binary" --workspace "$workspace" mcp reconnect ecosystem-smoke)
printf '%s\n' "$reconnect_output" | grep -F "connected" >/dev/null

"$binary" --workspace "$workspace" agent remove managed-reviewer --project >/dev/null
"$binary" --workspace "$workspace" command remove managed-check --project >/dev/null
"$binary" --workspace "$workspace" skill remove managed-inspect --project >/dev/null
"$binary" --workspace "$workspace" plugin remove root-source-plugin >/dev/null
test -f "$root_market/.claude-plugin/marketplace.json"

printf '%s\n' "ecosystem smoke passed: plugins + managed policy + MCP lifecycle + durable schedules"
