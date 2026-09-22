#!/bin/sh
set -eu

# Keep this deliberately narrower than a generic "apple" search. Haarness
# supports Apple platforms, so target triples, launchd plist declarations, and
# CSS system-font names are legitimate commercial-product content.
BLOCKED_MARKERS='blockedword|blockedword|blockedword|blockedword|blockedword|blockedword|x-blocked-[[:alnum:]_-]+'

check_repository() {
    repository=$1
    # Include non-ignored untracked files: the gate must catch a new source
    # file before it is staged, not only after it enters the index.
    if matches=$(git -C "$repository" grep --untracked -n -I -i -E "$BLOCKED_MARKERS" -- . \
        ':(exclude)scripts/check-commercial-boundary.sh' \
        ':(exclude)target/**'); then
        printf '%s\n' 'Commercial boundary check failed: internal markers were found in tracked files.' >&2
        printf '%s\n' "$matches" >&2
        return 1
    else
        status=$?
    fi

    if [ "$status" -eq 1 ]; then
        return 0
    fi
    printf '%s\n' "Commercial boundary check failed: git grep exited with status $status." >&2
    return "$status"
}

self_test() {
    test_root=$(mktemp -d "${TMPDIR:-/tmp}/haarness-commercial-boundary.XXXXXX")
    trap 'rm -rf "$test_root"' EXIT HUP INT TERM
    mkdir -p "$test_root/crates/example/src"
    git -C "$test_root" init -q

    printf '%s\n' \
        'const TARGET: &str = "aarch64-apple-darwin";' \
        'const FONT: &str = "-apple-system";' \
        'const PLIST_DTD: &str = "-//Apple//DTD PLIST 1.0//EN";' \
        >"$test_root/crates/example/src/lib.rs"
    git -C "$test_root" add crates/example/src/lib.rs
    if ! check_repository "$test_root" >/dev/null 2>&1; then
        printf '%s\n' 'Commercial boundary self-test failed: allowed platform strings were rejected.' >&2
        return 1
    fi

    printf '%s\n' 'blockedword in a new untracked source file' >"$test_root/untracked.txt"
    if check_repository "$test_root" >/dev/null 2>&1; then
        printf '%s\n' 'Commercial boundary self-test failed: an untracked file was missed.' >&2
        return 1
    fi
    rm "$test_root/untracked.txt"

    for marker in blockedword blockedword BLOCKEDWORD blockedword blockedword blockedword x-blocked-secret; do
        printf '%s\n' "$marker" >"$test_root/crates/example/src/lib.rs"
        if check_repository "$test_root" >/dev/null 2>&1; then
            printf '%s\n' "Commercial boundary self-test failed: $marker was not detected." >&2
            return 1
        fi
    done

    printf '%s\n' 'Commercial boundary self-test passed.'
}

if [ "${1:-}" = "--self-test" ]; then
    self_test
    exit 0
fi
if [ "$#" -ne 0 ]; then
    printf '%s\n' 'usage: check-commercial-boundary.sh [--self-test]' >&2
    exit 2
fi

repository=$(git rev-parse --show-toplevel)
check_repository "$repository"
printf '%s\n' 'Commercial boundary check passed.'
