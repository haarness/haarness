# Recent reliability fixes

These fixes cover redirect parsing, MCP transport recovery, durable approval errors, prepared-request accounting, per-server LSP timeouts, and shell-option decoding for undo checkpoints.

Shell permissions inspect parsed redirect destinations, including quoted paths, input redirects, standalone redirects and literal prefixes before substitutions. Text that merely contains a `>` does not request file access. Descriptor duplication such as `2>&1` remains unchanged. Quoted command options are decoded before identifying read-only commands, so writes such as `sort input '-o' sorted` receive undo checkpoints.

MCP transports are observed while idle as well as during calls. A closed transport clears live callable tools and marks the server failed, followed by one automatic reconnect attempt. Previously discovered schemas can remain in model context under the conditions below. Failed reconnection remains visible for manual retry. Recovery does not replay an interrupted tool call: it may already have performed its action. Explicit disconnects and newer configurations supersede pending connection attempts. Startup and discovery share the configured connection timeout; a Streamable HTTP tool call, and the response stream carrying its result, may run longer.

Desktop retains reply failures with the connection and permission request until a successful reply or authoritative resolution. Navigating or performing unrelated actions does not erase the warning. Auto remains disabled when its initial approval fails.

New usage records optionally contain `prompt`, a snapshot of estimated system, tool-schema and history tokens after request hooks, plus the number of media attachments. The Desktop context inspector displays this separately from the retained-text estimate and provider-reported usage. Estimates use four Unicode code points per token; they exclude binary media and provider framing. Older servers and saved conversations without this field remain supported. Both SDKs expose the optional field.

Language servers can override the global timeouts individually:

```toml
[lsp]
request_timeout_ms = 10000
diagnostic_timeout_ms = 3000

[lsp.servers.rust]
command = ["rust-analyzer"]
extensions = [".rs"]
initialize_timeout_ms = 30000
request_timeout_ms = 15000
diagnostic_timeout_ms = 5000
```

An omitted override inherits the existing global setting. Initialization falls back to the server request timeout, then the global request timeout. Cancellation remains available independently of these limits.

## Regression checks

- `cargo test -p haarness-core -p haarness-mcp -p haarness-language`
- `HAARNESS_BIN="$PWD/target/debug/haarness" python3 scripts/test-recent-fixes-tmux.py`
- `HAARNESS_BIN="$PWD/target/debug/haarness" python3 scripts/test-extensions.py`
- Desktop Playwright cases: `permission reply failures remain`, `conversation context inspector`, and `Auto approvals allow once`.
- Broader gates: Rust workspace tests, Clippy, SDK checks, Desktop tests, and existing tmux flows.

The real TUI tests wait for approval controls to render and disappear before sending keys. Goal-menu checks wait for Escape to close the current modal; compaction checks inspect the persisted model summary because reconnect notices can replace a transient success message. The existing extension infinite-loop test uses a two-second deadline, matching other extension fixtures, so cold Bun initialization can finish before the intended execution-timeout assertion. It still checks termination and rejects replay.

## Verification on 2026-09-12

The final Rust workspace run passed all 947 tests with no failures or ignored tests. Workspace Clippy (`--all-targets -- -D warnings`), formatting, architecture and commercial-boundary checks passed. Desktop's 82 tests, type checking and production build passed.

Real Electron tests passed Auto approval inheritance and reload, context-inspector accounting and conversation isolation, and failed permission replies that survive successful preference changes and then clear after retry. Captured screenshots were inspected. Real tmux tests passed quoted external allow/deny, input redirects, literal redirect-like text, quoted-write undo/redo, prepared usage, MCP recovery without replay, and compaction/fork behavior. The extension end-to-end gate verified prepared system accounting against the request actually received by its HTTP provider after hooks.

The complete SDK gate passed TypeScript and synchronous/asynchronous Python operation coverage, server lifecycle, stream cancellation, typing, and clean installation of built packages. A separate check verified the final binary's live schema against generated SDK types.

The full existing `scripts/test-tmux.sh` gate also completed with exit code 0, covering daemon lifecycle, approvals, goals, management catalogs, and Git worktree workflows in addition to the dedicated fixes.

These flows ran on macOS using real CLI/server/Electron/TUI processes and deterministic provider, MCP and language-server fixtures. They establish regression coverage for the exercised behavior; they do not constitute live-provider or cross-platform certification. Local logs, screenshots and the tested binary's SHA-256 are retained under `target/recent-fixes-verification/`.

## Reliability improvements on 2026-09-19

The following fixes were adapted to Haarness's Rust runtime and Solid Desktop:

- When `load_tool` is denied or disabled, allowed tools are advertised directly. Permission-denied tools remain excluded.
- `/btw` reuses the prepared main request's context and schemas; tool attempts receive local refusals and cannot execute.
- Side questions stop their transcript snapshot before the first unfinished tool call. Existing queued-input wrapping is retained.
- A missing bare MCP launcher can be found in standard user installation directories, with its runtime directory added only to the child process's PATH.
- MCP schemas survive transient disconnects under the same connection identity and are invalidated on catalog changes, configuration changes, explicit disconnect, or OAuth logout.
- Transcript and review rows release detached virtualizer measurements after DOM removal.

For MCP discovery, `HAARNESS_MCP_PATH` supplies additional absolute fallback
directories. Explicit command paths and per-server PATH settings are respected;
an installed command that fails is not replaced with another installation.
Remembered schemas are context only: disconnected tools still fail normally,
and no call is replayed. The identity includes effective configuration and OAuth
credentials as a digest. Servers using dynamic header helpers do not retain
offline schemas. A failed `tools/list_changed` refresh discards old schemas too.

See [Side questions](SIDE-QUESTIONS.md) for snapshot limits and cold-session
behavior. This adaptation does not add automatic continuation of interrupted
turns after a server crash; the existing explicit-resume behavior is retained.

The reproducible end-to-end checks are:

```sh
cargo test --workspace
HAARNESS_BIN="$PWD/target/debug/haarness" python3 scripts/test-runtime-reliability.py
# Optional live request using an existing OpenAI API credential:
HAARNESS_BIN="$PWD/target/debug/haarness" python3 scripts/test-runtime-reliability-live.py
HAARNESS_BIN="$PWD/target/debug/haarness" python3 scripts/test-recent-fixes-tmux.py
HAARNESS_BIN="$PWD/target/debug/haarness" python3 scripts/test-input-queue-api.py
HAARNESS_BIN="$PWD/target/debug/haarness" python3 scripts/test-input-queue-tmux.py
HAARNESS_BIN="$PWD/target/debug/haarness" sh scripts/test-compaction-fork-tmux.sh
HAARNESS_BIN="$PWD/target/debug/haarness" npm --prefix apps/desktop run test:e2e -- timeline-cleanup.spec.ts review-scaling.spec.ts
```

The provider fixture checks actual Anthropic and OpenAI HTTP requests, main/side
context parity during a held MCP call, refusal without transcript mutation,
launcher discovery, and reconnect without replay. Electron checks cover a
1,000-message transcript, session switching, a subsequent reply, and large
repository reviews. Verification logs and inspected screenshots are retained in
`target/runtime-reliability/`; its report records the final results and limits.

The final workspace run passed 1,035 Rust tests with no failures or ignored
tests. All 133 Desktop tests, typechecking, production build, architecture
checks, and four focused Electron tests passed. The real provider fixtures,
CLI/TUI checks, queue hard-restart test, and compaction/fork test passed. A live
`gpt-5.4-mini` request used the allowed MCP tool, and its side answer recalled
the returned marker without another tool call or transcript mutation.

Workspace Clippy completed with warnings outside the changed lines; strict
`-D warnings` remains blocked first by the existing `DefinitionExecution`
boolean-count warning. No cross-platform run or packaged-app release was performed.

## Reliability improvements on 2026-09-21

These changes preserve Haarness's workspace daemons, Rust MCP registry and
Desktop IPC storage.

- OAuth logout revokes live and remembered MCP tools in every workspace process sharing the stored credential. Late discovery and token refresh cannot restore the logged-out identity; explicit authorization headers remain independent.
- Desktop review drafts survive navigation and reload. Delivery pins the destination conversation, validates it, and persists the composer text and a receipt together. Retrying a failed write cannot append the same revision twice, and a late completion cannot erase a newer edit.
- Desktop and TUI recall the current conversation's prompts first, followed by other prompts in that workspace. Desktop preserves recall in memory when storage fails and merges unread history after recovery.

MCP logout uses a durable credential generation marker and a file lock shared
by workspace processes. Requests check the captured generation before execution;
pending connections and catalog refreshes check it before publishing results.
Reauthentication creates a fresh lease. Calls already sent before logout are
not replayed. The scope follows Haarness's existing name-based credential store.

Desktop review drafts are scoped by connection, conversation and diff mode.
Adding a comment still places it in the message composer; sending the message
remains a separate action. Each edited revision has its own delivery receipt.

The TUI writes scoped history to `tui-history-v2.json` under its existing config
directory. The old `tui-history.jsonl` remains intact. On the first successful
record into the new format, its legacy entries become fallback history for that
workspace only; they are not copied into every other workspace. Remote history
also includes the server URL in its scope. Corrupt or unreadable history is not
overwritten. Desktop retains its existing per-workspace history format with an
optional conversation field, so older entries remain valid fallback history.

Reproducible focused checks, after building the CLI and Desktop:

```sh
cargo test --workspace
HAARNESS_BIN="$PWD/target/debug/haarness" python3 scripts/test-logout-workspaces.py
HAARNESS_BIN="$PWD/target/debug/haarness" python3 scripts/test-history-scopes-tmux.py
HAARNESS_BIN="$PWD/target/debug/haarness" python3 scripts/test-runtime-reliability.py
npm --prefix apps/desktop test
HAARNESS_BIN="$PWD/target/debug/haarness" npm --prefix apps/desktop run test:e2e -- reliability-adaptations.spec.ts review-scaling.spec.ts timeline-cleanup.spec.ts
```

Verification passed 1,088 Rust workspace tests, all 146 Desktop tests, Desktop
typechecking and production build, and the architecture boundary check. Focused
MCP/TUI tests were rerun after the final backend edits (19 and 346 tests).
Clippy completed for both changed crates with no diagnostics on changed lines;
existing warnings in the dependency graph remain.

Real-process checks cover four simultaneous workspace daemons, CLI logout,
pending discovery, explicit-header independence, reauthentication, tmux recall
after restart and workspace/server switching, and Anthropic/OpenAI HTTP provider
fixtures. Ten focused Electron cases cover review draft recovery and delivery,
conversation recall, attachment retries, agent lifecycle, file/agent context,
1,000-file reviews and a 1,000-message transcript. Two existing fixture issues
were corrected: review navigation now stays within the server's existing
1,000-file cap, and the context test sets the wide viewport it requires. The
viewport failure reproduced on the preserved pre-change Desktop source.

Logs, inspected screenshots, the exact tested binary digest and a patch relative
to the preserved working-tree baseline are in `target/adaptations-20260921/`.
Validation used macOS with deterministic providers; it does not certify Linux,
Windows or live third-party OAuth deployments. Existing user daemons were not
restarted, and no packaged release was produced.


## TUI model picker and themes on 2026-09-23

The model picker uses one selection for keyboard and mouse. A hover at the edge
of a scrolling list previously left the selected row highlighted while a second
paint pass highlighted the row that moved under the pointer. Picker rows now
skip that redundant hover pass. The regression test reproduces two focused rows
with the old code and one with the fix.

Discovered model catalogs group provider/family and compare generations
numerically, keeping current GPT generations ahead of older o-series models.
Cloud model IDs display as `haarness-cloud/…` and both public and legacy prefixes
work in CLI/TUI selection. Storage IDs, favorites, recents, and active selections
remain compatible. The picker omits the redundant connected label.

`haarness` is the default theme for fresh installations, with warm charcoal and
paper variants matching Desktop. Explicit saved themes remain selected.
Five embedded light variants duplicated their dark palettes; those now have real
light palettes. Low-contrast light foregrounds throughout the collection were
corrected. All 35 built-in light variants have light surfaces and at least 4.5:1
contrast for body, secondary, accent, syntax, and diff text on their checked
surfaces. These checks measure palette colors, before terminal quantization.
Existing dark palettes are unchanged. Explicit theme choices and previews now
paint the page even in ANSI-256 terminals under `auto` background mode; an explicit
`terminal` background override remains honored. Saved light choices survive restart.

Verification:

- 575 tests passed across CLI (including integration workflows), protocol,
  providers, and TUI; the TUI suite contains 352 tests.
- Debug CLI/TUI build passed without new warnings.
- Real tmux mouse hover, wheel scrolling, keyboard movement, model filtering and
  selection retained exactly one focused model row.
- Live Cloud GPT-6 Luna request succeeded from the TUI after model selection.
- All 35 light themes were selected and their painted page colors checked in tmux.
- Apple Terminal ANSI-256 mode painted a light page and restored it after restart.
- Theme preview/escape was checked, and captured dark/light frames were visually
  inspected. Tests used isolated workspace/state and did not stop user sessions.

To switch an existing installation, use `/themes haarness dark` or
`/themes haarness light`. Open `/themes` to preview without committing; Escape
restores the prior palette.
