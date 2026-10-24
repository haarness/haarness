# Chats without a project ("This computer"), October 24, 2026

The owner asked for Desktop to stop forcing a folder on first launch: people chat without one, every such
chat gets a folder of its own as Codex's projectless threads do, the folder is chosen from a chip in the
message box, and these chats live in a sidebar section called **This computer**. Codex keeps such folders
under `~/Documents/Codex/<date>/<name>` (its users ask to move them out of iCloud-synced Documents); Claude
Desktop's Code tab uses a scratch folder removed when the session ends. Haarness keeps each chat's folder
for as long as the chat exists, in Desktop's own data folder. The same round finished two follow-ups of the
UX polish (a transcript that bounced when a card left, the permission request naming its tool call) and fixed
a main-process crash dialog.

## Architecture

One folder has one server, and nothing ever stops a server on its own, so a server per chat would pile up
processes (each with every MCP server) and make every new chat wait for a cold start. Instead:

- **One chat home per computer**: `<Desktop userData>/Chat folders`, served by one daemon started with
  `--chat-home` (project configuration off, registered as its own root whatever Git ancestor it has, never the
  legacy database's migration target, kept out of every project catalog, Cloud attention and the remote host's
  list; the mark also lives in `<data>/chat-home.json`, which an older build never rewrites).
- **A folder per chat inside it**: `CreateThreadRequest.directory` (`<YYYY-MM-DD>/<name>`, strict names) →
  `Thread.directory`. The folder is the chat's working directory and boundary: tools, shell, plans, language
  servers (one per folder), terminals (`PtyCreateRequest.directory`), Undo's snapshots (on by default there),
  side questions and `@` resources. A sibling chat's folder is outside. Children, workflows, forks and its
  schedules share it; in the chat home every root chat gets one (imports, `haarness run`, ACP, the TUI). A chat
  folder's native workflows need project configuration, like inline ones. Its schedules are seen and changed
  only from it.
- **File APIs scoped to a chat** (`threadId` on search, list, read, write and actions): paths relative to the
  chat's folder, links out refused, a deleted folder answering 404 without being made again. File search now
  reads `.gitignore` outside Git and skips dependency folders there; `.git` is refused in any spelling; a write
  lands in the folder its checks approved even if a link is swapped in.
- **Desktop**: the **This computer** section (local server, + and its chats), the message box's folder chip
  (No folder, projects, Open folder…), a ready message box on first launch while the chat home opens, Files,
  terminals, `@`, Search, images and links rooted in the chat's folder, unsaved edits kept per chat, the whole
  new-chat draft following the chip, and a chat home started only when it is used or has work to watch.

## Also in this round

- The transcript stays still when a card leaves, and a held step keeps its place when a row above shrinks
  (`e2e/card-leaving.spec.ts`).
- A permission request names the tool call that asks (`callId`), so the TUI, Desktop and ACP mark that call.
- Quitting ends Desktop's streams after the quit's callback, never inside one of their reads (Node 24's
  `onStreamRead` turned that into an uncaught `RangeError` and Electron's dialog kept the app open); a narrow
  guard notes that one error instead of the dialog, every other error behaves as before.

## Left for a decision

- The chat home's location is fixed (not a setting); remote servers and other computers still need a project.
- Hooks and MCP servers of a chat-home chat run in the chat home, not in the chat's folder (documented).
- A schedule's view is now split: a chat with a folder sees only its own, others see only folderless ones.
- `desktop.spec`'s outline test fails in about half of whole-file runs on `main` too: the dictation test leaves
  every later test 5–8 s slower. Not chased here.

## Verification

- Method: a code map of the folder/server model first, then a runtime lane and a Desktop lane against one
  written interface, each reviewed by a reader who had not written it until a review found nothing to fix
  (runtime five rounds, Desktop four; the crash fix and the bounce fix one each, plus their follow-ups).
  Reviews found, among others: chats with no folder of their own working at the chat home's root, a chat
  folder replaced by a link widening a chat's boundary, schedules and native workflows crossing from one chat
  to another, the chat home's mark lost to an older build, a search that hid a chat's own files behind its
  siblings' and committed `dist/` files in Git projects, unsaved edits lost between chats, a message sent while
  This computer opened landing in the project being left, and a write race on swapped links.
- Rust, on the final merge: `cargo test --workspace` 2,410 passed, none failed (2,364 before); `cargo fmt
  --check`, `xtask architecture`, the commercial-boundary check and `sdks/generate.py --check` pass; clippy
  finds nothing on changed lines. The file-API tests also ran on Linux (arm64, in a `rust:1.94` container):
  18 unit and 3 integration tests pass, the `O_PATH` and device-and-inode paths included.
- End to end on the final binary with isolated data: `scripts/test-chat-folders.py` (17 checks) and 30 TUI tmux
  scripts pass, `test-tmux.sh` and the PTY script included. A permission probe of 128 realistic tool calls in Git
  and plain projects, with default and custom rules, decides exactly as `main` does.
- Desktop: typecheck, 569 unit tests and the build. The whole Playwright suite (473 tests): 431 passed, 35
  skipped, 3 failed, 4 not run behind them. `browser-focus` failed while the screen was locked and passes 5 of 5
  unlocked; the outline test is `main`'s known flake and passes alone; `card-leaving`'s new held-step test had a
  timing-dependent set-up, fixed in this round (6 of 6 runs pass, and it still fails without the fix it pins).
- Not verified: the packaged app, Windows (its file-API path was not compiled), live Cloud and remote scripts,
  `haarness-account.spec`.

No paid model calls: the echo provider, fixtures and isolated data throughout.
