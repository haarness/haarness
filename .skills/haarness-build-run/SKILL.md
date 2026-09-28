---
name: haarness-build-run
description: Build, run, prototype, and test Haarness CLI, TUI, server, and Electron Desktop while keeping rebuilds fast and build storage controlled. Use for local build/run work and artifact cleanup in this repository.
---

# Haarness build and run

Run commands from the repository containing `Cargo.toml` and `apps/desktop`, not its parent directory. Preserve existing uncommitted work. Match the build to the changed surface; a renderer edit should not rebuild Rust or create an installer.

## Choose the shortest development loop

| Changed surface | Normal action |
| --- | --- |
| Desktop renderer/styles | Reuse the backend; run Electron Vite dev with hot reload |
| Electron main/preload | Use the same dev process; restart its Electron instance when needed |
| TypeScript SDK | Build `sdks/typescript`, then use Desktop dev; reinstall only if dependencies changed |
| Rust CLI/TUI/server | `cargo build -p haarness-cli`, then run the new binary; restart the affected development daemon if backend code changed |
| Remote hosts/published chats/Cloud relay | Use the [remote-access testing skill](../../skills/haarness-remote-access/SKILL.md) with matching native/Cloud versions; include Chrome, tmux and reconnect proof for user-facing changes |
| One Rust library | `cargo check -p` or `cargo test -p` for that crate before building the app if needed |
| Installer/release requested | Follow [Desktop packaging](../../docs/DESKTOP.md#package-and-launch) using the correct binaries and Bun version |

`npm run desktop:dev` at the repository root also builds Rust and the SDK. That is convenient for initial setup, but use the direct Desktop command below for repeated renderer work. Root `desktop:build` and `desktop:package` perform release work; do not use them as the UI edit loop.

## Build and run

Use the repository's `rust-toolchain.toml`. Desktop needs Node 22+ and npm. Bun/native dependencies are described in [Desktop](../../docs/DESKTOP.md) and the checked-in vendor manifests; preserve vendor archives.

Only when dependencies are missing or their manifests/lockfiles changed:

```sh
npm run desktop:install
```

Build the backend if absent or Rust changed:

```sh
cargo build -p haarness-cli
```

Build the SDK if absent or its source/API contract changed, then launch Desktop:

```sh
npm --prefix sdks/typescript run build
HAARNESS_BIN="$PWD/target/debug/haarness" npm --prefix apps/desktop run dev
```

Keep the dev process running for renderer hot reload. `HAARNESS_BIN` must point to the intended current binary; it does not replace an already-running daemon. When backend code changes, identify the daemon for the development workspace, stop that daemon gracefully, then reconnect/relaunch. Do not stop unrelated user work or every process named Haarness.

For TUI/server work, set `project_dir` to the requested project or an isolated fixture:

```sh
project_dir="$PWD"
# Interactive TUI, using the workspace's daemon:
target/debug/haarness --workspace "$project_dir"
# Or a foreground server (choose an unused port):
target/debug/haarness --workspace "$project_dir" serve --host 127.0.0.1 --port 4096
# Graceful shutdown of the development workspace daemon when needed:
target/debug/haarness --workspace "$project_dir" serve --stop
```

Check `/healthz` on the server you launched; attach using `haarness attach <url>` and that server's configured authentication. Do not silently add project trust or bypass approvals just to launch. For disposable Desktop tests, isolate both `HAARNESS_DESKTOP_DATA` (UI preferences) and `HAARNESS_DATA_DIR` (server state); keep their directories until the fixture's processes have stopped. Prefer existing E2E fixtures, which own their cleanup.

## Verify the changed surface

```sh
# Choose affected packages, for example:
cargo test -p haarness-protocol
npm --prefix apps/desktop run typecheck
npm --prefix apps/desktop test
# Renderer/main production build, without packaging or rebuilding Rust:
npm --prefix apps/desktop run build
# Example focused Electron workflow against the selected backend:
HAARNESS_BIN="$PWD/target/debug/haarness" npm --prefix apps/desktop run test:e2e -- --grep 'deferred input queue'
```

Choose the E2E filter for the actual change. TUI changes need a relevant real tmux workflow; Desktop changes need the relevant Electron flow and visual inspection. For protocol changes, regenerate and build the SDKs and run relevant SDK integration checks. Broad scripts such as `scripts/test-all.sh`, `scripts/test-desktop.sh`, and `scripts/test-sdks.sh` are full gates, not the inner edit loop; some reinstall dependencies or build release artifacts. Inspect them before running. Never run dependency installation while another test/build uses the same `node_modules`.

## Keep speed and storage balanced

- Default `dev`/`test` uses incremental compilation with `debug = "line-tables-only"`: retain compiler state and file/line backtraces without full variable/type debug data. Keep debug assertions and overflow checks.
- Reuse the ordinary `target` and installed dependencies across iterations. Avoid repeatedly switching toolchains, profiles, features or compiler flags. Do not write unchanged files or run whole-tree formatters unnecessarily; needless changes trigger rebuilds.
- Do **not** clean before/after each build, on every test, or automatically at a size threshold. Warm caches are useful. Inspect `df -h .` and `du -sh target apps/desktop/node_modules apps/desktop/release` when disk pressure or unexpectedly large growth warrants it.
- Start with the narrow cleanup preview below; apply only when cache removal is within the task scope and there is a concrete space need. It refuses active artifacts and retains runnable binaries, release outputs, Desktop and user data.

```sh
python3 scripts/clean-build-artifacts.py --incremental-only
# Same scope, after reviewing the preview:
python3 scripts/clean-build-artifacts.py --incremental-only --apply
# Broader stale debug intermediates, when a cold rebuild is worth the space:
python3 scripts/clean-build-artifacts.py
```

Do not add `--router` unless cleanup of the sibling Router is also in scope. Full cleanup uses `--apply`; expect a cold dependency rebuild afterward. Do not use `git clean -fdx` as build cleanup. Do not remove sessions, credentials, vendor archives, active checkouts, current executables or application installs to save build space.

For exceptional low-space sessions, use `CARGO_INCREMENTAL=0` consistently for that session. For full variable inspection, use `cargo build --profile debugger -p haarness-cli` and `target/debugger/haarness`; that extra profile costs disk space. See [build storage](../../docs/BUILD-STORAGE.md) for the cleanup boundaries. Report which surface/build was verified and any cold-build cost introduced by cleanup.
