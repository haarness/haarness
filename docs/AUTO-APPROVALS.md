# Automatic approvals

Desktop and TUI now offer **Auto** and **Review** independently from `--yolo`.
Auto replies **once** to permission requests; it never adds an Always rule or changes the server's permission configuration. Explicit denies and agent permission ceilings still win. Questions remain manual. A failed reply remains available for manual review.

## Desktop

- Use the shield control at the left of the composer, or search for **Enable Auto approvals** / **Switch approvals to Review**.
- With a conversation open, the setting applies to that conversation and its child agents. Ordinary forks and other conversations keep their own setting.
- On a new chat, the control sets the default for that workspace/checkout. A conversation's explicit Review setting overrides that default.
- **Allow once + Auto** answers the visible request first and enables Auto only after the reply succeeds and the view/mode has not changed. From a child agent view it applies only to that child and its descendants; the parent and siblings keep their mode. Search can switch that child back to Review.
- Enabling Auto handles eligible pending requests in that scope. Settings persist across Desktop reloads. Streams stay connected for locations with Auto enabled, including collapsed projects and background conversations, while the window is open.
- Worktrees, remote connections, and projects have separate identities and settings. Enabling Auto in one does not grant Auto to another.

## TUI

- **Ctrl+Y** toggles Auto/Review. `/permissions-mode auto`, `/permissions-mode review`, and `/permission-mode` work through the command menu too.
- A pending permission offers **Once + Auto** (`u`). It enables Auto only after a successful one-time reply.
- Mode is local to the current TUI connection and lasts until exit. It covers new requests from all sessions and child agents on that workspace's server. Already-seen pending requests stay manual when Auto is enabled.
- Reconnects preserve the mode. Newly discovered requests can be answered automatically; replayed resolved requests never reappear, and failed requests are not retried in a loop.
- The status band shows the mode when space permits, keeping the location, queue and interrupt hints readable in narrow terminals.

## Verification commands

```sh
npm --prefix apps/desktop test
cargo test --release -p haarness-tui --lib
cargo test --release -p haarness-core --lib
cargo build --release -p haarness-cli
HAARNESS_BIN="$PWD/target/release/haarness" python3 scripts/test-auto-mode.py
HAARNESS_BIN="$PWD/target/release/haarness" npm --prefix apps/desktop run test:e2e -- --grep 'Auto approvals allow|Auto from a child|workspace Auto stays|SSE chat fabric|dictation inserts|universal search opens settings'
```

Desktop tests drive actual Electron controls against a real Haarness server. The TUI script uses a real tmux terminal and real server with the deterministic test provider; it checks filesystem effects, pending requests, child/background work, deny rules, questions, reconnects and relaunch behavior. No paid model or external side effects are needed.

The event bus serializes cursor allocation through replay and live delivery, so concurrent agents cannot overtake an earlier approval event. Its regression test publishes 8,000 events from eight threads and checks both delivery paths in cursor order.
