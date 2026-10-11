#!/bin/sh
set -eu

repo=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
binary=${HAARNESS_BIN:-"$repo/target/debug/haarness"}
temporary=$(mktemp -d "${TMPDIR:-/tmp}/haarness-session-archive.XXXXXX")
source_data="$temporary/source-data"
target_data="$temporary/target-data"
source_workspace="$temporary/source-workspace"
target_workspace="$temporary/target-workspace"
full_archive="$temporary/full.json"
sanitized_archive="$temporary/sanitized.json"

cleanup() {
    "$binary" --data-dir "$source_data" --workspace "$source_workspace" \
        serve --stop >/dev/null 2>&1 || true
    "$binary" --data-dir "$target_data" --workspace "$target_workspace" \
        serve --stop >/dev/null 2>&1 || true
    rm -rf -- "$temporary"
}
trap cleanup EXIT HUP INT TERM

mkdir -p "$source_data" "$target_data" "$source_workspace" "$target_workspace"
if [ ! -x "$binary" ]; then
    cargo build -q -p haarness-cli
fi
command -v jq >/dev/null

source_cli() {
    env HAARNESS_TEST_PROVIDER=1 "$binary" --data-dir "$source_data" \
        --workspace "$source_workspace" "$@"
}
target_cli() {
    env HAARNESS_TEST_PROVIDER=1 "$binary" --data-dir "$target_data" \
        --workspace "$target_workspace" "$@"
}

source_cli run --provider echo --model echo-1 --title 'Archive root secret' \
    'root transcript secret' >/dev/null
root_id=$(source_cli sessions list --json | jq -er \
    '.[] | select(.title == "Archive root secret") | .id')
child_id=$(source_cli sessions fork "$root_id" --title 'Archive child secret' --json | jq -er '.id')
source_cli --session "$child_id" run --provider echo --model echo-1 \
    'child transcript secret' >/dev/null

source_cli sessions export "$root_id" --output "$full_archive" >/dev/null
source_cli sessions export "$root_id" --sanitize --output "$sanitized_archive" >/dev/null

jq -e --arg root "$root_id" '
    .format == "haarness.session-archive" and
    .schemaVersion == 1 and
    .rootThreadId == $root and
    .sanitized == false and
    (.threads | length) == 2 and
    ([.threads[].items[].data.type] | index("snapshot") == null)
' "$full_archive" >/dev/null
grep -F 'root transcript secret' "$full_archive" >/dev/null
grep -F 'child transcript secret' "$full_archive" >/dev/null
jq -e '.sanitized == true and (.threads | length) == 2' "$sanitized_archive" >/dev/null
if grep -F 'root transcript secret' "$sanitized_archive" >/dev/null || \
   grep -F 'child transcript secret' "$sanitized_archive" >/dev/null || \
   grep -F 'Archive root secret' "$sanitized_archive" >/dev/null; then
    echo 'sanitized session archive leaked source content' >&2
    exit 1
fi

first=$(target_cli sessions import "$full_archive" --json)
printf '%s\n' "$first" | jq -e --arg root "$root_id" \
    '.root.id == $root and (.created | length) == 2 and (.merged | length) == 0' >/dev/null
target_cli sessions get "$root_id" --transcript | grep -F 'root transcript secret' >/dev/null
target_cli sessions get "$child_id" --transcript | grep -F 'child transcript secret' >/dev/null
target_cli sessions get "$child_id" | jq -e --arg root "$root_id" '.parentId == $root' >/dev/null

second=$(target_cli sessions import "$full_archive" --json)
printf '%s\n' "$second" | jq -e \
    '(.created | length) == 0 and (.merged | length) == 2' >/dev/null
test "$(target_cli sessions list --json | jq 'length')" -eq 2

jq '.schemaVersion = 99' "$full_archive" > "$temporary/unsupported.json"
if target_cli sessions import "$temporary/unsupported.json" >/dev/null 2>&1; then
    echo 'unsupported session archive schema was accepted' >&2
    exit 1
fi
test "$(target_cli sessions list --json | jq 'length')" -eq 2

printf '%s\n' 'session archive smoke passed: tree export + sanitization + transactional idempotent import'
