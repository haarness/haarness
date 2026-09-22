# Continuing delegated tasks

Omit `task_id` when assigning independent work. Each call creates its own child session. Pass an existing `task_id` only when continuing that same child's work. If the child is running, the instruction joins its queued guidance and the task runs in the background; if it has finished, a new turn continues its conversation.

The September 13 conflict occurred when several parallel calls reused one completed child's ID. Each caller observed an idle child before waiting for an available agent slot. After that wait, the first caller started the child and the others failed with `child thread already has an active turn`. Separate audit scopes had also been assigned the same ID instead of creating separate children.

Task admission now serializes the decision for each child, including capacity waits and installation of its task registry entry. Waiting callers recheck the current runner and queue their instructions into it. Queueing holds the runner's acceptance gate so completion cannot race with delivery. A closing runner settles before a new turn starts. Parent ownership checks, cancellation, and the global concurrency limit remain enforced. The tool description explains when to create a child versus continue one.

This addresses the local scheduling conflict. A provider rejection is a separate result and still propagates normally.

## Verification

From the repository root:

```sh
cargo test -p haarness-server --test api concurrent_task_resumptions_recheck_the_child_after_waiting_for_capacity
cargo test -p haarness-server --test api
cargo build -p haarness-cli
python3 scripts/test-task-resume-tmux.py
```

The regression fills all four agent slots, then starts three concurrent continuations of one idle child. Before the fix it produced two active-turn conflicts. With the fix, all three instructions reach the same child exactly once. The API suite also covers independent parallel tasks, foreground-to-background promotion, queued input, and cancellation. The tmux workflow runs the same scenario through a real TUI and isolated daemon and checks persisted task results and child messages.

On September 13, 2026, all 94 server API tests and the core/server library tests passed, along with the real tmux workflow and three Electron agent lifecycle workflows (live task rows, targeted stopping, and unsuccessful/cancelled completion states). The debug CLI build passed. The tmux fixture uses the deterministic Echo provider so it can reproduce scheduling pressure without depending on a model choosing a particular tool sequence.

Existing daemon processes keep their loaded code until restarted. Build the CLI and reconnect to a restarted workspace daemon to use the fix in an existing development workspace; wait for ongoing work to finish before stopping that daemon.
