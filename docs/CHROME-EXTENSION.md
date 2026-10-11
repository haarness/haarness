# Haarness for Chrome

> **User guide:** [Use your Chrome](https://haarness.ai/docs/desktop/use-your-chrome). This page has the engineering detail.

Haarness for Chrome lets agents in the TUI, the CLI, Desktop and the SDKs use the user's real Google Chrome, with its signed-in profile and its own network, through the same `browser_*` tools as Desktop's built-in Haarness browser. It is a Manifest V3 extension (`apps/browser-extension`), a native messaging host built into the `haarness` binary (`haarness browser native-host`), and an extension-host endpoint on every Haarness server. Version 1 reaches servers on the computer where Chrome runs. How the model chooses a browser, how tabs are routed and how Chrome's settings and approvals work are in [Browser alongside chat](BROWSER.md#haarness-browser-and-chrome) and [Permissions and execution](PERMISSIONS-AND-EXECUTION.md#chrome-approvals).

Names: the product is **Haarness for Chrome**; the manifest name and toolbar button are **Haarness**; Desktop's settings group is **Chrome** under Settings → Browser; the CLI is `haarness browser …`; the TUI command is `/chrome`. Desktop Settings never say "extension" alone, because Settings → Extensions means TypeScript runtime extensions ([EXTENSIONS.md](EXTENSIONS.md)).

## Components

```text
 TUI / CLI / Desktop / SDK chat
          │ browser_* tool call
          ▼
 haarness server (one per project folder)
   BrowserBroker: Desktop hosts per thread, extension hosts per Chrome profile
          ▲  WebSocket GET /v1/browser/extension/host?version=1
          │  bearer token, no Origin header, JSON text frames
          ▼
 haarness browser native-host  (the bridge; Chrome starts one per profile)
   discovery of local servers, relay, upload staging, download reads
          ▲  native messaging: 4-byte little-endian length + UTF-8 JSON on stdin/stdout
          ▼
 Haarness for Chrome service worker (MV3)
   registry, tab groups, policy enforcement, shared browser driver
          │  chrome.debugger (CDP 1.3)         ▲ chrome.runtime messages
          ▼                                    │
   agent tabs in the user's Chrome      popup.html, welcome.html
```

The server never talks to Chrome directly: loopback servers refuse a `chrome-extension://` Origin, and the bearer token must not reach the extension. Chrome starts and stops the bridge, which lives as long as the extension's native port; it never starts, replaces or stops a Haarness server.

## Names and constants

| Name | Value |
| --- | --- |
| Native host name | `ai.haarness.browser` |
| Extension ID | `accdjhmcgljbadpeanajllmcembhkomn`, pinned by the manifest `key` |
| Extra allowed extension IDs | `HAARNESS_BROWSER_EXTENSION_IDS` (comma separated), read by `haarness browser setup` |
| Chrome Web Store URL | none yet: the Rust constant `CHROME_WEB_STORE_URL` in the CLI browser module, reported as `webStoreUrl` |
| Server capability | `browser-extension.v1`, advertised only when the server has both the browser broker and management |
| Extension host endpoint | `GET /v1/browser/extension/host?version=1` |
| Hosts endpoint | `GET /v1/browser/hosts[?thread=<id>]` |
| Settings key | `browser.chrome`, a sibling of `browser.policy`; the global layer is authoritative |
| Minimum Chrome | 125 (`minimum_chrome_version`) |

## Code layout

| Part | Where |
| --- | --- |
| Extension | `apps/browser-extension`: `src/core/*` is browser-neutral, `src/platform/chrome/*` holds the Chrome adapters (`page.ts` as the driver's `PageSurface`, `cdp.ts` as its `CdpTransport` over `chrome.debugger`); `manifest.config.ts` generates the manifest |
| Shared browser driver | `packages/browser-driver` (`@haarness/browser-driver`): the tool schemas (`rpc.ts`), page core, policy, diagnostics and agent cursor that Desktop and the extension both run |
| Bridge and CLI | `crates/haarness-cli/src/browser/`: `setup.rs`, `manifest.rs`, `native_host.rs`, `framing.rs`, `discovery.rs`, `relay.rs`, `files.rs`. Discovery, relay and file staging are transport-neutral; framing and manifests are Chromium-specific |
| Server | `crates/haarness-server/src/browser.rs` (broker, routing, extension host endpoint, hosts API), `browser_policy.rs` (effective Chrome policy) |
| Explicit approvals | `haarness-core` permission engine and runtime, `haarness-protocol` (`PendingPermissionInfo.explicit`), and every client's approval UI |
| Desktop | Settings → Browser → Chrome, the **Use Chrome with Haarness** setup dialog, the approval dock, transcript chips |
| TUI | `/chrome` (alias `/browser`), the `/status` Chrome line, the explicit approval card |

## Setup and data layout

`haarness browser setup` makes Chrome able to start the bridge and prepares an unpacked copy of the extension. Desktop runs the same command from its setup dialog, the TUI from `/chrome`.

```sh
haarness browser setup [--browser <chrome|chrome-beta|chrome-canary|chrome-for-testing|chromium|edge|brave|vivaldi>]… \
  [--manifest-dir <dir>]… [--json] [--quiet] [--if-missing]
haarness browser status [--json]
haarness browser remove [--all] [--json]
```

Haarness keeps these files under its data and cache directories (`~/.local/share/haarness` and `~/.cache/haarness` by default, see [Configuration scopes](CONFIGURATION-SCOPES.md#locations)):

| Path | Content |
| --- | --- |
| `<data>/browser/chrome/haarness-browser-host` (Windows: `haarness-browser-host.bat`) | The wrapper Chrome starts |
| `<data>/browser/chrome/bin/` | A copied `haarness` binary, when the running one is not at a stable path |
| `<data>/browser/chrome/Haarness for Chrome/` | The unpacked extension the user loads |
| `<data>/browser/chrome/ai.haarness.browser.json` | The host manifest on Windows (other systems write it into each browser's folder) |
| `<data>/browser/chrome/removed` | Opt-out marker written by `remove` |
| `<data>/logs/browser-host.log` | Bridge log, without payloads or tokens |
| `<cache>/browser/uploads/<uuid>/` | Upload staging, see [Files across the bridge](#files-across-the-bridge) |

Setup proceeds as follows:

1. **Executable.** The canonical `current_exe()` is used directly when its path is stable: the CLI install directory, a macOS app bundle in `/Applications` or `~/Applications`, or a `target/` directory during development. It is copied to `<data>/browser/chrome/bin/` when the path is unstable or locked: always on Windows (`haarness-<version>.exe`, so an installer can still replace the original), and on Linux when `APPIMAGE` is set or the path starts with `/tmp/.mount_` (as `haarness`). Paths under `/AppTranslocation/` or `/Volumes/` are refused with "Move Haarness to Applications, then run setup again".
2. **Wrapper.** Chrome cannot pass arguments to a host and starts it with only the caller origin (`chrome-extension://<id>/`, plus `--parent-window=N` on Windows) and its own environment, which for a Dock-launched Chrome has none of the shell's variables. The wrapper therefore exports each of `HAARNESS_HOME`, `HAARNESS_DATA_DIR`, `XDG_CONFIG_HOME`, `XDG_DATA_HOME` and `XDG_CACHE_HOME` that was set and absolute when setup ran (POSIX single-quoted, `'` escaped as `'\''`), then runs `exec '<exe>' browser native-host "$@"`. The Windows `.bat` uses `@echo off`, `set "VAR=value"` lines with `%` doubled, and `"<exe>" browser native-host %*`. Setup warns (JSON `warnings[]`) when `HAARNESS_DATA_DIR` or `HAARNESS_HOME` is set and no `--manifest-dir` (or `HAARNESS_BROWSER_MANIFEST_DIR`) was given, because the registration in the user's own Chrome would then point at that non-default root.
3. **Extension copy.** The source is the first of `$HAARNESS_BROWSER_EXTENSION_DIR`, `<exe dir>/../browser-extension/chrome` (inside Desktop's bundle) and `<exe dir>/../../apps/browser-extension/dist/chrome` (a development checkout). It is copied, with an atomic replace, to `<data>/browser/chrome/Haarness for Chrome/` when that is missing or its `manifest.json` `version_name` differs. The data directory is under a hidden folder that the Load unpacked file dialog does not show, which is why every client tells users to press ⌘⇧G (Ctrl+L on Linux) and paste the path.
4. **Host manifest.** `{"name":"ai.haarness.browser","description":"Haarness for Chrome","path":"<wrapper>","type":"stdio","allowed_origins":["chrome-extension://accdjhmcgljbadpeanajllmcembhkomn/", …extra]}` goes into each selected browser's `NativeMessagingHosts` folder. The default selection is Google Chrome always, plus every other known browser whose profile root exists; `--manifest-dir` (repeatable) writes only there. `HAARNESS_BROWSER_MANIFEST_DIR` (a path list) does the same for `setup`, `status` and `remove` when no `--manifest-dir` is given; test runs set it so that nothing, including a Desktop started for a test, registers with the user's own browsers. Desktop refuses to run setup under `HAARNESS_DESKTOP_TEST` without it.

   | Browser | macOS (`~/Library/Application Support/…`) | Linux (`~/.config/…`) | Windows registry (`HKCU\Software\…\NativeMessagingHosts\ai.haarness.browser`) |
   | --- | --- | --- | --- |
   | Google Chrome | `Google/Chrome` | `google-chrome` | `Google\Chrome` |
   | Chrome Beta | `Google/Chrome Beta` | `google-chrome-beta` | — |
   | Chrome Canary / Dev | `Google/Chrome Canary` | `google-chrome-unstable` | — |
   | Chrome for Testing | `Google/Chrome for Testing` | — | — |
   | Chromium | `Chromium` | `chromium` | `Chromium` |
   | Microsoft Edge | `Microsoft Edge` | `microsoft-edge` | `Microsoft\Edge` |
   | Brave | `BraveSoftware/Brave-Browser` | `BraveSoftware/Brave-Browser` | `BraveSoftware\Brave-Browser` |
   | Vivaldi | `Vivaldi` | `vivaldi` | — |

   Each folder above gets `/NativeMessagingHosts/ai.haarness.browser.json`. On Windows the manifest stays at `<data>\browser\chrome\ai.haarness.browser.json` and is registered with `reg add "HKCU\Software\<vendor>\NativeMessagingHosts\ai.haarness.browser" /ve /t REG_SZ /d "<manifest>" /f`. Flatpak (`~/.var/app/*`) and Snap (`~/snap/chromium`) profiles cannot see host manifests outside their sandbox; they are reported with `"supported": false` and a note, never as registered.
5. **`--if-missing`** is what Desktop runs at launch: nothing happens when the `removed` marker exists; otherwise the host is (re)registered when the Chrome registration, the wrapper or the wrapper's executable is missing, or when the wrapper points at a `/target/(debug|release)/` build while the current executable is an installed or packaged one. The extension copy is always refreshed when its `version_name` differs. An explicit `setup` deletes the `removed` marker.

`--json` (and `status --json`, which adds `connections`) prints:

```json
{"hostName":"ai.haarness.browser","extensionId":"accdjhmcgljbadpeanajllmcembhkomn","webStoreUrl":null,
 "executable":"/abs/haarness","wrapper":"/abs/…/haarness-browser-host","removed":false,"warnings":[],
 "browsers":[{"browser":"chrome","name":"Google Chrome","extensionsPage":"chrome://extensions","manifest":"/abs/…json",
   "detected":true,"registered":true,"supported":true,"note":null}],
 "extension":{"path":"/abs/<data>/browser/chrome/Haarness for Chrome","version":"0.1.1-preview.1"},
 "connections":[{"workspace":"/abs/project","serverId":"…","hosts":[…]}]}
```

`registered` means the manifest exists, its `path` is the current wrapper, and the wrapper's executable exists. `extension` is `null` when no extension source was found. <!-- verify --> `extensionsPage` is `chrome://extensions` for Chrome variants and Chromium, `edge://extensions`, `brave://extensions` and `vivaldi://extensions` for the others. Human output is short and ends with the next step: "Open chrome://extensions, turn on Developer mode (keep it on), click Load unpacked, press ⌘⇧G (Ctrl+L on Linux) and paste: &lt;path&gt;. Then pin Haarness to the toolbar."

`remove` deletes the manifests and registry keys it owns, the wrapper and `bin/`, keeps the extension copy unless `--all`, and writes the `removed` marker so Desktop does not register again by itself. `haarness doctor` prints one informational line about the Chrome registration and never fails because of it.

## Native host bridge

`main()` dispatches `browser native-host [ORIGIN] [--parent-window=N]`, and the bare form whose first argument starts with `chrome-extension://`, before typo detection, natural-language argument rewriting and `Paths::new`: the bridge registers no workspace, initialises no layout and writes nothing to stdout except frames. A panic hook never writes to stdout. `setup`, `status` and `remove` run in the early standalone block like `login`.

- **Framing.** Each message is a 4-byte little-endian length followed by UTF-8 JSON. Inbound messages are capped at 64 MiB. Chrome drops the channel when a host sends more than 1 MB, so an outbound message whose JSON exceeds 700 000 bytes is sent as consecutive `{"type":"chunk","id":"<uuid>","index":i,"count":n,"data":"<base64 of up to 700000 bytes of the JSON>"}` frames by a single writer, never interleaved with other messages; the service worker reassembles them before decoding.
- **Shutdown.** Stdin EOF means Chrome is done: close sockets, delete staged uploads and exit within 1.5 s (Chrome kills the host about 3 s later), and never reconnect afterwards. The bridge also exits cleanly when the fingerprint of the wrapper's target executable changes (checked every 2 s), so an update is picked up by the extension's reconnect.
- **Logs** go to `<data>/logs/browser-host.log`, without payloads or tokens.

### Discovery

The bridge serves every Haarness server on this computer that it can find, without ever locking `server.lock` or `connect.lock`, starting or replacing a daemon, or deleting files:

- Roots are the union, canonicalised and deduplicated, of the root resolved from the bridge's environment (as the wrapper set it), the default root resolved with no `HAARNESS_*`/`XDG_*` variables, and the paths in `HAARNESS_BROWSER_EXTRA_ROOTS`, all resolved with the same logic as `Layout::resolve`. A Dock-launched Desktop and a TUI started from a shell with custom `XDG_*` variables can therefore both be served.
- Every 2 s, on `rescan` and after a socket closes, it globs `<cache>/workspaces/*/runtime.json`. Records whose PID is dead are skipped (Unix; Windows relies on the HTTP check), and transient failures are retried with backoff.
- Each candidate must answer `GET /v1/info` on a loopback URL (`0.0.0.0` and `::` are normalised to loopback) within 750 ms, with the bearer token, no `Origin` and no redirects, with `name == "haarness"`, the record's `pid` (when both are present), `browser-extension.v1` and a matching workspace. Servers are deduplicated by `serverInstanceId`. A server without the capability is still listed to the extension, as `state: "error"` with "restart this project's Haarness server".

### Connections

For each server, the bridge opens one WebSocket to `/v1/browser/extension/host?version=1` with `Authorization: Bearer`, only after the extension's `hello`, and sends the cached hello as the first frame. It pings every 20 s and reconnects with backoff (250 ms doubling to 5 s, ±20 % jitter, reset after 30 s connected), rescanning on close. Messages to one server are relayed in arrival order.

While the extension is connected, the bridge polls each server every 2 s with `GET /v1/turns/active` and `GET /v1/permissions`, and sends `activity` to the extension when it changes. Only explicit Chrome approvals are forwarded; the extension uses them for the badge and for answering from the popup.

## Protocols

### Extension and bridge

Extension → bridge:

| Message | Effect |
| --- | --- |
| `{"type":"hello","protocol":1,"browser":{…},"extension":{"version"},"paused","stopped","focusedAt"}` | Starts relaying; cached and replayed to each server |
| `{"type":"to-server","server","message":{…}}` | Relayed to that server |
| `{"type":"stop","server","thread"}` | `POST /v1/threads/{root}/interrupt?resume=true`, where the root follows `parent` through `GET /v1/threads/{id}`; answered with `{"type":"stopped","server","thread","ok","error"?}` |
| `{"type":"thread-info","server","thread"}` | Answered with `{"type":"thread","server","thread","title","archived","parent":id\|null}` |
| `{"type":"answer","server","permission":"<id>","decision":"always"\|"once"\|"reject"}` | `POST /v1/permissions/{id}`, **only** if that id is in the latest forwarded `approvals` for that server; answered with `{"type":"answered","server","permission","ok","error"?}` |
| `{"type":"status","paused","stopped","focusedAt"}` | Updates the cached hello and is forwarded to every server |
| `{"type":"download","server","thread","command":"<command id>","path","url"}` | Announces a finished download, see below |
| `{"type":"rescan"}` | Rescans for servers now |

Bridge → extension: on start `{"type":"bridge","protocol":1,"version":"<haarness version>","uploadsRoot":"<abs>","extension":{"path","version"}|null}`; `{"type":"servers","servers":[{"id","workspace","name":"<basename>","version","state":"connecting"|"connected"|"error","error"?}]}` on every change; `{"type":"from-server","server","message":{…}}`; `{"type":"activity","server","active":["<thread>",…],"approvals":[{"id","thread","patterns":[…],"explicit":true}]}`; and the answers above. The bridge validates everything before any REST call: `server` must be a connected server id, `thread` and `permission` must parse as UUIDs, and URLs are built from path segments. URLs and tokens are never sent to the extension. If `hello.protocol` is not `1`, the bridge answers `bridge` and relays nothing.

### Bridge and server

On upgrade the server sends `{"type":"attached","version":1,"lease":"<uuidv7>","server":{"id":"<serverInstanceId>","workspace":"<abs>","pid":<u32>}}`.

Host → server:

- `hello`, which must arrive within 10 s: `{"type":"hello","browser":{"id":"<uuid>","kind":"chrome","name":"Google Chrome","label":"Work"|null,"version":"154.0.8037.98"},"extension":{"version":"0.1.1"},"paused":false,"stopped":false,"focusedAt":<ms>}`. `browser.id` must be a UUID and `kind` must be `chrome`; `name`, `label` and `version` are at most 64 characters and `extension.version` at most 32. A new connection with the same `browser.id` replaces the old host.
- `state`: `{"type":"state","thread":"<id>","state":{"tabs":[Tab…],"focusedTabID":…},"revoked":["tab_…"]?}` replaces that thread's inventory (`tabs: []` releases it). At most 32 tabs per thread and 256 threads; tab ids, generations and URLs are validated as for Desktop hosts. `revoked` lists tabs the user took back. After every `attached`, the extension sends `state` again for every thread it holds for that server's workspace.
- `result`: `{"type":"result","id","outcome":Outcome}`; `status`: `{"type":"status","paused":bool,"stopped":bool,"focusedAt":ms}`; `ping`, answered with `{"type":"pong"}`.

Server → host: `{"type":"command","id","thread":"<id>","title":"<thread title>","parent":"<parent thread id>"|null,"command":{…}}` with the same command shape Desktop hosts receive, `{"type":"cancel","id"}` and `{"type":"pong"}`. Unknown message types and fields are ignored; invalid JSON, binary frames and invalid `state` close the connection. The thread id always comes from the runtime, never from tool arguments.

Server ↔ bridge commands carry files as base64 `File`s. Bridge ↔ extension use the driver's `HostCommand`/`HostResult`, in which a file may also be a `FileRef` `{id, name, mime, bytes, path}`.

Failure codes the extension returns in an outcome: `paused`, `stopped-by-user`, `unsupported`, `restricted-page`, `not-in-group`, `site-blocked` and `background-tab`. The server replaces the message with its own text for each, except `site-blocked`, whose message names the origin and passes through.

### Messages the model sees

The server owns these texts:

| Case | Text |
| --- | --- |
| Haarness browser requested, no Desktop host | The Haarness browser is available only while this chat is open in Haarness Desktop. Use browser "chrome" to work in the user's Chrome, or ask the user to open this chat in Desktop. |
| Chrome not connected | Chrome is not connected. If the user has not set up Haarness for Chrome, ask them to (Settings → Browser → Chrome, /chrome in the TUI, or `haarness browser setup`); otherwise ask them to open Chrome. Then retry. |
| Chrome turned off | Agents may not use Chrome: it is turned off in Settings → Browser → Chrome (or /chrome in the TUI). Ask the user; do not retry. |
| Paused | The user paused Haarness in Chrome. Ask them to resume it from the Haarness button in Chrome's toolbar; do not retry until they do. |
| Stopped | The user stopped Haarness in Chrome (they cancelled the debugging bar or pressed Stop). Ask before trying again; they can click Resume on the Haarness button in Chrome's toolbar. |
| No browser | No browser is available to this chat. Open it in Haarness Desktop to use the Haarness browser, or set up Haarness for Chrome (/chrome in the TUI or `haarness browser setup`) to use Chrome. |
| Scheduled run, no usable browser | This scheduled run has no browser it may use unattended. Use the Haarness browser in Desktop, or pass browser "chrome" only if the user asked for their Chrome. |
| Tab taken back | The user took this tab back from Haarness. Don't use it again without asking. |
| Chrome or the extension went away mid-call | Chrome disconnected or the Haarness extension restarted. List tabs and inspect the page before repeating any mutation; its outcome may be unknown. |
| `unsupported` | This browser tool is not available in Chrome; use the Haarness browser in Desktop for it. |
| `restricted-page` | Haarness cannot control this Chrome page (browser, extension and file pages are off limits). Navigate the tab to an http(s) site. |
| `not-in-group` | This tab is no longer in the chat's Chrome tab group. Call browser_tabs_list. |
| `background-tab` | Chrome cannot capture this tab while it is in the background of the window the user is using. Use browser_snapshot, or call browser_tabs_focus first. |

A tab id that no host lists answers "Tab does not belong to this conversation or is closed. Call browser_tabs_list."; a small LRU (512 entries) remembers which kind of host owned a closed tab, so a later call gets that browser's recovery text. Desktop hosts keep their own texts, and "Agent browser control is disabled…" now adds "(or /chrome in the TUI)".

## Routing, policy and approvals

The broker keeps Desktop hosts per thread exactly as before, and extension hosts per `browser.id`, each with its threads' inventories and its `paused`, `stopped`, `focusedAt` and `connectedAt`. The CLI daemon wiring injects a thread lookup (title, parent, whether the thread belongs to a schedule) so commands carry the chat title for the tab group and Automatic routing can skip Chrome for scheduled runs. Routing, `browser_tabs_list` merging and the selector are described in [Browser alongside chat](BROWSER.md#haarness-browser-and-chrome); paused and stopped hosts fail before a command is sent.

For an extension host the server computes an effective `BrowserPolicy` from the Chrome settings (global layer, project tightening, shared Block rules; see [Chrome settings](BROWSER.md#chrome-settings)) plus a `scripts` decision for `evaluate`, sends it with every command, and gates each HTTP(S) resource origin: deny refuses, allow passes, ask pushes the remember pattern `chrome-<category>:<thread id>:<origin>` (categories `browsing`, `downloads`, `uploads`, and `scripts` for `browser_evaluate`). Those patterns are explicit approvals; see [Chrome approvals](PERMISSIONS-AND-EXECUTION.md#chrome-approvals).

## Files across the bridge

- **Uploads.** For each base64 `File` in a command, the bridge writes the bytes to `<uploadsRoot>/<uuid>/<sanitised name>` (directory 0700, file 0600), where `uploadsRoot` is `<cache>/browser/uploads`, and replaces it with a `FileRef`. The extension accepts upload paths only under `uploadsRoot` and passes them to `DOM.setFileInputFiles` or `Input.dispatchDragEvent`. Staged files are deleted 10 minutes after the command's result and when the bridge shuts down.
- **Downloads.** The extension attributes a download to an agent only through CDP `Page.downloadWillBegin` on an attached agent tab during a command, matched to `chrome.downloads.onCreated` by URL within 5 s, and announces it with a `download` message when it completes. Unmatched downloads are never exposed. When a relayed `files.get` result carries a `FileRef`, the bridge reads it only if the extension announced that exact path for the same server, thread and command after the command was relayed. It opens it without following symlinks and requires a regular file with one link, at most 5 MiB, changed after the command was relayed, and outside every Haarness data, config and cache root and every `~/.*` directory. The bytes go to the server as a base64 `File`, which stores it under `.haarness-chat-attachments`; any failure becomes "Chrome's download could not be read: &lt;reason&gt;".
- Downloads land wherever Chrome's own settings put them, and stay there. With Chrome's "Ask where to save each file before downloading" on, Chrome opens a native Save dialog that the agent cannot answer; the user guide tells users to turn it off.

## The extension

- **Service worker.** Only the service worker calls `connectNative`; the popup and welcome page talk to it with `chrome.runtime` messages, checking `sender.id === chrome.runtime.id`. All listeners are registered synchronously at top level, and an open native port keeps the worker alive. Disconnect reasons map to states: "Specified native messaging host not found." and "Access to the specified native messaging host is forbidden." mean *not set up* (run setup again), "Native host has exited." means the bridge exited. It reconnects every 2 s while the welcome page or popup is open, otherwise with backoff and a one-minute `chrome.alarms` wake-up.
- **Registry.** Keyed by (workspace, thread), not by server id, in `chrome.storage.session`: Haarness tab ids mapped to Chrome tab ids, generations, group ids, the origins each thread's executed commands were approved for (cleared when that workspace's server instance changes), and paused/stopped state. The snapshot ref counter is seeded from a persisted value so refs never repeat after a worker restart.
- **Tab groups.** One group per (workspace, thread), titled with the chat title (at most 40 characters, "Haarness" as fallback) and a colour derived from the thread id; a subagent thread gets its own group with its own title. Chrome's saved tab groups sync these titles to the user's Google account when tab group sync is on; the user guide says so. Without `chrome.tabGroups`, tabs are tracked ungrouped. New tabs open in the last focused normal window (one is created only if none exists), in the background unless `focus` is true, which activates the tab only; agent commands never call `windows.update({focused: true})`. The popup's **Show tabs**, a user action, is the only place that focuses a window.
- **Ownership.** Before attaching and before every command the extension re-reads the live tab, including its URL over CDP, and its `groupId`. It refuses `restricted-page` for `chrome://`, `chrome-extension://`, `devtools://`, `file://`, `view-source:` and any other non-HTTP(S) URL except `about:blank`, and `not-in-group` when the tab left the group. A pop-up whose `openerTabId` is an agent tab becomes an agent tab of the same thread at once, and is closed if its `pendingUrl` origin is blocked. A tab the user drags into the group is shared (HTTP(S) only; restricted tabs and tabs over the 32-tab limit are silently ungrouped again); dragging out, ungrouping or closing revokes it.
- **Debugger.** The extension attaches `chrome.debugger` (protocol 1.3) on a thread's first command and detaches 30 s after that thread's last command, so Chrome's "started debugging this browser" bar does not stay up; while detached, URL changes from `tabs.onUpdated` bump the tab generation. `onDetach` with `canceled_by_user` (the user pressed Cancel on the bar) puts the whole extension into the **stopped** state: every command fails `stopped-by-user` and every server gets `status {stopped: true}` until the user clicks **Resume**.
- **Policy while a command runs.** Before dispatching input the extension awaits `Network.setBypassServiceWorker({bypass: true})`, disables prerendering where Chrome allows it, and enables `Fetch` on the root and child sessions with a `Document` pattern plus one URL pattern per blocked origin. Each redirect hop is decided separately: a blocked origin (any resource type), a main-frame document with a non-HTTP(S) scheme, or a main-frame document on an ask origin this thread has not approved fails with `BlockedByClient` and records `site-blocked`; everything else continues. After each command, and on a main-frame `frameNavigated` within 2 s, a backstop stops a tab that landed on a blocked or unapproved origin, navigates it to `about:blank` and fails the command. Requests made by workers and service workers are not covered.
- **Driver.** The extension runs the shared driver from `packages/browser-driver` through a Chrome `PageSurface` (`selectAll` via the `selectAll` editing command, captures and downloads gated on the top document, resources reported as security origins) and a `CdpTransport` that demultiplexes `chrome.debugger.onEvent` by tab and flattened child session. `lighthouse`, `heap.*`, `cpu.*` and `trace.*` are `unsupported`. `network.get` omits request bodies ("omitted in Chrome") and redacts `x-*-token`, `x-api-key`, `x-csrf-*`, `x-xsrf-*` and cookie headers in addition to Desktop's list. On opaque or non-HTTP(S) documents only `tabs.*`, `navigate`, `snapshot`, `screenshot`, `wait`, `frames` and `find` run; everything else is `unsupported`.
- **Screenshots** use `Page.captureScreenshot`. For a hidden tab whose window is not the focused window, the extension activates the tab, captures and restores the previously active tab; when the user is in that window, it fails `background-tab`.
- **Activity.** From the bridge's `activity` messages: a chat is *Working* while it has an active turn and owns tabs (its **Stop** button shows then), and the badge shows the number of working chats, or `!` when an explicit Chrome approval is waiting.
- **Pause, Stop, Stop all.** Pause aborts in-flight commands, detaches everywhere and fails new commands with `paused`. Stop sends `stop` for that chat's root thread. The `stop-all` command ("Stop Haarness in this browser", suggested key Alt+Shift+S) stops every chat working in this browser. <!-- verify: stop-all semantics -->
- **Agent cursor.** The shared cursor script runs in an isolated world (`Page.createIsolatedWorld` + `Runtime.evaluate`) when "Show the agent's cursor" is on.
- **Updates.** When the bridge reports an on-disk extension version different from the running one and nothing is in flight, the extension calls `chrome.runtime.reload()`; otherwise the popup shows "Update ready · Reload".
- **UI.** Solid, with tokens copied from Desktop's `theme.css`, light and dark, logical CSS properties and right-to-left Arabic, system fonts and no network fetches. The 360 px popup shows the status pill (Connected, Connecting…, Not set up, Paused, Stopped, Update ready), waiting approvals with **Allow in this chat** and **Deny**, setup help when not set up (with a chrome://policy hint after repeated failures), **Resume** when stopped, and per-chat **Show tabs**, **Stop** and **Close tabs**, plus **Close all agent tabs** and the **Let Haarness use this browser** switch. The welcome page (`welcome.html`, also the options page) walks through setup with live steps, explains what Haarness can do and the debugging bar, and holds the language, profile label and cursor settings, connected projects, versions and troubleshooting.
- **Languages.** Strings use `t("English")` keys from the repository's `locales/*.json`; the build writes per-language subsets to `dist/chrome/locales/`. English is the default; an explicit choice is stored in `chrome.storage.local`.

The manifest (generated by `manifest.config.ts`): MV3, `name: "Haarness"`, description "Let Haarness agents use Chrome — with your permission.", the pinned `key`, permissions `debugger`, `tabs`, `tabGroups`, `nativeMessaging`, `storage`, `alarms` and `downloads`, `incognito: "not_allowed"`, no content scripts, no `web_accessible_resources`, no `externally_connectable`, a module service worker `background.js`, the popup `popup.html`, the options page `welcome.html`, icons from `brand/png`, and the `stop-all` command. `version` maps the workspace version to Chrome's integer format (`X.Y.Z-preview.N` → `X.Y.Z.N`, `X.Y.Z` → `X.Y.Z.1000`) and `version_name` keeps the workspace version. <!-- verify: host_permissions dropped unless chrome.debugger.attach needed them -->

## Build and test

```sh
npm run extension:install                         # ci for packages/browser-driver and apps/browser-extension
npm run extension:build                           # typecheck + vite build → apps/browser-extension/dist/chrome
npm run extension:test                            # unit tests (node --test with chrome.* fakes)
npm --prefix apps/browser-extension run typecheck
npm --prefix apps/browser-extension test
npm --prefix packages/browser-driver test
cargo test -p haarness-cli browser                # setup, manifests, framing, discovery, file staging, bridge loop
cargo test -p haarness-server browser             # broker routing, policy layering, gates, extension host endpoint
cargo test -p haarness-core explicit              # explicit approvals in the runtime
./scripts/test-sdks.sh                            # connectBrowserExtensionHost and listBrowserHosts coverage
(cd apps/desktop && HAARNESS_BIN=<copied haarness> npx playwright test e2e/chrome-settings.spec.ts)
                                                  # Desktop: Settings › Browser › Chrome, approvals, and a real
                                                  # `browser setup` into HAARNESS_BROWSER_MANIFEST_DIR
```

<!-- verify: exact root script names and cargo test filters after integration -->

To try a development build by hand: `cargo build -p haarness-cli`, `npm run extension:build`, then `target/debug/haarness browser setup` (it finds `apps/browser-extension/dist/chrome` from a `target/` binary) and load the printed folder with **Load unpacked**. Use an isolated `HAARNESS_DATA_DIR` together with `--manifest-dir <chrome user data dir>/NativeMessagingHosts` and a separate Chrome profile (`--user-data-dir`) to keep a development host away from your normal Chrome.

### End to end

The extension's Playwright suite (`npm --prefix apps/browser-extension run test:e2e`) is hermetic:

- It launches Chromium with `chromium.launchPersistentContext(userDataDir, {headless: true, args: ["--disable-extensions-except=<dist/chrome>", "--load-extension=<dist/chrome>", "--silent-debugger-extension-api"], env: {HAARNESS_DATA_DIR: <tmp>, …}})`. The executable is `HAARNESS_E2E_CHROMIUM`, or Playwright's Chromium (`npx playwright install chromium`, channel `chromium`). <!-- verify -->
- Chromium reads host manifests from `<userDataDir>/NativeMessagingHosts/` on macOS and Linux, so `haarness browser setup --manifest-dir <userDataDir>/NativeMessagingHosts --json` registers the host without touching the user's real Chrome. Windows only reads the registry, so the suite does not run there.
- The daemon comes from `haarness --workspace <dir> desktop` with `{"action":"connect"}`; tools are called through the SDK's `callBrowserTool` with the deterministic echo provider, and permissions are answered through the SDK.

Pitfalls: branded Google Chrome 137 and later ignores `--load-extension` (load over CDP `Extensions.loadUnpacked` with `--enable-unsafe-extension-debugging` instead, and drop Playwright's default `--disable-extensions`); `chromium-headless-shell` does not run extensions at all; an attached Playwright keeps an idle service worker alive, so worker-lifetime tests need a launcher without a CDP client; and the debugging bar appears only headed and without `--silent-debugger-extension-api`. Command-line extensions get `file://` access by default, unlike store installs, so restricted-page tests must not rely on it.

## Packaging

- `apps/desktop/scripts/prepare.mjs` builds the extension, and electron-builder's `extraResources` copies `apps/browser-extension/dist/chrome` to `browser-extension/chrome` beside `bin/` in the app (`Haarness.app/Contents/Resources/browser-extension/chrome` on macOS). `scripts/test-release-package.py` checks that its `manifest.json` exists and that its version follows the mapping above.
- A packaged Desktop runs `haarness browser setup --quiet --if-missing --json` once in the background at launch, except when `HAARNESS_DESKTOP_TEST`, `HAARNESS_DATA_DIR` or `HAARNESS_DESKTOP_DATA` is set. Because the copy in the data directory is refreshed whenever the bundled version changes, an unpacked install keeps working across Desktop updates; the extension reloads itself when nothing is in flight.
- The CLI release is a single binary and carries no extension. Until the Web Store listing exists, `haarness browser setup` finds an extension only next to a Desktop bundle, in a development checkout, in `HAARNESS_BROWSER_EXTENSION_DIR`, or already copied into the data directory by Desktop. <!-- verify: whether a CLI-only install gets the extension some other way -->
- The extension bundles the shared driver, which includes MIT-licensed code; its `dist` ships the matching third-party notices.

## Release and Chrome Web Store publishing

There is no Web Store listing yet: `CHROME_WEB_STORE_URL` is `None`, `webStoreUrl` is `null`, and every client shows the Load unpacked steps. When a listing exists, set the constant; Desktop's setup dialog then shows a Web Store button instead of the steps, and the CLI and TUI print the URL.

**The key.** The manifest `key` is the extension's public key, committed as one line of base64 so the unpacked ID is always `accdjhmcgljbadpeanajllmcembhkomn`. The private key never enters a repository. The release owner keeps it in a `.chrome-extension-key/` folder beside the checkouts: `key.pem` (the private key, mode 0600), `pub.der` and `pub.b64` (the public key; `pub.b64` is the manifest `key`) and `id.txt` (the ID derived from it). To derive the ID from the public key:

```sh
python3 -c "import hashlib,sys;h=hashlib.sha256(open(sys.argv[1],'rb').read()).hexdigest()[:32];print(''.join(chr(97+int(c,16)) for c in h))" pub.der
```

**Publishing.** <!-- verify: Chrome Web Store rules at the time of the first upload -->

1. Build with `npm run extension:build`. The store upload is a ZIP of `dist/chrome` whose `manifest.json` has no `key` field; the store manages the item's key.
2. To keep the same ID in the store, upload the first version with the private key included as `key.pem` at the root of the ZIP (Chrome's documented way to upload a previously packaged extension); otherwise the store assigns a new ID, which must be added to the host's `allowed_origins` through the CLI constants and `HAARNESS_BROWSER_EXTENSION_IDS` for testing.
3. Fill in the listing: single purpose ("let Haarness agents use Chrome with the user's permission"), a justification for each permission (`debugger` to drive tabs the agent owns, `nativeMessaging` to reach Haarness on this computer, `tabs`/`tabGroups` for the agent's tab groups, `downloads` to hand downloads the agent started to it, `storage`/`alarms` for state and reconnection), and a privacy statement: the extension makes no network requests and sends page content only to the Haarness server on the same computer. Expect the `debugger` permission to trigger a longer review.
4. Set `CHROME_WEB_STORE_URL`, release Haarness, and update the user guide's setup steps.

**Development IDs.** Anyone can build an unpacked extension with the committed public key and get the same ID, which the host accepts. That is acceptable while unpacked installs are the only channel, because loading one needs Developer mode and a deliberate user action. Once the store listing exists, release builds should allow only the store ID by default, and development IDs should be opt-in at setup time (`HAARNESS_BROWSER_EXTENSION_IDS`, or a separate development key and host name such as `ai.haarness.browser.dev`), so that a stray unpacked copy cannot impersonate the store extension.

Releases keep the extension, the bridge and the server on one version: `version_name` equals the workspace version, the bridge reports its own version in the `bridge` message, and the extension reloads itself when the on-disk copy changes. The wire protocols are versioned separately (`protocol: 1` between extension and bridge, `version=1` on the server endpoint), so a mismatch is refused rather than misread.

## Firefox and Safari

Neither is supported yet. The seams for them are in place:

- **Bridge.** Discovery, relay and file staging (`discovery.rs`, `relay.rs`, `files.rs`) are independent of the browser; framing and manifests are Chromium-specific. Firefox uses the same stdio framing but reads manifests from `~/Library/Application Support/Mozilla/NativeMessagingHosts/` (macOS) or `~/.mozilla/native-messaging-hosts/` (Linux), uses `allowed_extensions` instead of `allowed_origins`, and passes the manifest path and extension ID as arguments. Safari web extensions ship inside a macOS app and reach native code through that app, not through a stdio host; the commercial-boundary check (`scripts/check-commercial-boundary.sh`) also applies to any Xcode project files.
- **Extension.** `src/core/*` is browser-neutral and `src/platform/chrome/*` holds the Chrome adapters; the build target is `dist/chrome`, so a `dist/firefox` can sit beside it.
- **Automation.** Firefox has no `chrome.debugger`, so it needs a different backend behind the driver's `PageSurface` and `CdpTransport` boundary, most likely DOM-based through content scripts, with a smaller set of supported operations.
- **Identity.** `hello.browser.kind` and the model-facing selector value are the places a new browser name enters; both are `chrome` today.

## Limits in this version

- Servers on this computer only: a project that Desktop or `haarness attach` reaches on another machine cannot use Chrome; Desktop says "Chrome works with projects on this computer." and the TUI "Chrome works with projects on the computer where Chrome runs". A chat shared through remote control still runs here and can use Chrome, but remote-scoped clients cannot list hosts or answer Chrome approvals.
- Open-access servers (started with no token) refuse the extension host.
- Flatpak and Snap browsers cannot see host manifests and are reported as unsupported.
- Incognito windows are excluded (`incognito: "not_allowed"`).
- Organization policies can block it: `NativeMessagingUserLevelHosts`, `NativeMessagingBlocklist`, extension install blocklists, or Developer mode disabled by policy. The popup then stays at "Not set up" and points the user at `chrome://policy`.
- Windows registration is not covered by the hermetic end-to-end suite.
