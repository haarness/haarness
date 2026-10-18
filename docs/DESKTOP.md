# Haarness Desktop

> **User guides:** [Install and first run](https://haarness.ai/docs/desktop/install), [Work in a conversation](https://haarness.ai/docs/desktop/the-conversation), [Settings](https://haarness.ai/docs/desktop/settings). This page has the engineering detail.

Haarness Desktop is an Electron application in `apps/desktop`. Its SolidJS interface uses Haarness's directory-scoped server for every conversation, turn, tool, approval, workflow, and terminal. The server remains the durable owner of work and can be shared with the CLI and TUI.

Normal Send steers active work; **Queue** or **Alt+Enter** saves a follow-up until that work finishes. Queued messages wait in a tray above the composer. Each can be edited in the composer, sent now, reordered or removed, and each runs as its own turn. Stop answers steering the agent has not read yet and pauses the queue. See [Steer and Queue](INPUT-QUEUE.md).

## First run

1. **Open a project.** A new install opens on **Open a project to get started**. Choose **Open
   project** and pick a folder. Every chat belongs to a project folder or one of its Git
   worktrees, so the message box waits until a project is open. Project configuration, MCP
   servers, hooks and extensions load automatically; launch with
   `HAARNESS_DISABLE_PROJECT_CONFIG=1` to use only global configuration.
2. **Connect a model.** When the project's server has no model it can use, the **Welcome to
   Haarness** dialog opens. On this computer it starts with **Use Haarness Cloud** (Cloud models
   through your Haarness account, with no API keys), then **Popular** providers, **More
   providers**, and under **Something else** a **Custom endpoint** or a self-hosted **Haarness
   Router**. Type to filter the list; connected providers show a check. If Ollama or LM Studio is
   already running with a model, Desktop picks that model and skips the dialog: a line above the
   first chat names the local model and offers **Connect a provider for larger models**.
3. **Sign in or paste a key.** A provider with more than one way in asks **How do you want to
   connect?**
   - **Use Haarness Cloud** opens your browser to sign in to your Haarness account and closes
     the dialog when you come back.
   - **Sign in with ChatGPT** opens the sign-in page in your browser; there is no code to type.
     **Sign in with GitHub** (Copilot) and **Sign in with xAI** also show a code to enter, with
     **Copy code**. **Sign in with a code**, or **Use a code instead** while a browser sign-in
     waits, is for SSH sessions and remote servers; a remote server offers only the code sign-in. The
     dialog shows **Waiting for you to finish in the browser…** and closes itself when sign-in
     completes.
   - **Use an API key** shows a masked field, a **Create a key** link and, under **Other ways to
     connect**, the environment variable that works instead. **Connect** checks the key with the
     provider first: a key the provider refuses is not saved, and the dialog shows the provider's
     reason. Keys are stored by the Haarness server in its owner-only `auth.json`, never in the
     project.
4. **Send your first message.** Desktop confirms the connection (for example **OpenAI is
   connected**) and the composer picks one of that provider's models. Type in the message box
   and press Enter (**Settings → General → Send message with** can make it Cmd/Ctrl+Enter). The
   chat is named from your first message; to change the name, choose **Rename** in the **Chat
   actions** menu above the chat, or search for **Rename chat**.
5. **Answer approvals, or switch to Auto.** The shield pill at the left of the composer reads
   **Review** (the default) or **Auto**, and applies to the open chat on its server, so the TUI
   and other windows see the same mode. In Review, edits and commands inside the project run
   without asking; Haarness asks before using files outside the project, reading secrets such as
   `.env`, switching agents or generating paid media. The browser asks about a site only when
   **Settings → Browser** sets it to **Requires approval**. A request card offers **Allow once**,
   **Allow once + Auto**, **Always for this folder** (in a Git repository, **Always allow**
   offers the folder or the whole repository) and **Deny**. Auto answers those requests once by
   itself for the chat and its agents; Cloud actions that spend money or delete something still
   ask, and so does a site set to **Requires approval** unless **Remember site approvals** is on.
   Auto never answers for a website in your own Chrome: its card, **Allow in Chrome**, offers
   **Deny**, **Allow once** and **Allow in this chat**. See [Automatic approvals](AUTO-APPROVALS.md)
   and [Chrome approvals](PERMISSIONS-AND-EXECUTION.md#chrome-approvals).

**Cmd+.** (Ctrl+. on Windows and Linux) switches the message box to the next agent and
**Cmd+Shift+.** to the previous one; change them in **Settings → Keyboard shortcuts**.

To connect another provider later, use **Settings → Providers**: **Connect a provider** opens the
same dialog, and each provider row has **Connect** or **Reconnect** and **Disconnect**, next to
**Refresh models**, custom endpoints and the Router. While no model is available, the composer's
model pill reads **Connect a provider** and opens the dialog. The e2e suite drives the key path and
the code sign-in path against a fixture issuer (`HAARNESS_OPENAI_OAUTH_ISSUER`).

## Development

See [Keeping local builds small](BUILD-STORAGE.md) for Rust build defaults and safe cache cleanup.

Install Node 22+, npm, Rust and Bun. From this repository:

```sh
npm run desktop:install
npm run desktop:dev
```

`HAARNESS_BIN` can select an existing Haarness binary. `HAARNESS_DESKTOP_DATA` overrides desktop preferences and encrypted remote credentials; `HAARNESS_DATA_DIR` independently overrides runtime storage. Normal builds use the system application-data directory and normal Haarness storage.

## Package and launch

```sh
npm run desktop:package
```

Packaging builds the Rust executable, SDK, Haarness for Chrome extension, renderer and Electron main/preload, then bundles the Rust executable and Bun 1.3.14 into the application, with the built extension in `browser-extension/chrome` beside `bin/` in its resources. Extensions and workflows therefore work without a separate Bun installation. The Rust executable also embeds ripgrep 15.2.0, available as `rg` in agent shell calls and integrated terminals without a separate install or download. Installers appear in `apps/desktop/release`. `npm --prefix apps/desktop run package:dir` produces an unpacked application for local testing. A packaged Desktop runs `haarness browser setup --quiet --if-missing --json` once in the background at launch, so Chrome can start Haarness and the extension copy in the data directory follows Desktop updates; it skips this when `HAARNESS_DESKTOP_TEST`, `HAARNESS_DATA_DIR`, `HAARNESS_HOME` or `HAARNESS_DESKTOP_DATA` is set, and the command does nothing after `haarness browser remove` until setup runs again. See [Haarness for Chrome](CHROME-EXTENSION.md#packaging). Build on the target platform for macOS DMG/ZIP, Windows NSIS, and Linux AppImage/DEB.

Commands the agent or the terminal runs are children of Haarness, so macOS asks on Haarness's behalf the first time one of them needs your calendars, your reminders, a camera, or to control another app with AppleScript (`icalBuddy`, an EventKit script, `imagesnap`, `osascript`). The app declares those usage descriptions and, for the hardened runtime, the camera, calendars and Apple Events entitlements; without them macOS refuses such a request without asking. Allow it once and later commands keep the permission; change it in **System Settings → Privacy & Security** (Calendars, Reminders, Camera or Automation). When the server was started from a terminal app (the TUI or CLI), macOS asks that terminal app instead, and commands on a remote host use that host's permissions.

Signing and notarization use standard electron-builder credentials supplied by release CI. Local packaging uses an ad hoc signature. For distribution, explicitly override `mac.identity` with the release signing identity and supply notarization credentials. An optional HTTPS `HAARNESS_UPDATE_URL` points to an electron-updater generic feed. Do not point production clients at unsigned or development artifacts.

## Session transport

Only the Electron main process holds server bearer tokens. A narrow, validated IPC bridge exposes domain operations to a sandboxed renderer with context isolation and a restrictive content-security policy. External links open through a protocol allowlist; a mail link keeps only its to, cc, bcc, subject and body fields, so it cannot attach a local file. A link in a reply or a previewed Markdown file that names a file inside the project (a relative path, which in a Markdown file starts from that file's folder, an absolute path, or a local `file:` URL) opens in the Files panel, at its line for `path:12` or `#L12`; HTML and SVG files render only in the sandboxed preview, and a folder opens in the tree. No link reaches the system's file opener, so a link cannot launch an app or run a script. Paths outside the project, network locations (`//host`, `\\host`, `file://host`, `smb:`) and other schemes do not open and say why, and Markdown never loads an image from a network path. `haarness://open?workspace=<folder>&session=<chat>` links open a folder already in the sidebar as it was saved; any other local folder is confirmed by name first and opens with its project configuration off (its MCP servers, hooks and extensions do not run until it is opened with **Open project**). A linked chat opens only within that project, and a link naming a network or relative folder opens nothing. Local provider credentials use the existing Haarness credential store; remote connection credentials use the operating-system keyring through Electron safeStorage.

On connection or resynchronization, the desktop captures `/v1/info`'s event cursor, fetches REST snapshots, and subscribes to `/v1/events` with `Last-Event-ID`. The SDK validates ordering and server identity. Snapshot races reconcile with events received during the fetch, including the history the open chat reads again after a revert or a compaction: a compaction happens mid-turn, and items the turn writes while that read is in flight are kept. Durable items are merged by ID and sequence; live text/reasoning is replaced by its persisted item. A slash command's message is rewritten to what the command expanded to once it runs (`item.updated`), and the open chat shows that text, as it does after a reopen. Same-instance reconnects resume from the last consumed event and preserve partial streaming text; server changes and replay gaps trigger fresh snapshots. Message request IDs are retained across uncertain failures and reloads so a retry cannot submit the same message twice. Reconnect uses bounded backoff and refreshes snapshots, including after a daemon restart. Observing or closing a window does not cancel a turn.

## Conversation controls

Open a conversation's actions menu or search the command palette for **Undo last turn**, **Redo turn**, **Compact chat**, or **Find in chat**. The composer also accepts `/undo`, `/redo`, and `/compact` as local commands.

Slash commands that answer with text, such as `/workflow` or `/goal status`, show the answer in a tray above the composer; `/workflow run` names the run, keeps its state current until it ends, and shows the ID that `/workflow cancel`, `pause` and `resume` take. The answer stays with its chat until you dismiss it or send the next message. A goal that pauses, blocks or finishes on its own shows its message there too.

Undo restores the previous conversation boundary and the files changed by that turn, and puts the undone prompt back in the composer. An existing draft is preserved. Redo restores the next undone turn; an untouched restored prompt is removed from the draft. Undo state and drafts survive reload. Sending a new message after undo replaces the undone branch. If files changed outside the conversation conflict with an undo, Haarness reports the conflict without overwriting those edits.

Manual compaction asks the conversation's model to summarize its history. History actions are unavailable during an active turn or another history operation. Redo or continue an undone conversation before compacting. Provider errors leave the draft available to retry.

Press **Cmd+F** on macOS or **Ctrl+F** elsewhere while in a conversation to search messages and tool output, including older messages outside the currently rendered viewport. Search is literal and case insensitive; the counter counts matching messages. Enter and Shift+Enter move between matches, and Escape closes search. File editor search keeps its own shortcut when the editor has focus.

## Hooks and memory

Open **Settings → Hooks** to add, edit, enable, disable, or remove command hooks. Choose **This project** or **All projects** when adding a hook, then select an event and enter the command. For example, a `Stop` hook can run a script after a response finishes. Tool events accept a matcher such as `Bash|Edit`; `*` matches every tool. Advanced fields include arguments, a shell, timeout, and asynchronous execution.

The editor shows hooks from the same configuration sources the runtime loads, including compatible configuration files. Plugin hooks are read-only. Project hooks remain inactive when project configuration is not trusted; change that separately under **Settings → Projects**. Supported events are `PreToolUse`, `PostToolUse`, `PostToolUseFailure`, `UserPromptSubmit`, `PermissionRequest`, `Stop`, `PostCompact`, `StopFailure`, `SessionStart`, `SessionEnd`, `TurnEnd`, `Notification`, `FileChanged`, and `ConfigChange`.

Enter **`/memory`** in the composer, or search for **Manage memory**, to manage the active project's memories. Add or edit a named memory with its description, type, and content, or delete an obsolete memory. The dialog shows the storage directory outside the repository. **Index limit** controls how many summaries enter the prompt (1–1000, default 200). Turning memory off removes its prompt index and disables memory tools without deleting saved memories.

These editors use the existing local host management service. For remote projects, manage configuration on the remote host. Runtime changes apply when idle. If a file changes outside an editor, saving reports a conflict and retains your draft so you can reopen the current file before applying your changes.

## Project tools

Projects keep their position when selected. Drag anywhere on a project or conversation row to reorder it. There are no dedicated grips, move menus, or reorder notifications. Alt+arrow keys and Alt+Home/End provide keyboard ordering while retaining focus. Conversation, side-panel, and terminal tabs use the same controls. Ordering is remembered per project and collection and survives activity, renaming, filtering, and relaunching. Newly opened projects are appended; new conversations appear at the top without disturbing existing saved positions.

Saved projects remain in navigation while their local runtimes start or recover, with cached conversation titles. The app restores the last project and task on launch, with that project's open chat tabs: each project, and each of its worktrees, remembers its own tabs, so a window reload, a relaunch or a return to the project reopens them (a chat archived meanwhile stays closed). New chat opens a per-project draft and creates a conversation on send; its composer includes a project picker. The first message goes out without a loading screen: the new conversation has no history to fetch, so the composer stays in view until the message appears. Switching to another chat shows the loading mark only when its history takes longer than about a third of a second, fading it in (without the fade under reduced motion). A waiting permission or question belongs to its own task and is indicated in the conversation list. See the [project and task model](DESKTOP-PROJECT-MODEL.md).

Projects expand independently. Use the chevron to show or hide a project’s conversations without switching the active chat; click its name to toggle its conversations and activate it, or select any visible conversation to open that exact chat. Several projects can remain expanded, with independent ordering. Expansion choices, including an explicitly collapsed active project, survive relaunches. Conversation titles and activity indicators update live in expanded inactive projects. Each conversation row ends with how long ago the chat was last active, in the interface language ("now", "5m", "3h", "2d", "3w", then the date), unless it waits for an approval or an answer, which the row says instead; the age is not part of the row's accessible name.

**Ctrl+Tab** and **Ctrl+Shift+Tab** (Control on macOS too) step to the next and previous tab of the panel that has the keyboard, wrapping at either end: the chat tabs from the conversation or the message box, the terminal tabs from a terminal (the shell never receives the key, and the keyboard moves into the terminal shown), the open files from the Files panel, the pages from the browser panel's tab strip or address bar, and the side panel's own tabs elsewhere in the side panel. A panel with one tab takes the key and changes nothing; in the sidebar, or in a page the browser shows, the key is left alone. Change both keys in **Settings → Keyboard shortcuts**, where Control chords can be recorded on macOS. If you already used Ctrl+Tab or Ctrl+Shift+Tab for another action (off macOS, a `Mod+Tab` agent key is the same keys), that action keeps it and the tab step is left unset until you choose a key for it; an unset key does nothing. Terminal, file and side-panel tabs tell assistive technology which one is shown.

**Alt+↑** and **Alt+↓** (Option on macOS) open the previous and next chat among the rows the sidebar draws, top to bottom and wrapping; rows inside a collapsed project, checkout or schedule are passed over. They work from the conversation and the message box (a draft is kept, and the keyboard stays in the box), but not in the file editor, which moves lines with them, a terminal or a dialog. On a sidebar row that has the keyboard, Alt and the arrows still move that row. **Focus sidebar** (Control+Shift+L on macOS; Alt+Shift+L elsewhere, where Control+Shift+L is the editor's **Add selection to context**) shows the sidebar if it is hidden, opens the open chat's project and checkout, and puts the keyboard on its row. In the open chat, **Command/Ctrl+↑** and **Command/Ctrl+↓** move to the previous and next prompt, **Command/Ctrl+Shift+↑** to the first prompt and **Command/Ctrl+Shift+↓** back to the newest output, which it then follows; in the message box they act only while it is empty, so they still move the caret in a draft. From the newest output, the previous prompt is the last one. All seven are in Universal search and can be changed or unset in **Settings → Keyboard shortcuts**; a new default that is already another action's key is left unset.

**Show and hide Haarness from any app** (Settings → Keyboard shortcuts) registers a system-wide shortcut, Option+Space (Alt+Space elsewhere) unless you record another: from another app it brings Haarness's last window to the front, and with Haarness in front it hides Haarness (minimizes the window outside macOS). It is off until you turn it on, because while it is on no other app receives that combination; turning it off releases it and keeps your choice for next time. The combination needs Command, Control or Option and one more key. One the system keeps (such as Command+Space for Spotlight) or one another app already registered is refused with a notice, and the shortcut saved before stays in use. The current shortcut pauses while you record a new one. Linux sessions under Wayland may not allow system-wide shortcuts.

The side-panel button opens a launcher when no panel is selected. Its Browser, Files, Review, Terminal and Context entries include descriptions and the configured shortcuts. The plus button in the side-panel tab bar returns to this launcher; closing the last tab also returns there. Files and review stay alongside the conversation, and terminals open below it.

Expanding the terminal starts the first shell automatically and focuses its input. Existing terminals are reused when reopening the panel. Hiding the panel keeps its processes running; closing a terminal tab stops that terminal immediately without a confirmation dialog. Closing the last terminal hides the panel, and reopening it starts a fresh shell. The shell always has the terminal's current size, including after a change of terminal text size, and a size the server failed to take is sent again when the terminal reconnects. Showing a terminal again (reopening the panel or choosing its tab) replays its earlier output; xterm's answers to terminal queries in output it already showed (device attributes, cursor position, colours) stay out of the shell, so a replayed `vim` or `fish` start types nothing at the prompt. Output it has not shown yet is answered: a new terminal's shell that asks as it starts, before the panel connects, gets its answer, and so does a program whose question arrived while the connection was down. The first time a window shows a terminal it did not start (after a reload or a relaunch), all of its earlier output counts as shown. If the terminal's connection ends (the server drops one that falls behind a burst of output, or the network or server goes away), the terminal connects again and replays only the output it missed, while the shell is still running; keys typed while it is down (up to 64 KiB, the latest kept) are sent once it is back. Stepping quickly through terminal tabs never leaves the one shown without its connection. A paste longer than 32 KiB reaches the shell in pieces, in order. When Haarness itself has no locale (Desktop opened from Finder sets no `LANG`, `LC_ALL` or `LC_CTYPE`), a terminal, and each shell command the agent runs, gets `LANG` from the system's region setting, or `en_US.UTF-8` when that has no locale of its own (`C.UTF-8` on Linux), so text such as 日本 stays intact. A locale you set in any of the three is kept, and so is a `LANG` from a `shell.env` hook.

The server keeps the last two megabytes of a terminal's output. A terminal shown again after more output than that starts over with a notice, from the first whole line still kept (never inside an escape sequence or a character), in the terminal modes the dropped output left on: a full-screen program such as vim, htop or tmux keeps its alternate screen, cursor and keypad keys, mouse reporting and bracketed paste.

If a terminal's connection stays down for more than a second, the terminal shows **Reconnecting…**; after 15 seconds it says that Haarness can't reach it and that its shell may still be running, with **Retry** (try at once, backing off from the start again) and **New terminal**. It keeps reconnecting in the background meanwhile, and never replaces a terminal while the server cannot say what became of it. When the server answers without the terminal (it was restarted or replaced while the terminal was cut off), the panel shows the server's terminals instead, and a new shell when it has none.

A terminal shown again (the panel reopened, its tab chosen again) opens at the line it was scrolled up to, unless older output was dropped meanwhile.

On macOS, **Option as Meta key** (Settings → General) makes Option send Meta in the terminal (Esc and the key, which shells and editors read as Alt+B, Alt+F or Alt+.) instead of the character the keyboard layout composes. **Left Option** keeps the right one for characters such as @ and [ on German and other layouts; **Both Option keys** uses either. It is off by default.

Opening a project goes directly to the native folder picker. Project configuration, MCP servers, hooks, and extensions are enabled by default. Explicit per-folder settings are remembered when reopening recent projects and can be changed in Settings → Projects. When the requested setting differs from an existing local daemon, the desktop applies it by restarting the idle daemon; running work must finish and its terminals close first, and session history is retained.

The desktop keeps the conversation in the center, projects and sessions in a collapsible sidebar, and files or diffs in a tabbed panel on the right. The sidebar contains New chat, Projects, and Settings. Chat has no secondary toolbar for compacting, undoing, or exporting; its title stays in the window header and controls stay with the composer. Schedules and plugins are managed in **Settings → Schedules** and **Settings → Plugins**. Workflows and goals run from the message box with `/workflow` and `/goal`; their answers appear in a tray above it (see [Conversation controls](#conversation-controls)). **Search** reaches every command, setting and chat. The terminal opens below the workspace. Drag the dividers to resize the sidebar, side panel or terminal; focused dividers also support arrow keys, Shift for larger steps, and Home or double-click to reset. Panel sizes and sidebar visibility are saved between launches. The side panel can expand to fill the workspace; smaller windows show one pane at a time.

Sidebar conversation rows expose one Archive/Restore action. Renaming and forking remain in the conversation header. Archiving in another expanded project leaves the current conversation open. An archived chat opened from Search (or archived while open) shows a quiet line at the top of the conversation, **This chat is archived. Sending a message restores it.**, with a **Restore** button that returns it to the sidebar. Opening it alone leaves it archived; sending a message restores it first, as resuming an archived session does in the TUI. A chat archived while its tab waits behind another (from its sidebar row, **Archive chats**, or another window) closes that tab along with its sidebar row; restoring it later does not reopen the tab. An agent's own chat, which the server archives when the agent's work ends, keeps its tab.

Forked conversations show a “Continued from [source title]” divider between the copied history and new messages. Activate it to open the source conversation at the original fork point, including archived sources. The boundary survives reloads and renames. Older forks without a saved boundary show the source link at the beginning; a portable fork whose source is unavailable retains the source title with an inactive link.

While a task runs, the composer border pulses gently; approvals and questions use a steady outline while awaiting input. A question without choices opens with its answer field focused, unless you are typing elsewhere (a draft in the message box, a terminal or another field); Enter in the answer field replies, as **Reply** does. Reduced-motion preferences disable the pulse. The send-button position becomes Stop when the active task has no draft to send; entering guidance restores Send. There is no separate Working/Stop row or send-key footer hint.

Model, agent, effort, review, settings, and form pickers share a themed popover component instead of native HTML selects. Long lists have search, options stay inside the window, arrow keys and type-ahead navigate, Enter/Space select, and Escape dismisses only the picker. Form values are submitted through hidden inputs. Pickers opened inside dialogs stay within that dialog's top layer.

New chats resolve and display a concrete model before the first message. Selection follows the current explicit choice, the chosen agent's pinned model, the project's configured provider/model, valid remembered choices and conversation history, then a connected provider's preferred model (favoring tool support). Child-agent history does not change the default for new conversations. Manual choices are remembered across reloads; existing conversations keep their selected model even if it is no longer in the catalog. With no connected model, the composer offers **Connect a provider**. Reasoning-effort choices follow the resolved model immediately, and the exact displayed provider/model is sent when creating the conversation. The composer's model pill shows the full model name (up to 320px) beside its provider icon, effort label and chevron; in a narrow composer the name ellipsizes before the toolbar would wrap the send button onto its own line, and in Arabic it still truncates at its end.

Settings → General separates appearance from palette. Choose Light, Dark, or System (follow the operating system; the default on a new install), then Haarness, Graphite, Ocean, Forest, Violet, Rose, Sand, or High contrast. Every palette includes light and dark variants. Changes apply immediately to the conversation, file editor, syntax colors, diffs, and existing terminals, and persist across app launches. In the terminal, ANSI black stays dark and white light in both appearances (black is the text colour in light), so text a program writes in black stays readable.


Search is a universal launcher, available from the sidebar or its configurable keyboard shortcut (Command/Control K by default). It finds application and contextual commands, settings sections and individual controls, keyboard shortcuts, saved projects and worktrees, active and archived conversations, and files in the current workspace. Settings results open their section and focus the matching control. File results open the editor; conversation results retain their project/worktree identity. Search temporarily refreshes conversation indexes for collapsed projects without expanding their sidebar groups. Missing worktrees remain hidden.

Results show their destination and configured shortcut, match title words and aliases, and support compact fuzzy matches. Chats appear first, followed by matching archived chats and then commands, settings and other results. Matching chats are not capped at eight; the empty-query view shows the eight most recent chats first. Arrow keys navigate, Enter opens the selected result, and Escape closes Search and restores focus. A slash-command result inserts into the draft for review before sending. Modules register their contextual actions with the shared index, so editor, review, terminal, and conversation commands can be searched alongside global actions.

The explorer offers nested directories, filename search, hidden files, editor tabs, syntax highlighting, search, save, rename, creation, deletion, and media previews. Saves carry a SHA-256 revision and reject external modification. The tree and open files follow changes on disk after the agent's file tools, at the end of each turn, after saves from another window and when the window regains focus; expanded folders stay open. An open file without unsaved edits shows the new text. One with unsaved edits keeps them under a bar that offers **Reload** (show the file on disk) or **Keep mine** (the next save replaces it), and a save that meets a newer file on disk offers the same choice. The server constrains file access to its workspace, rejects Git metadata and escaping symlinks, and bounds individual files to 10 MiB. A read opens the real file the check approved, so a link swapped afterwards for one that leads out of the project is not followed. Clicking a file in the tree that links outside the project says "This file is a link to somewhere outside the project, so it does not open here." instead of showing the server's error, because opening it would show any file on the computer to anyone connected to the server. Unsaved file edits survive panel navigation and require a discard decision before changing workspaces or closing the window.

Diff review offers working-tree, branch and last-turn changes, split/unified presentation, word differences, context selection, review markers and line comments added to the session draft. Clicking a line's plus button or double-clicking its code opens an input directly below that line. Split diffs keep the input on the selected original or updated side. Enter adds the comment to the message draft, Shift+Enter inserts a new line, and Escape or Cancel closes the input. Unsubmitted text survives diff layout changes and refreshes within the review. Terminals use the server's PTY WebSocket API, including replay on reattachment; hiding a terminal preserves its process.

Settings cover appearance, keyboard shortcuts, connections, providers, models, agents, commands, skills, tools, MCP, plugins, extensions, schedules and profiles. Raw JSON configuration editors and their sidebar entries are omitted; existing engine settings remain supported in configuration files. See [configuration scopes](CONFIGURATION-SCOPES.md) for shared global/project storage, migration and precedence. Native management is available for local workspaces. Remote catalogs remain available, while remote host configuration is managed on that host.

MCP setup uses a transport-specific form. Local servers have an executable, ordered argument/optional-value rows, and environment-variable key/value rows. Remote HTTP servers have a URL, request-header key/value rows, and automatic, enabled, or disabled OAuth with optional client ID and scopes. Switching transport keeps each draft; saving writes only the selected transport. Values with spaces remain single arguments, and environment/header values are masked until revealed. Servers can be available to all projects or just the current project, and saved disabled for later use. Duplicate keys and invalid entries are rejected before saving; server-side errors preserve the draft. Closing with Escape or a backdrop click leaves Settings open.


Schedules use frequency, day/time, agent, model and reasoning-effort pickers. Common frequencies are daily, weekdays, weekly, an interval, or once; custom cron is an explicit advanced choice. The model is resolved from the connected catalog before saving. Settings show definitions for the current checkout, with edit, enable/disable, run-now and remove actions; execution logs and raw command output stay out of this panel. macOS and Windows calendar triggers follow the computer timezone; Linux calendar schedules can select a timezone.

Scheduled conversations live below ordinary chats in a collapsible **Scheduled** section, grouped by schedule within their original checkout and ordered newest first. Opening a run expands its group, and each group initially shows ten conversations with a show-more control. Renaming a conversation or schedule does not split new runs into new groups. Removing a schedule leaves its conversations available; archiving a run removes just that conversation from the sidebar. Independent forks remain ordinary chats. Older runs with the historical `Schedule: name @ timestamp` title remain discoverable.


Slash suggestions open above the composer without moving the conversation. Long names wrap within the available width, descriptions truncate, arrow keys move the selection, Enter or Tab completes it, and Escape dismisses it.

New chats have the same searchable project picker in the welcome heading and composer. It lists saved projects in the user's order, marks the selection, and opens the native folder chooser from New project. Every conversation belongs to a project folder or worktree; switching projects preserves each draft.

## The conversation

The transcript has three shapes and no more. What you said is a bubble on the right, so it needs no author label. What Haarness answered is plain prose in the reading column, with no repeated author heading. What the engine _did_ hangs off a single hairline rail beneath the answer. One reading measure — about ninety-five characters at the default conversation size — governs prose, steps, approvals and the composer, so the text column never moves when a side panel opens.

Each step on the rail is a verb and a target: “Ran `npm test`”, “Read `src/server.ts`”, “Edited `src/app.tsx`”. Haarness's own tools are named exactly; an MCP or extension tool falls back to its family, and an unrecognised tool is labelled with its own name rather than a guessed verb. Consecutive steps share one unbroken rail, so a burst of file reads reads as one column of work. Successful steps state “Done” only on hover or when open; running steps keep their status visible. Tools that return an error use the same neutral icon, rail and output colors, with no “Failed” badge. Their returned message remains available when expanded, including inside batches. Opening a command shows one Shell panel with the literal command and readable output. Other tools use plain labelled inputs and results; edits keep their compact diffs. JSON argument and batch-result envelopes do not render. Actual source files and shell output retain their original contents, including JSON when that is the content being read or printed.

Batches use an action summary such as “Read files, ran commands” and expand into individual tool rows. Each nested row opens independently, keeps its expansion when results arrive, and shows its own output or failure. Dragging across a row's command or path selects it for copying without opening or closing the row; a click, Enter or Space still toggles it. A row shows its command on one line; hovering it shows the whole command as written, line breaks included. Batch results are paired in input order, including repeated tool names; transcript results are scoped to their thread, turn, call, and undo boundary. A failed child marks the batch as failed without hiding successful results. Full files and changes remain available in the Review panel.

Assistant replies carry a compact footer: a quiet timestamp beside Copy and Fork icons that appear on hover or keyboard focus. The footer always reserves its height, so revealing the icons never reflows the transcript. Copy preserves Markdown; Fork includes the selected reply while excluding later history. Your own messages expose the same actions and reveal their timestamp with them, so re-reading a thread stays clean.

Once a conversation has two prompts, a short stack of dashes at the top of the transcript's leading edge outlines it, one dash per prompt you wrote. The prompt you are reading has the longer, brighter dash. Hover the stack to open a panel listing every prompt by its first line, then click one to jump to it. Past 24 prompts, each dash stands for an even slice of the conversation, and the panel shows at most 16 rows and scrolls through the rest. Transcript text always keeps clear of the stack. In a conversation too narrow for it, or a transcript too short to open its panel, the stack is hidden, and a panel open when the transcript shrinks closes until the pointer comes back. In Arabic it sits on the right edge. It works from layout the transcript has already measured, so streaming and scrolling stay as fast as before.

The current task checklist lives in a compact Plan tray above the composer. It updates in place, shows completed and cancelled counts, and marks each step with a round glyph: a dashed ring while it waits, a ring in the theme's primary colour that turns while the agent works on it (still once the turn stops, and under reduced motion), a filled check once it is done, when its text steps back to the muted colour, and a dash with struck-through text when it was cancelled. Click its header, or use Enter or Space, to collapse it to the current step and its glyph. The X dismisses the entire tray for that checklist, and the choice survives status updates, chat switches, and desktop restarts. A different checklist, a checklist created after clearing the list, or restarting a completed/cancelled checklist opens a new tray. Reordering existing steps preserves dismissal. An incomplete plan opens while work is active and collapses to a summary when idle, unless you explicitly expand it. Once every step is completed or cancelled, the tray disappears automatically, even if it was expanded or the turn is still running. Long lists scroll inside the tray. Switching chats and restoring history select that conversation's latest plan, including its undo boundary. Neither the checklist snapshots nor the tool calls that write them render in the transcript.

Fenced code is framed, labelled with its language, and offers Copy inside the block. That control is attached only once a message has finished streaming, so a block that is still growing never moves under the pointer. During a turn the transcript shows one activity line derived from the tool that is actually running. Its text shimmers without a spinner, using a shared animation clock synchronized before painting so refreshes, navigation and virtual-row remounts do not restart the sweep. The gradient travels exactly one fixed-width tile per loop, keeping the repeat seamless and its position independent of label width. Reduced-motion preferences disable the shimmer. An error is reported where it happened and offers the action that resolves it — a missing credential links straight to Settings → Providers.

Icons come from Lucide through one shared component in `renderer/icons.tsx`. Call sites name a role (`diff`, `branch`, `stop`) rather than a drawing, sizes are a five-step scale rather than arbitrary pixels, and the stroke is specified in rendered pixels so a 12px glyph and a 22px glyph have the same optical weight. Icon-only controls always go through `Button`, which owns the hit area, the accessible name and the tooltip. Haarness's brand artwork — the **Haarness.** wordmark and the H mark — is deliberately outside that catalogue and lives in `renderer/brand.tsx`, drawn from [`brand/`](../brand/README.md). Both inherit `currentColor` and scale off the font-size of whatever they sit in. The mark is two posts joined by a strap that overhangs them; the wordmark's H is redrawn at the interface face's own cap height, stem weight and side bearings so it stands in for the word's first letter rather than sitting beside a letter it would repeat.

On macOS, the composer microphone button (or Command+Shift+M) starts dictation using Apple Speech and AVFoundation. macOS 26 and later use SpeechAnalyzer with SpeechTranscriber, falling back to DictationTranscriber for other supported languages. These engines run on-device without requiring Siri or keyboard dictation to be enabled. The first use may download the selected language's speech model and asks for microphone access; the composer shows model preparation until capture starts. Older macOS versions use SFSpeechRecognizer and require speech recognition access and enabled system dictation; recognition stays on-device where supported, otherwise macOS may use Apple's speech service. Transcription replaces the selected text or inserts at the cursor, revises partial results, and never sends automatically. Typing, navigating to another conversation, opening settings, reloading or closing the window stops capture. Audio is not written to an app recording file or sent to the Haarness model runtime. See Apple's [SpeechAnalyzer overview](https://developer.apple.com/videos/play/wwdc2025/277/). Windows/Linux dictation is not implemented in this revision.

### MCP App pages

An MCP server may pair a tool with an interactive page ([MCP Apps](https://modelcontextprotocol.io/extensions/apps)). When the agent calls such a tool, the page appears under the tool's row once the call finishes, in the current theme, palette and language, and grows with its content up to 640 px. There is nothing to turn on; servers learn that Haarness renders pages when it connects to them (see [MCP results](REFERENCE.md)). A failed call shows its error and no page.

What a page can do:

- **Show the tool's input and result** in its own layout. A result too large to keep for the page (over 512 KB without its media) shows a notice instead.
- **Open a link** (http, https or mailto only) through the same check as any other link. Right after a click or key press inside the page it opens at once; otherwise the page's request waits under it with **Open** and **Dismiss**.
- **Write a message for you**: the text goes into the message box after anything already there, and is never sent on its own.
- **Read its own `ui://` resources** from its own server.

What a page cannot do: call tools (it is told to ask in the chat instead), change what the model reads, download files, go full screen, or open nested frames. Each page runs in a sandboxed frame that allows scripts and forms only, with an opaque origin, so it cannot reach the window, the project, the server or other pages. A separate host page (`mcp-app.html`) holds the frame and refuses any navigation of it; the page's own Content Security Policy allows only the network and resource hosts its server declares in `_meta.ui.csp` (host sources only, no network when none are declared, no form submissions), and only clipboard writing among the permissions a page may ask for. A page that tries to leave is closed, with **Try again**. Scrolling over a page scrolls the conversation.

## Verification

```sh
./scripts/test-desktop.sh
```

The desktop suite launches real Electron against private temporary Git workspaces and a real Haarness daemon using the deterministic test provider. It does not use paid providers or personal runtime storage. Tests exercise chat and SSE, cross-client updates, approvals, files, diff review, terminal reattachment, settings and lifecycle. Playwright keeps traces and screenshots for failures. Rust tests cover workspace path confinement and conflicting configuration edits. SDK generation and coverage remain part of `scripts/test-sdks.sh`.

On macOS 26+, the dictation E2E also transcribes synthetic speech through the real native helper into the composer. After building, `python3 apps/desktop/tests/native/voice.py` checks native recognition with AIFF and 16 kHz WAV input and cancellation during preparation. Add `--microphone` to check live capture, conversion, stopping, and finalization; this briefly opens the microphone without saving audio or printing microphone transcripts.

The implementation validation record and tested feature matrix are in the [Desktop verification record](records/2026-09-07-desktop-verification.md).

## Presentation ordering

`renderer/transcript.ts` owns the pure presentation rules for transcript items — the tool vocabulary, the change shapes, permission details, error recovery and path shortening — with no DOM or runtime access, so they are covered by ordinary unit tests. `renderer/Session.tsx` renders one `Switch` over item types rather than a chain of conditionals.

`renderer/ordering.ts` owns the pure ID reconciliation and move operations. `renderer/Sortable.tsx` makes whole rows draggable and supplies keyboard controls, drop indicators, and silent ordering. All collections use this layer, rather than separate sorting logic in each component.

The versioned `ordering:v1` desktop record stores ordered IDs under collection scopes (`list:projects`, project-grouped sessions, conversation tabs and terminals, and side-panel tabs). Domain timestamps, session ownership, and server history are unchanged by presentation moves. Invalid persisted data is ignored. Missing/filtered IDs retain their slots, new IDs are reconciled according to the collection's policy, and a move can only target the same collection. This also gives future reorderable collections a common integration point.

`renderer/ProjectGroup.tsx` owns each project’s disclosure and conversation list. The versioned `project-expansion:v1` record stores boolean expansion choices by stable project ID. `renderer/project-overview.ts` caches thread/turn metadata for inactive projects and subscribes while they are expanded, capturing a cursor before its snapshot to replay concurrent changes. Activating a project transfers display ownership to the canonical active workspace state; collapsing an inactive project or disposing the component releases its subscription without stopping the daemon or its work. Shared connection requests and generation guards prevent stale workspace loads from winning after a switch.

Terminal failures are published as durable `item.created` events before `turn.finished`, using the same runtime path as other transcript items. The desktop renders them as live alerts; it does not need a pane change or history reload to discover the error.

Forked conversations remain ordinary sidebar entries; lineage does not hide them. Conversation actions open directly beside the button, with a text dialog only for Rename. New conversations show a centered project-aware starting area with four editable prompt suggestions. Upward wheel, touch or keyboard scrolling pauses stream following immediately, cancels queued automatic scrolls, and keeps that position through Markdown reflow and turn completion. Following resumes at the bottom, through Jump to latest, or with End.

## Projects and worktrees

Projects group conversations from the main project folder and its Git worktrees. Choose **Enable worktree view** from a project’s sidebar menu to show sessions under collapsible checkout groups, with branch names, a new-chat button for each checkout, and menus for opening, resetting, removing, or refreshing worktrees. **New worktree** appears above the groups. Choose **Disable worktree view** to restore the flat session list. New chat keeps the active checkout in worktree view; flat view defaults to the project folder. While Desktop switches to another project or checkout, the message box and Send wait for it, so text typed in that moment goes to the chat it opens, never into the draft of the one being left or to the project being left. Drafts, terminal processes, permissions, and file views stay scoped to their working location. Worktree conversations carry a checkout badge. See [the project model](DESKTOP-PROJECT-MODEL.md) for identity, draft migration, runtime routing, and practical scaling behavior.

Worktrees whose folders are unavailable stay out of the normal sidebar and working-location picker. In worktree view, the project menu's **Show unavailable worktrees** action temporarily reveals them for inspection or removal. Hiding a worktree keeps its registration and conversation history. Restoring its folder and refreshing the project makes it available again.

The same project menu offers **Pin**, which floats the project to the top of the list with a small pin glyph, **Edit**, which gives it a display name of your own, **Section**, which files it under a named heading, **Reveal in Finder** (the platform's file manager elsewhere) for local folders, **Archive chats**, which archives every conversation in the project without leaving the current one and says how many it archived, **Select chats**, and **Remove project**. Sections list first, each under its heading, and once anything is filed the remaining projects follow under a **No section** heading. A pin keeps a project at the top of its own group. Dragging and Alt+arrow ordering move a project among its group's rows, and dropping it onto a row in another group files it there, before or after that row. A section exists as long as a project is filed under it, and its heading menu renames or removes it. Pins, names and sections are desktop presentation stored in `project-presentation:v1`; the runtime's project registry and conversation data are untouched, and a custom name replaces the folder name wherever the desktop shows the project.

**Select chats** puts the project's chat rows in selection mode; Command-click on macOS (Control-click elsewhere) on any chat row starts it there with that row picked, in the flat list too. A tick box replaces each row's status dot, a click picks or drops a row instead of opening it, Shift-click picks every row drawn since the last click, and in worktree view each worktree Haarness created gets a box of its own (the project folder does not). The bar above Settings counts what is picked and offers **Select all shown** (rows inside a collapsed checkout are not shown), **Archive**, **Remove worktrees** once a worktree is picked, and **Cancel**; Escape cancels as well. Each action asks first and lists what it touches: Archive marks chats that are still working, which archiving does not stop, and Remove worktrees deletes the folders, so their chats leave the sidebar, but keeps any worktree with uncommitted changes or running work, as removing one worktree does. A notice then says how many were archived or removed and names any it could not handle with the reason. Haarness has no permanent chat deletion; archived chats stay searchable and can be restored.

## Conversation titles

New conversations are named from the first real user message in a background request while the main turn runs. The configured title-agent model or `small_model` is preferred, with fallback to the conversation model. Titles appear in tabs and sidebars through `thread.updated`, including while tools wait for permission; TUI clients receive the same event. Title failures do not interrupt work, and a manual rename wins over a late generated title. Child sessions and explicitly named conversations keep their existing names.

## Delegated agents

A delegation appears as one compact, clickable task row in its parent's transcript. It shows the task description, agent and resolved model. Active titles shimmer smoothly, with live tool activity underneath and a square stop button that stops only that agent and its descendants. The parent and sibling agents keep running. Finished tasks use quiet completion, failure or stop icons; approvals and questions use attention icons. Status descriptions remain available to screen readers and on hover. Shimmer respects reduced-motion preferences. Link and lifecycle records update that row instead of producing repeated buttons. Clicking the row or pressing Enter opens the child conversation; the separate stop button also supports keyboard activation. Older rows cannot offer a stop control for a newer run in the same child conversation. The child header provides a return to the parent. The child view has no message composer, model controls or dictation input, including after completion or reload. Permission and question responses remain available. Continue the work from the parent; ordinary forks still have their own composers. Opening a child preserves and saves the parent's draft immediately.

The sidebar lists independent conversations, including forks. Delegated agent sessions stay under their parent's task rows, with ongoing work and pending approvals reflected on the parent sidebar row. The server's persisted `isSubagent` flag distinguishes delegation from fork lineage; older databases are migrated using actual task links. A compatibility reader supports older daemons while they finish existing work, without stopping or replacing active turns.

### Approval mode and composer

The rounded composer has attachment and approval controls on the left, with agent/model/effort, dictation and the circular send/stop control on the right. Controls wrap in narrower windows. The **+** button (**Attach**) offers **Attach files**, the system's file picker, and **Attach screenshots**, which shows your newest screenshots: on macOS the folder the Screenshot app saves to (the Desktop unless you chose another under **Options > Save to**), on Windows `Pictures\Screenshots`; Linux has no standard folder, so the first time you choose one and the dialog opens it after that. Each screenshot shows how long ago it was taken. Click the ones you want: each gets a number, and they attach in that order (clicking one again takes it out); the arrow keys move between them and Space picks. **Add 2 screenshots** (the button counts them) attaches them, up to 20 at a time. **Choose folder…** picks from another folder once; next time the dialog opens your screenshots folder again. Desktop only shows the folder's images and reads the ones you pick; it never moves or deletes your screenshots. Files dropped anywhere in the chat attach to the draft, and so do files pasted while the conversation or nothing at all has the focus; pasted text then goes to the message box at its caret. A dropped or pasted file that is not an image, PDF, audio or video file (source code, a text file, a folder) goes into the message box at its caret as its path instead, relative to the project when it is inside it and in quotes when it holds a space; a remote project's server cannot read this computer's files, so there such a file is refused. Paths and pasted text put in the message box this way come out again with Cmd+Z (Ctrl+Z elsewhere). A field of its own (the answer to a question, find, a terminal) keeps its paste. Removing an attachment leaves the focus in the message box. An attached image is typed by its bytes, not its name, and a file named as an image that holds none is refused at once with a notice, while it can still be replaced; HEIC, BMP and TIFF are accepted and converted by the server (see [attachments](API.md)). When a provider refuses an image later, the transcript says which one is left out of the chat from then on. The shield pill reads **Review** or **Auto** and switches the open chat's mode on its server, or the default for new chats in this checkout before one exists. Auto answers requests once, including child agents, and can be turned off without leaving learned permission rules. See [Automatic approvals](AUTO-APPROVALS.md) for scope, persistence and TUI controls.

## Editing commands, agents and other resources

Commands, agents and skills use dedicated editors with a large prompt area and a clear project or all-projects scope. Editing preserves the definition's original scope and unexposed metadata. Commands offer agent/model pickers, an argument hint and an **Insert arguments** action for `$ARGUMENTS`. Agents offer session/subagent modes and optional tool-access override rows. Skill notes have their own field.

Model pickers list connected providers using friendly names. **Use chat model** and **Use chat agent** let reusable definitions inherit the conversation's choices; an existing unavailable selection is preserved until explicitly changed. Custom-provider setup separates API format, authentication and model discovery. API keys stay masked and use the credential store. Explicit model lists use removable rows.

Plugin, marketplace and extension installers ask for a source type first, then show only the relevant folder, repository, marketplace or package fields; local sources support the native file/folder picker.

These editors retain drafts and show an inline error if validation or saving fails. Escape, Cancel and clicking the backdrop dismiss without saving; closing a child editor returns focus to its opener in Settings.

## Servers and projects

Use the server switcher above New chat to choose **This computer** or a saved remote server. The sidebar, project picker and project search show projects from the selected server. Desktop remembers that server and its last project across relaunches. Switching back restores that server’s project and conversation.

**Settings → Servers** manages remote URLs, encrypted credentials and server names. **Settings → Projects** shows the current folder and server at the top, with a refresh action, followed by the saved project list. Project configuration loads automatically when opening a folder. Launch with `HAARNESS_DISABLE_PROJECT_CONFIG=1` to use only global configuration; there is no project-trust prompt. Forgetting a project preserves its saved server connection. Removing a server removes its saved connection and project entries from Desktop; conversations and running tasks remain on the server.

**Open project** on a remote server opens a server-side folder browser. Browse into folders, enter an absolute remote path, or select a known working folder, then choose **Open project**. The selected folder becomes a separate sidebar project on that server. Conversations, drafts, files, approvals and terminals stay scoped to their folder, including across switching and relaunch. Opening another folder does not stop work in the previous one. Local projects continue to use the native folder picker.

Run an updated `haarness serve` on the remote machine to enable folder browsing. Older servers retain their original project and show an update explanation if you request the browser. A host token authorizes browsing and opening folders accessible to that server's OS account; project configuration is enabled by default and follows the host's `--disable-project-config` or `HAARNESS_DISABLE_PROJECT_CONFIG` opt-out. New directory daemons never inherit `--yolo`.

The host gateway resolves canonical folders through the native workspace registry and starts or reuses the existing leased directory daemons. Only the host endpoint is exposed to Desktop. Directory-specific API paths proxy REST, SSE and terminal WebSockets; private daemon tokens stay on the host. Desktop saves one encrypted credential per server and separate connection IDs per working folder. Legacy origin-only connections retain their IDs and history.

## Prompt context

Type **@** in the message composer to search workspace files and delegation agents. Use the arrow keys and Enter/Tab, or click a result, to attach it. The mention becomes a removable context item. Agent mentions request a task delegation without changing the primary conversation agent; hidden and primary-only agents are excluded.

In the file editor, select code and choose **Add selection to context**, or press **Command/Ctrl+Shift+L**. With no selection, the same action adds the whole file. Selected ranges capture complete lines and show their line numbers. Unsaved editor content is supported and labeled. Expand a context item to inspect its captured content, or remove it before sending.

Context is a snapshot taken when attached, so subsequent file edits do not silently change an existing draft. To update it, attach the same file/range again. Context is saved separately for each project, checkout and conversation and restored after reload. Local and remote file mentions use the workspace file API; binary files use the existing media attachment flow. A draft supports up to 20 context items and 128 KiB, with an inline error when the limit is exceeded.

`prompt-context.ts` owns context identity, bounds, mention detection, line selection and serialization. At submission, the shared send path includes fenced file snapshots and delegation instructions in the ordinary user request, including queued guidance and command arguments. This keeps the context inspectable in saved messages and compatible with the current server protocol and other clients. Context clears only after submission succeeds.

## Prompt history

The composer sends messages to the selected agent. Drafts and prompt history stay scoped to each project.

While the message box has focus, **Command/Ctrl+.** switches to the next agent and **Command/Ctrl+Shift+.** to the previous one, and screen readers announce the new agent. Tab and Shift+Tab move keyboard focus, as everywhere in the window; Tab still accepts an open @ or / suggestion. Change either agent key in Settings → Keyboard shortcuts. Saved preferences that still hold the old Tab defaults move to the new keys once.

Press Up at the beginning of the input to recall a submitted entry. Up and Down browse history from either end of a recalled entry; Down past the newest entry restores the unfinished draft. A selection, modified arrow keys, and a caret inside multiline text retain normal editing behavior. Editing recalled text starts a fresh draft. Chat history includes its attachments and file/agent context. Each workspace keeps up to 100 chat history entries across reloads. Large media entries remain available in memory but are omitted from persisted history when they exceed the 1.5 MiB history storage budget.

## Provider logos and model reasoning

Conversation and execution pickers show visible primary/all-mode agents. Agents marked `hidden` or `mode: subagent` remain in the management catalog but are not offered for a new conversation. A stale draft selection falls back to build (or the first available conversation agent); existing conversations and saved configurations are not silently rewritten.

The message composer's border and agent selector use the selected agent's theme color. The border becomes stronger while focused and keeps that agent color while running; approval waiting retains its separate highlight. Built-in agents have fixed color roles, and custom agents receive a stable color based on their ID. These presentation colors live in `agent-colors.ts` and do not change agent permissions or execution.

Provider logos appear in model pickers, provider settings, the model catalog, search results and delegated task details. Assets are bundled with Desktop and adapt to the active theme; custom providers have a neutral icon.

The model picker also controls reasoning. Open **Model**, select a model, then use the reasoning slider or its arrow keys. Each stop comes from that model's advertised reasoning levels, with **Model default** as a separate choice. Reset returns to the model default. Models without configurable levels have no slider. **Done** or Escape closes the picker. The composer, reusable-resource editors and schedules share this component. Changing models resets unsupported effort values, while an existing unavailable saved setting remains visible in editors until changed or reset.

The presentation layers are `provider-brands.ts` (identity), `ProviderIcon.tsx` (bundled artwork), `provider-model-options.tsx` (catalog presentation) and `ModelPicker.tsx` (combined selection). Generic `Select` only renders optional icon slots, a trigger suffix and footer; it contains no provider or reasoning rules. Runtime capabilities remain the source of truth.

## Conversation context and usage

Choose **Context** from the side-panel launcher or **View chat context** in Universal search to open the **Context** side-panel tab. It follows the selected conversation and refreshes when provider usage reports arrive. The open tab is restored with the saved layout after reload. New chats show an empty state until a conversation exists.

The inspector separates three measurements:

- **Retained text estimate** breaks saved context into user messages, assistant messages, tool calls, tool results and compaction summary. It respects undo boundaries and the latest compaction's retained tail. Its four-characters-per-token heuristic excludes system instructions, tool definitions, media and provider overhead, so the displayed share of a known model limit is not full request occupancy. Missing model limits remain unavailable.
- **Latest reported request** displays input, output, cache-read and cache-write tokens as reported, with the actual turn's model when available. Cache semantics differ across providers, so cache counts are not added to input. Reasoning token counts are not separately available in the current protocol. A request that finished cleanly also shows **First token**, the wait from sending it to its first streamed text, reasoning or tool call, and **Output speed**, its output tokens per second over the time they streamed. Speed is left out when the output streamed for less than half a second, where one burst would read as an absurd rate. Requests that were stopped or failed, and those recorded before requests were timed, show neither.
- **Chat totals** uses `threadUsage` for cumulative input/output/cache statistics, including usage from undone and compacted history. Known cost includes only priced requests; unavailable pricing is not shown as zero.

`context-usage.ts` owns the text estimate and history boundaries. `ConversationContext.tsx` owns the inspector and guards requests by connection, conversation and request generation so a late response cannot replace another conversation's statistics. It uses the existing SDK/IPC contract and leaves provider billing and runtime compaction decisions unchanged.

### Editing agents, commands, and skills

Open Settings, choose Agents, Commands, or Skills, and click **Edit**. Your own
definitions save in their existing scope. Editing a built-in or plugin definition
saves a custom version for **All projects** or **This project**; the plugin's
original files remain intact. **Reset to default** removes your override and
restores the original. Skills retain their supporting files, and agent edits
retain their permission rules. These editors also work on a connected remote
server; changes are saved on that server.

Internal agents used for conversation titles and compaction are hidden from
Settings and search, and cannot be edited through definition management.

## Embedded browser

Open **Browser** in the side panel to browse alongside a conversation, or ask Haarness to operate it. See [Browser alongside chat](BROWSER.md) for local and remote networking, tools, files and diagnostics.


Composer history uses the unsent text as a prefix: Up recalls only prompts beginning with that draft, and does nothing when there are no matches. An empty draft browses all history, with the current conversation first. Down past the newest match restores the exact draft, attachments and context. Editing a recalled prompt begins a new search.

### Chrome

Agents can also use the user's own Chrome through Haarness for Chrome ([details](CHROME-EXTENSION.md)). **Settings → Browser → Chrome** shows its connection (**Connected · Google Chrome**, **{count} browsers connected**, **Not connected**, **Paused in Chrome** or **Stopped in Chrome**), **Let agents use Chrome**, **Browser for new agent tabs** and **Chrome site permissions**; these always save globally, and are disabled with the reason for remote projects or servers without `browser-extension.v1`. When a project's own configuration tightens Chrome, the section says **Restricted by this project’s settings** and lists what changed in the interface language (such as "Chrome is turned off for this project." or "Scripts: Block by default"); the rows keep showing the global values. When agent browser control is off, a note says agents can't use Chrome in this project until it is turned on at the top of the page. Search finds them under terms such as `chrome`, `edge`, `brave`, `signed in` and `real profile`.

**Set up Chrome…** (**Manage…** once a Chrome is connected) opens **Use Chrome with Haarness**. Step 1 runs the bundled `haarness browser setup --json` through the main process and shows **Chrome can start Haarness** with the browsers it registered (and why Flatpak or Snap browsers cannot be used). Step 2, **Add Haarness for Chrome**, lists the browser's extensions page with a copy button, then asks to turn on **Developer mode** and keep it on, click **Load unpacked** and paste the folder path (with a copy button and a reveal button, **Reveal in Finder**, **Show in Explorer** or **Show in file manager** as in the project menu, which shows that folder in the file manager), and pin Haarness to the toolbar; once a Web Store listing exists, a store button replaces these steps. Step 3 polls `listBrowserHosts` every 2 s, from **Waiting for Chrome…** to **Connected to Google Chrome**.

A Chrome approval opens the dock as **Allow in Chrome**, with **Deny**, **Allow once** and **Allow in this chat**, and no folder, repository or Auto choices; the fallback auto-approver for older servers skips it. Browser rows whose result came from Chrome carry a small **Chrome** chip in the transcript, and the Browser panel says "This chat also uses Chrome · {count} tabs" while the chat has Chrome tabs.

### Canvas previews

See [Canvas and file previews](CANVAS-PREVIEW.md) for interactive HTML, inline previews, saved versions, comparison and restore.
