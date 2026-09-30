# Reliability verification — 2026-09-26

Applied to Haarness from base `c906c63c4fdf9897dca3fcbb2232144200005351`.
The changes cover the native Rust runtime and Electron application.

## Changes

| Area | Result |
| --- | --- |
| Agent switch | Resolve destination/model before consent, show both clients the model change, scope remembered consent to one session and literal agent name, reject stale consent |
| Filtered responses | Preserve partial text/reasoning and usage, report a non-retryable failure, pair pending tool calls with failed results without executing them |
| Cloud model discovery | Retain the last successful catalog through transient failures; successful empty results remove membership; account/credential changes and logout fence stale results |
| MCP lifecycle | Stop the dedicated process group and snapshotted descendants, including detached grandchildren, on disconnect, reconnect and failed initialization |
| Unicode labels | Truncate whole graphemes and preserve them through selection/hover styling |

## Validation

All completed successfully on macOS:

- Rust library tests: core 116, providers 93, MCP 21, TUI 355.
- Server API integration tests: 105. CLI tests: 110. Total Rust tests: **800**.
- Desktop typecheck, production build and **147** automated tests.
- Real Electron end-to-end test: agent-switch consent/model handoff and filtered response persistence, with both screenshots inspected.
- `scripts/test-reliability-adaptations.py`: **nine** real daemon/provider HTTP/tmux scenarios covering consent, remembered scope, destination isolation, stale-consent rejection, filtered OpenAI and Cloud wire formats, catalog outage/empty/recovery, MCP descendant cleanup, and a narrow Unicode session picker.
- `scripts/test-recent-fixes-tmux.py`: permission, shell/path, undo/redo, usage and MCP crash/recovery regressions.
- `scripts/test-permission-parity-tmux.py`: five repository/worktree, context and nested-agent permission scenarios.
- Current debug CLI build and `git diff --check`.

The new test entry points are:

```sh
cargo test -p haarness-core -p haarness-providers -p haarness-mcp -p haarness-tui --lib
cargo test -p haarness-server --test api
cargo test -p haarness-cli --bin haarness
cargo build -p haarness-cli
python3 scripts/test-reliability-adaptations.py
HAARNESS_BIN="$PWD/target/debug/haarness" npm --prefix apps/desktop run test:e2e -- --grep 'agent switch discloses model'
```

The Python test retains terminal captures under `target/reliability-adaptations/tmux`.
Playwright retains screenshots under `apps/desktop/test-results`.
Fixtures isolate account, workspace, daemon and UI state and stop their own processes.

## Boundaries

Provider-wire tests use controlled local HTTP servers and the actual provider adapters. The separate external live-provider script could not run because its required saved API credential was unavailable. Windows Job Object cleanup was implemented but was not exercised on this macOS host. Unix descendant cleanup snapshots ancestry before shutdown; a detached process already reparented before that snapshot is outside that ancestry proof. Credential renewal conservatively starts a fresh Cloud catalog instead of retaining another credential's cache.

Cloud control-plane containers and Fly agent images are not rebuilt or deployed by this change.
