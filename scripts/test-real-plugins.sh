#!/bin/sh
set -eu

repo=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
binary=${HAARNESS_BIN:-"$repo/target/debug/haarness"}
marketplace=${HAARNESS_REAL_PLUGIN_MARKETPLACE:-https://github.com/anthropics/claude-code.git}
temporary=$(mktemp -d "${TMPDIR:-/tmp}/haarness-real-plugins.XXXXXX")
data="$temporary/data"
workspace="$temporary/workspace"
test_home="$temporary/home"
mkdir -p "$data" "$workspace" "$test_home"

cleanup() {
    "$binary" --data-dir "$data" --workspace "$workspace" serve --stop >/dev/null 2>&1 || true
    rm -rf -- "$temporary"
}
trap cleanup EXIT HUP INT TERM

if [ ! -x "$binary" ]; then
    cargo build -p haarness-cli
fi

export HAARNESS_DATA_DIR="$data"
export HAARNESS_TEST_PROVIDER=1
export HOME="$test_home"

"$binary" --workspace "$workspace" plugin marketplace add "$marketplace" >/dev/null
market_root="$data/data/plugins/marketplaces/claude-code-plugins"
revision=$(git -C "$market_root" rev-parse HEAD)

"$binary" --workspace "$workspace" plugin install \
    commit-commands@claude-code-plugins >/dev/null
"$binary" --workspace "$workspace" plugin install \
    plugin-dev@claude-code-plugins >/dev/null

plugin_output=$("$binary" --workspace "$workspace" plugin list)
printf '%s\n' "$plugin_output" | grep -F "commit-commands" >/dev/null
printf '%s\n' "$plugin_output" | grep -F "plugin-dev" >/dev/null

command_output=$("$binary" --workspace "$workspace" command list)
printf '%s\n' "$command_output" | grep -F "commit-commands:commit" >/dev/null
printf '%s\n' "$command_output" | grep -F "plugin-dev:create-plugin" >/dev/null

agent_output=$("$binary" --workspace "$workspace" agent list --all)
printf '%s\n' "$agent_output" | grep -F "plugin-dev:plugin-validator" >/dev/null

skill_output=$("$binary" --workspace "$workspace" skill list)
printf '%s\n' "$skill_output" | grep -F "plugin-dev:plugin-structure" >/dev/null

# The real command evaluates git context, so provide an actual repository.
git -C "$workspace" init -q
git -C "$workspace" -c user.name=Fixture -c user.email=fixture@example.test commit -q --allow-empty -m "Fixture"
run_output=$("$binary" --workspace "$workspace" run real-plugin-smoke \
    --provider echo --model echo-1 --command commit-commands:commit)
printf '%s\n' "$run_output" | grep -F "create a single git commit" >/dev/null

python3 "$repo/tests/fixtures/assert_real_plugins.py" "$data" >/dev/null

"$binary" --workspace "$workspace" plugin remove commit-commands >/dev/null
"$binary" --workspace "$workspace" plugin remove plugin-dev >/dev/null
test -f "$market_root/.claude-plugin/marketplace.json"

printf '%s\n' "real plugin smoke passed: anthropics/claude-code@$revision"
