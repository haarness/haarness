# Terminal UI internals

> **User guides:** [Terminal UI](https://haarness.ai/docs/tui/tour), its
> [slash commands](https://haarness.ai/docs/tui/slash-commands) and
> [keyboard shortcuts](https://haarness.ai/docs/tui/keyboard-shortcuts). What each command and key
> does is in the [reference](REFERENCE.md#terminal-ui); this page is how the TUI is built and tested.

The terminal UI is a client of the folder's Haarness server, like Desktop. `haarness`, `haarness tui`
and `haarness attach <url>` start or find a server, then hand the TUI an API client; everything it
shows comes from that server over REST and SSE, and quitting it never stops the server's work.

## Crates

| Crate | Role |
| --- | --- |
| `haarness-tui` | The client: state, input, views and slash commands. Depends on `haarness-protocol` (wire types), `haarness-opentui` and `haarness-account`, never on `haarness-core`, a provider or a store; `cargo run -p xtask -- architecture` enforces it. |
| `haarness-opentui` | The safe wrapper around the vendored OpenTUI native renderer (`vendor/opentui`). All of the workspace's `unsafe` code is here; see [Native boundary](ARCHITECTURE.md#native-boundary). |
| `haarness-cli` | Starts or attaches to the server and calls the TUI's `run_*` entry points; ACP also reuses `haarness_tui::api::ApiClient`. |

## How a frame happens

`crates/haarness-tui/src/`:

- `api.rs`: the strict server client (REST calls and the SSE subscription).
- `state/`: the `App` state, which mirrors the server or holds pure UI concerns; `reduce.rs` folds SSE
  events into it, keeping events of the open chat and of its child agents.
- `input/`: keys (`mod.rs`), the composer (`composer.rs`) and the mouse (`mouse.rs`). A key either
  changes state, runs a slash command or calls the API.
- `view/`: frame composition (header, transcript, sidebar, menus, approval cards, composer), drawn into
  OpenTUI buffers. Hit regions recorded while drawing route mouse clicks.
- `commands.rs`: the slash-command catalog and dispatch. `management.rs` renders the server-authored
  forms (`POST /v1/manage/{resource}`) that Desktop's settings also use, so `/providers`, `/mcp`,
  `/hooks`, `/schedules`, `/marketplace` and others share their logic with Desktop.
- Feature modules: `connect.rs` (`/connect`), `chrome.rs` (`/chrome`), `pty_view.rs` (`/terminal`
  over server-owned PTYs), `diffview.rs` (`/diff`), `messages.rs` (`/messages`), `side_chat.rs`
  (`/btw`), `input_queue.rs` (steer and queue), `statusline.rs`, `theme.rs`, `keybindings.rs`.
- `locale.rs`: the 17 interface catalogs in `locales/`, shared with Desktop. Visible text is looked up
  by its English text, so every new string needs a key in each catalog (see
  [Supported languages](../locales/README.md)).
- `terminal.rs`: raw mode and the alternate screen, restored however the process ends.

## Files the TUI keeps

In the global configuration folder (`~/.config/haarness/` unless `HAARNESS_HOME` or
`XDG_CONFIG_HOME` says otherwise): `tui-state.toml` (thinking display, sidebar, theme, saved
`/keybinds`), `tui-history.jsonl` (prompt history), `tui-stash.json` (stashed drafts),
`tui-servers.json` and `tui-server-drafts.json` (saved servers), `tui-resource-picks.json` (MCP
resources picked with `@`) and `themes/`. `[tui]` settings, including `[tui.keybinds]` and
`[tui.statusline]`, are read from `config.toml` there; a binding saved in `/keybinds` wins.

## Testing

- `cargo test -p haarness-tui`: headless tests that reduce events, press keys and read the rendered
  lines, without a terminal; where a test needs the API it answers from a small local HTTP stub, not
  a Haarness server. They keep their files in a temporary folder of their own.
- Real terminals: `scripts/test-tmux.sh` and the `scripts/test-*-tmux.*` scripts start an isolated
  server and drive the TUI in tmux, reading the screen. `scripts/e2e_tmux.py` makes a tmux pane run
  with the script's environment instead of the tmux server's. `scripts/test-tui-cursor.py` runs in CI.
- Useful environment: `HAARNESS_DATA_DIR` or `HAARNESS_HOME` for an isolated install,
  `HAARNESS_TEST_PROVIDER=1` for the deterministic `echo` provider, `HAARNESS_SCHEDULE_SKIP_INSTALL=1`
  so no schedule reaches the operating system, and `HAARNESS_E2E_KEEP=1` to keep a passing Python
  script's temporary folder.

See [CONTRIBUTING.md](../CONTRIBUTING.md#checks) for what a TUI change must pass.
