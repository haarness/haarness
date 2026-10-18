# Browser alongside chat

> **User guides:** [Test a website in the browser](https://haarness.ai/docs/desktop/projects-and-panels#test-a-website-in-the-browser), [Use your Chrome](https://haarness.ai/docs/desktop/use-your-chrome). This page has the engineering detail.

Agents have two browsers. The **Haarness browser** is Desktop's isolated, built-in Chromium, described first below. **Chrome** is the user's own Google Chrome, with its signed-in profile and its own network, reached through the Haarness for Chrome extension; see [Haarness browser and Chrome](#haarness-browser-and-chrome) and, for the extension and its bridge, [Haarness for Chrome](CHROME-EXTENSION.md). Both use the same `browser_*` tools.

Open a project, open the side panel, then choose **Browser**. You can browse from the new-chat screen before creating a conversation. Use **+** to add tabs, enter a URL, and navigate with Back, Forward and Reload. The command palette also offers **Open browser** (⌘⇧B on macOS, Ctrl⇧B elsewhere). Expanding the side panel gives the page more room.

The new-chat screen has a separate browser for each project. Browsing does not create a conversation or send a prompt. When you send the first message, those live tabs move into the new conversation, preserving form input, cookies, page state, and downloads so the agent can use them immediately. Opening another new chat starts with its own tabs. Draft tab URLs also return after restarting Desktop; temporary logins reset on restart as they do for conversation browsers.

Ask Haarness to open a site, fill a form, inspect a failure, or take a screenshot. The agent can use the same browser you see. When the agent opens or focuses a tab, or takes the tab the panel shows to another page (navigate, Back or Forward), a closed side panel opens on the Browser. If you put the Browser away in a conversation (hid the side panel or closed the Browser's tab while it showed), it stays away while that conversation's agent works: the **Show side panel** button gets a dot, and showing the side panel shows the Browser and clears it. If another panel, such as Review, is showing, it stays and the Browser tab gets a dot until you look. Reloads and pages in a background tab leave the panel as it is.

While the agent loads, clicks, types or takes a screenshot, your keyboard stays where it was, such as in the message box: a page that focuses one of its fields or calls `window.focus()` does not take it. A page that has your keyboard and is put away (the agent shows another tab or closes it) gives it back to the message box, if the message box had it before and Haarness is the app you are using; it never brings Haarness in front of another app. A file chooser a page opens during the agent's action, or in the five seconds after it (a widget may wait for a request first), does not appear: the page sees it cancelled, and the agent is told to use `browser_files_upload`, with that action's result or, for a later chooser, its next one. Pressing a key or clicking in the page ends that hold at once, so to type in the page or choose a file yourself, click the page.

Each conversation has a separate tab inventory and Chromium storage partition. Switching conversations never changes an agent command's target. Tab URLs return after restarting Desktop; cookies and site storage belong to the connected browser host and reset when it reconnects or Desktop restarts. The Haarness browser never uses the user's Chrome profile.

A TUI attached to the **same conversation and server** can use its Desktop browser too. Keep Desktop running; screenshots also require its window and target tab to be visible. Without Desktop, TUI, CLI and SDK conversations on this computer can browse in the user's Chrome once Haarness for Chrome is set up (`/chrome` in the TUI, Settings → Browser → Chrome in Desktop, or `haarness browser setup`).

## Tools

Browser tools are native Haarness tools, available through normal tool discovery. They do not require an MCP server, runtime extension or skill; only work in Chrome needs the Haarness for Chrome browser extension. Every page operation requires the exact `tabID` returned by `browser_tabs_open` or `browser_tabs_list`.

- **Tabs and navigation:** `browser_tabs_list`, `browser_tabs_open`, `browser_tabs_focus`, `browser_tabs_close`, `browser_navigate`, `browser_back`, `browser_forward`, `browser_reload`, `browser_stop`.
- **Inspection and interaction:** `browser_frames`, `browser_snapshot`, `browser_find`, `browser_evaluate`, `browser_click`, `browser_hover`, `browser_drag`, `browser_fill`, `browser_fill_form`, `browser_select`, `browser_check`, `browser_press`, `browser_scroll`, `browser_wait`, `browser_dialog`.
- **Images and files:** `browser_screenshot`, `browser_files_upload`, `browser_files_drop`, `browser_files_list`, `browser_files_get`. A screenshot is saved as the model sees it: shrunk to at most 2000 pixels on a side and 3.75 MB, the limits every provider takes; one that does not decode is saved but not shown to the model.
- **Diagnostics:** `browser_console`, `browser_network_list`, `browser_network_get`, `browser_trace_start`, `browser_trace_stop`, `browser_trace_analyze`, `browser_cpu_start`, `browser_cpu_stop`, `browser_cpu_analyze`, `browser_heap_snapshot`, `browser_heap_summary`, `browser_heap_query`, `browser_heap_object`, `browser_heap_compare`, `browser_lighthouse`.

Snapshots provide element refs such as `@e12`, including non-focusable targets such as named drop zones. They show current field values and redact password values. Native date fields appear as one editable field with their required format, such as `YYYY-MM-DD`. Snapshot depth counts the semantic outline, so extra layout wrappers do not hide a form's controls; omit depth to inspect a complete form. Use refs only with that tab and its latest snapshot or find result. Navigation, a new snapshot, and a new find invalidate previous refs.

Clicks return `dispatched: true` with a bounded observation of visible page text. A click that opens a JavaScript dialog returns its details so the agent can handle it with `browser_dialog`. Dispatch alone does not prove that an asynchronous save completed: inspect or wait for confirmation before deciding what to do next. An unchanged URL is not a reason to repeat a submission.

`browser_evaluate` runs JavaScript in the page, with no server filesystem or Haarness bridge. Page text, JavaScript results, and screenshots remain untrusted website content. In Chrome, the page is the user's signed-in session, so `browser_evaluate` has its own per-origin approval (see [Permissions in Chrome](#permissions-in-chrome)).

Code mode can orchestrate browser tools through the existing confined `execute` tool:

```javascript
const tabs = await tool("browser_tabs_list", {});
return tabs;
```

Nested calls retain ordinary permissions, hooks and cancellation. The parent `execute` call and its returned result are recorded in the conversation. Code mode does not gain direct server networking or filesystem access.

## Haarness browser and Chrome

`browser_tabs_open` takes an optional `browser`: `"haarness"` (the Haarness browser, available only while Desktop hosts this conversation) or `"chrome"` (the user's Chrome). Without it, the server uses `browser.chrome.defaultBrowser`:

| Value | A new tab opens in |
| --- | --- |
| `auto` (default) | The Haarness browser when Desktop hosts this conversation; otherwise Chrome when it is available and the conversation is not part of a scheduled run (the run itself, or a subagent, workflow or workflow agent it started); otherwise the call fails with the most specific reason, Chrome being turned off before Chrome not being connected |
| `haarness` | The Haarness browser, or a failure that suggests `browser: "chrome"` |
| `chrome` | Chrome, or a failure that says why (turned off, paused, stopped, not connected) |

After that, the tab ID decides: every page operation goes to the browser that owns the tab, and a tab ID from another conversation is refused. Several Chrome profiles can be connected at once, each through its own extension; a conversation keeps using the profile that already holds its tabs, and otherwise gets the unpaused one the user focused most recently.

`browser_tabs_list` tags every tab with `browser` and lists the tabs of every browser that holds this conversation. It also returns `browsers`, one entry per browser, for example `{"browser":"chrome","name":"Google Chrome (Work)","available":true}`; when Chrome has never connected to this server and Haarness for Chrome is not set up on this computer, a single unavailable Chrome entry carries the note "not set up; mention only if the user asks for their Chrome". With no browser at all, the list is empty rather than an error, so the model learns what it could use. `browser_tabs_close` returns the remaining tabs of that browser only. Every browser tool result carries `metadata.browser`, which Desktop and the TUI use to mark Chrome rows.

When a browser is missing, the model gets an instruction it can act on rather than a bare error: for example "Chrome is not connected. If the user has not set up Haarness for Chrome, ask them to (Settings → Browser → Chrome, /chrome in the TUI, or `haarness browser setup`); otherwise ask them to open Chrome. Then retry." Paused, stopped, turned-off, revoked and restricted cases have their own texts, owned by the server; the exact strings are in [Haarness for Chrome](CHROME-EXTENSION.md#messages-the-model-sees).

How the two browsers differ:

| | Haarness browser | Chrome |
| --- | --- | --- |
| Where it runs | Desktop, on the client's computer | The user's Chrome, on the same computer as the Haarness server (the only setup v1 supports) |
| Profile | A private storage partition per conversation; logins reset on restart | The user's real Chrome profile: cookies, signed-in accounts, saved site permissions |
| Network | Proxied through the conversation's server; `localhost` is the server | Chrome's own network, proxy and VPN; `localhost` is the user's computer |
| Tabs | The Browser panel | One Chrome tab group per conversation, named after the chat, opened in the background |
| Default site policy | Allow | Requires approval, per conversation and per origin, including scripts |
| Not available | — | Lighthouse, CPU and heap profiling, traces; restricted pages (`chrome://`, extension pages, `file://`, DevTools, `view-source:`) |
| Downloads | Retained per project in Desktop | Saved where Chrome saves them; Chrome's "Ask where to save each file" blocks the agent |

Chrome tabs never take window focus: a new agent tab opens in the background of the last focused window unless `focus` is true, and even then only the tab is activated. `browser_tabs_focus` makes a tab the conversation's focused tab and shows it in its window, except in the window the user is working in, where it comes forward only over another of the conversation's tabs; otherwise it stays in the background and the result says so. A screenshot of a hidden tab is taken by briefly activating it when the user is in another window; when the user is working in that window, the call fails and suggests `browser_snapshot` or asking the user to show the tab. A file chooser a page opens during an agent's action, or in the five seconds after it, does not appear in Chrome either; there the hold always lasts the five seconds, since the extension cannot see the user's own presses in a page. `browser_network_get` in Chrome reports request bodies as `omitted` and redacts more headers than Desktop (`x-*-token`, `x-api-key`, `x-csrf*`, `x-xsrf*` and any header whose name contains `cookie`). On `about:blank` only tab, `browser_navigate`, snapshot, screenshot, wait, frame and find operations run; other pages that are not HTTP(S) are refused as restricted.

The user can share a tab by dragging it into the conversation's group, and take one back by dragging it out, ungrouping or closing it; the model is then told "The user took this tab back from Haarness. Don't use it again without asking." A tab that an agent tab opens (a link in a new tab, a pop-up that Chrome opens as a tab) joins its group; a pop-up in a separate pop-up window cannot join a tab group and is usually not adopted. Chrome traffic is not proxied through the server, so the transfer rules below for remote projects do not apply to it; [file transfer in Chrome](CHROME-EXTENSION.md#files-across-the-bridge) stages uploads on the user's computer and reads back only downloads the extension attributed to that command.

## Local and remote projects

This section is about the Haarness browser; Chrome works only with servers on the computer where it runs. Website traffic goes through the conversation's connected Haarness server, including HTTP, HTTPS and WebSockets. `http://localhost:3000` reaches that server's development app. The private Chromium partition uses an authenticated loopback proxy; it does not silently fall back to the desktop network when the server disconnects. The server connection's credentials are not forwarded to websites. The server tries one of a site's IPv4 addresses first and then alternates IPv6 and IPv4 addresses, giving each at most 3 seconds and the last one whatever is left of 10, so a network that answers IPv6 lookups but drops IPv6 traffic does not stall every page, and IPv4 addresses that never answer do not keep IPv6 from being tried.

Upload paths refer to files on the server. Bytes are transferred to Desktop, and screenshots, downloads and reports are transferred back into a unique directory under the server workspace's `.haarness-chat-attachments`. Chrome's screenshots and downloads land there too. The server creates `.haarness-chat-attachments/.gitignore` containing `*` when it is missing and writes those files with mode 0600, so captures of a signed-in page are not committed by accident. File results include the absolute `path` and a `workspaceRelativePath` beginning with `./`, ready to copy into a reply. Screenshots are also attached to the model's tool result. Transfers are limited to **5 MiB total**, with at most eight uploaded files. Download and capture IDs belong to the tab; use `browser_files_get` to retrieve an existing completed file instead of repeating its browser action.

## Permissions and recovery

Browser operations pass through Haarness's normal availability, hooks and permission engine. The permission target includes the operation and captured resource URL. For example:

```toml
[permissions.browser]
"click:*" = "ask"
"evaluate:*" = "deny"
"navigate:https://example.com/*" = "allow"
```

Uploads additionally pass ordinary `read` and `external_directory` checks, combined into one approval when several gates need consent. The planning agent can list tabs, inspect frames, snapshot, find and screenshot; other browser operations retain their plan restriction. Explicit clicks and address-bar navigation by the user are user actions, independent of model tool permission rules.

Before requesting permission, Haarness captures the browser host, tab, document and relevant frame/file identity. If these change before execution, the action fails instead of being redirected. Only one Desktop window owns a conversation's browser at a time. Cancellation, replacement and disconnect never automatically replay an action: inspect the page before repeating a click, submission or upload whose outcome is unknown. When Chrome disconnects or its extension restarts mid-call, the model is told "Chrome disconnected or the Haarness extension restarted. List tabs and inspect the page before repeating any mutation; its outcome may be unknown."

The Haarness browser's Chromium runs sandboxed with Node integration, the Haarness preload, camera, microphone, screen capture, and device permissions disabled. Navigation is restricted to HTTP, HTTPS and `about:blank`. This controls browser pages; it does not control other desktop applications. Chrome keeps the user's own settings: a site the user already allowed to use the camera, microphone, location or notifications keeps that permission while an agent drives it.

### Permissions in Chrome

Chrome pages are signed in as the user, so Chrome has stricter rules, all enforced by the server:

- Chrome has its own site settings, read from the global configuration only (see [Chrome settings](#chrome-settings)). By default browsing, downloads, uploads and scripts all **Requires approval**.
- A site set to **Requires approval** raises an *explicit* approval for that origin and category. It is satisfied only by a person's answer: **Allow once** for the call, or **Allow in this chat**, which covers that origin and category in this conversation until the server stops. Auto mode, `--yolo` (server or client), hooks, configured permission rules (including `"*": "allow"` and `browser_site:*`) and client auto-repliers never answer it, and a chat shared with `/remote-control` (a published chat) cannot answer it from Cloud. Configured rules can still deny.
- A site set to **Always allow** under Chrome site permissions needs no answer; a site set to **Block** is refused.
- `browser_evaluate` asks separately per origin (`scripts`), even on a site approved for browsing.
- A scheduled run, and every subagent, workflow and workflow agent it starts, fails at once instead of waiting, in turns and in direct tool calls (workflow `tool()`, `POST /v1/threads/{id}/browser/tool`) alike: "Chrome needs the user's approval for &lt;origin&gt; and this run cannot ask. Ask the user to allow it in an interactive chat, then retry." A `--yolo` client (the TUI, `haarness run`, ACP) and the workflow agents it starts still ask: a person is there.
- A **Block** in any layer's `browser.policy` (Agent site permissions) also blocks Chrome; its Allow and Ask rules do not apply to Chrome.
- Inspect reports each document's security origin. Navigation and new tabs gate the URL's origin together with its `www.` twin, so one answer covers both. Screenshots are gated on the top-level document's origin, not on every iframe, and a download on that origin plus the origins of the download's URL and final URL; actions on an element use that frame's origin.
- While a command runs, the extension refuses a main-frame load of a blocked origin, of a non-HTTP(S) URL, of an address carrying a user name or password, or of an origin needing approval that this conversation has not approved, and the model is told to navigate there directly to ask. Frames and resources from a blocked origin, under either spelling of its host (with or without a trailing dot), are not loaded either, without failing the command. Requests that workers and service workers make, and WebSockets, are not filtered.

Prompt patterns, client behaviour and API fields are in [Chrome approvals](PERMISSIONS-AND-EXECUTION.md#chrome-approvals).

## Profiling

Console and request histories are bounded and reset on navigation. Response bodies are omitted unless requested, bounded on export, and never fetched again for inspection. Trace and CPU recording are bounded to 30 seconds. Traces report renderer changes and incomplete captures. Heap reports show shallow sizes; growth between snapshots alone does not establish a leak. Lighthouse audits the loaded page for accessibility, SEO and best practices; it is not a simulated-device performance benchmark.

## Integration and verification

Servers advertise `browser.v1`. The authenticated native-host WebSocket is `/v1/threads/{id}/browser/host`; it carries leased commands, state, results and cancellation. Authenticated tunnel endpoints are private host transport. The public `POST /v1/threads/{id}/browser/tool` executes a named browser tool through normal runtime permissions and persists its result. It returns `output`, `metadata`, and `isError`; screenshots appear in `metadata.attachments`.

Servers that have both the browser broker and management also advertise `browser-extension.v1`, and `/v1/info` includes the server's `pid`. Two endpoints serve Chrome:

- `GET /v1/browser/extension/host?version=1` is the WebSocket the Haarness for Chrome bridge attaches to, one connection per Chrome profile serving every conversation of that server. It requires the bearer token, answers 403 to any request carrying an `Origin` header (browsers always send one; the bridge never does), is refused on open-access servers that have no token, answers 400 when `version` is missing or not `1`, and 501 without a broker. The protocol is in [Haarness for Chrome](CHROME-EXTENSION.md#bridge-and-server).
- `GET /v1/browser/hosts[?thread=<id>]` lists the connected Chrome profiles and the Chrome settings that matter to clients:

  ```json
  {"hosts":[{"kind":"chrome","browserId":"…","name":"Google Chrome","label":null,"version":"154.0.8037.98",
   "extensionVersion":"0.1.1","paused":false,"stopped":false,"connectedAt":"<RFC3339>","threads":2,"tabs":5,"threadTabs":1}],
   "chrome":{"enabled":true,"globalEnabled":true,"blockedBy":null,"defaultBrowser":"auto","everConnected":true}}
  ```

  `threadTabs` counts the tabs of the conversation named by `thread`. `enabled` says whether agents may use Chrome now; `globalEnabled` is Chrome's own switch in the global settings; `blockedBy` says why Chrome is off although that switch is on: `"browser-control"` (agent browser control is off in some layer) or `"project"` (a project's `browser.chrome.enabled: false`), otherwise `null`. Desktop's Settings and Browser panel, the TUI's `/chrome` and `haarness browser status` read it.

Requests scoped to a published chat (one shared with `/remote-control`, carrying `x-haarness-remote-thread`) are denied both endpoints; a computer connected whole with `haarness remote enable` relays unscoped requests and is treated like a local client. Clients treat 403, 404 and 501 as "Chrome is not available here". The SDKs expose them as `connectBrowserExtensionHost` / `connect_browser_extension_host` and `listBrowserHosts` / `list_browser_hosts`.

The TypeScript and Python SDKs expose `callBrowserTool` / `call_browser_tool`. Allow longer request timeouts for diagnostics:

```typescript
const opened = await client.callBrowserTool(threadId, {
  name: "browser_tabs_open",
  arguments: { url: "http://localhost:3000" },
}, { timeoutMs: 90_000 });
const tab = JSON.parse(opened.output);
const snapshot = await client.callBrowserTool(threadId, {
  name: "browser_snapshot",
  arguments: { tabID: tab.id },
}, { timeoutMs: 90_000 });
```

The catalog is generated from the same bounded schemas that Desktop and the Chrome extension decode with, in the shared driver package `packages/browser-driver`; CI fails when it is out of date:

```sh
npm --prefix packages/browser-driver run contract         # regenerate crates/haarness-server/src/browser-tools.json
npm --prefix packages/browser-driver run contract:check   # fail if the catalog is out of date
npm --prefix packages/browser-driver test
npm --prefix apps/desktop run typecheck
npm --prefix apps/desktop run test:e2e -- browser.spec.ts
```

The Electron suite uses disposable Git repositories and actual native Chromium views. Optional live verification uses a real model and an attached tmux TUI: `HAARNESS_BROWSER_LIVE=1`. Local test credentials stay in process memory. Set `HAARNESS_BROWSER_EVIDENCE` to retain test artifacts.

The broader natural-language suite submits 27 prompts across 24 scenarios through Desktop chat and an attached tmux TUI. It verifies actual page state, transferred bytes, screenshots, diagnostics and persisted tool results. With an OpenAI API key available:

```sh
HAARNESS_BROWSER_PROMPTS=1 HAARNESS_BROWSER_EVIDENCE=/tmp/browser-language \
  npm --prefix apps/desktop run test:e2e -- browser-prompts.spec.ts
```

It defaults to `gpt-5.6-luna`, which both an API key and a ChatGPT sign-in can run; set `HAARNESS_BROWSER_MODEL` to test another compatible OpenAI model. Live runs make billable model requests.

Chromium control, diagnostics, profiling and proxy code include MIT-licensed components; see the third-party notices of the [shared driver](../packages/browser-driver/THIRD-PARTY-NOTICES.md) and [Desktop](../apps/desktop/src/main/browser/THIRD-PARTY-NOTICES.md).

The OpenAI Responses adapter explicitly preserves optional tool arguments instead of allowing automatic strict-mode schema normalization. Haarness still validates arguments locally before permission checks. See [OpenAI function calling](https://developers.openai.com/api/docs/guides/function-calling#strict-mode).

Run the checks above for current browser regression coverage. The Chrome extension, its bridge and their tests are covered in [Haarness for Chrome](CHROME-EXTENSION.md#build-and-test).

## Browser settings

Open **Settings → Browser**, or search for a control in Settings or global search (**⌘K / Ctrl+K**). Search understands both labels and terms such as `cookies`, `localhost`, `downloads`, and `disable browser`. Selecting a result opens and focuses its setting. The same search catalog covers the other settings panes, shortcuts, and installed runtime resources.

**Agent browser control** enables or disables native browser tool execution for the connected project. The change applies immediately to Desktop and an attached TUI, including calls waiting for approval. In-progress browser work is cancelled when its policy changes; cancellation cannot undo an action already performed. Manual browser tabs and navigation remain available.

Choose **This project** or **Global default** before changing agent control or site rules; the Chrome rows ignore this choice and always save globally. An explicit project policy overrides inherited settings. **Use inherited settings** removes that scope’s entire browser policy override. The pane displays the effective current-project setting; when editing Global, a project override may still keep that project enabled or disabled. Concurrent edits are revision checked rather than silently overwriting another client’s changes.

The remaining controls apply to this desktop:

- **Web link destination** and **Local URL destination** choose Haarness or the system browser for conversation links. Haarness opens links in the selected conversation or the project's new-chat browser; localhost still reaches its connected server. Without a connected project, links open in the system browser. Authorization links keep their explicit external-browser flow.
- **Show full URL** hides or shows the path, query, and fragment when the address bar is idle. Editing always reveals the full URL and preserves its navigation destination.
- **Browsing history** keeps the latest 1,000 visits per project on this desktop. Search, reopen a visit in the current conversation, or remove entries.
- **Download history** retains the latest 200 browser downloads per project. **Save as…** copies a completed download to a location you choose. **Ask where to save downloads** opens that save dialog automatically after completion. Removing a download removes its retained copy, not a separately saved file.
- **Clear browsing data** closes this project’s browser tabs and clears cookies, site storage, cache, browsing history, and retained downloads. Conversation attachments and files saved separately are preserved. Other projects’ browsing data is unaffected.

**Agent site permissions** has separate Browsing, Downloads, and Uploads choices: Always allow, Requires approval, or Block. Rules use exact HTTP(S) origins, including ports, such as `https://example.com` or `http://localhost:5173`, or a leading subdomain wildcard such as `https://*.example.com`. Paths, queries, any other wildcard, and embedded credentials are rejected. A host's trailing dot (`https://example.com.`) names the same site, in rules and in the pages and requests they govern. Downloads govern browser files exported to the agent; uploads also retain normal file-read and external-directory checks. Normal agent/tool permission restrictions still apply, even when a browser rule says Always allow. Requires approval remains an explicit prompt even if a prior session approval allowed the tool.

During an agent operation, browser requests to blocked sites are refused, WebSockets included: a `ws://` or `wss://` address counts as the `http://` or `https://` origin it opens on. So is any address that carries a user name or password, whatever its site. A redirect or script request to an origin requiring new approval is refused until the agent navigates there directly and obtains authorization. `data:`, `blob:` and `about:` addresses reach no site and are left to the page. Back/Forward navigation checks the destination, including cached pages. This policy governs native browser operations; it does not replace permissions for independent shell or HTTP tools.

Policies are stored under `browser.policy` in the existing scoped `haarness.json`/`haarness.jsonc` configuration files. The authenticated `manageResource("browser-settings", …)` API supports `get`, `set`, and `reset`; mutations require the revision returned by `get`. Servers advertise `browser-settings.v1`.

In the Haarness browser, logins remain temporary and isolated per conversation. Browser profiles/password import, autofill, camera/microphone controls, annotation workflows, WebMCP, and unrestricted CDP are not part of this release; a task that needs the user's own signed-in profile uses Chrome instead.

Settings → Browser also supports **Remember site approvals** and leading subdomain rules such as `https://*.example.com`. Remembering is off by default. See [browser approval behavior](PERMISSIONS-AND-EXECUTION.md#browser-approvals) for persistence scopes, origin matching and redirect boundaries.

### Chrome settings

Settings → Browser has a **Chrome** group after the general rows, headed "Chrome settings apply to every project on this computer.":

- **Chrome** shows the connection ("Connected · Google Chrome", "2 browsers connected", "Not connected", "Paused in Chrome", "Stopped in Chrome") and opens **Set up Chrome…** or **Manage…**.
- **Let agents use Chrome** turns Chrome on or off for agents in every project. When Chrome is still off here, the group says why: agent browser control is off (with a note pointing to that switch), or the project's settings turn Chrome off.
- **Browser for new agent tabs**: **Automatic** (the Haarness browser when the chat is open in Desktop, otherwise Chrome), **Haarness browser** or **Chrome**.
- **Chrome site permissions**: Site, Browsing, Downloads, Uploads and Scripts, with a **Default** row and **Add site**; site rules follow the same origin syntax as Agent site permissions. "Blocked sites in Agent site permissions are blocked in Chrome too."

The rows are disabled, with the reason, when the server lacks `browser-extension.v1` or the project is remote ("Chrome works with projects on this computer."). When project configuration made Chrome stricter, the group says "Restricted by this project's settings" and lists each restriction in the reader's language. The TUI's `/chrome` changes the same two settings with `e` and `d`, and says the same about what keeps Chrome off.

They are stored as `browser.chrome`, a sibling of `browser.policy`, so older daemons that reject unknown `browser.policy` fields keep working:

```jsonc
{
  "browser": {
    "chrome": {
      "enabled": true,
      "defaultBrowser": "auto",            // "auto" | "haarness" | "chrome"
      "defaults": { "browsing": "ask", "downloads": "ask", "uploads": "ask", "scripts": "ask" },
      "sites": [
        { "origin": "https://mail.google.com", "browsing": "allow", "downloads": "ask", "uploads": "ask", "scripts": "ask" }
      ]
    }
  }
}
```

Parsing is lenient: unknown fields are ignored, unknown values fall back to defaults, and an unreadable `browser.chrome` logs a warning and uses the defaults without breaking `browser.policy`. At most 200 site rules are accepted.

The global layer is authoritative. Project layers can only tighten it: a project `browser.chrome.enabled: false` turns Chrome off for that project, and project `defaults` and `sites` can only move a decision from allow to ask to deny, per category; every other project value is ignored. A deny in any layer's `browser.policy.sites` or `browser.policy.defaults` also applies to Chrome for that category (scripts follow browsing), and `browser.policy.enabled: false` turns Chrome off too. Allow and ask rules in `browser.policy` do not affect Chrome. **Remember site approvals** does not apply to Chrome. A Chrome call waiting for approval or running is cancelled only when a change alters a Chrome decision; editing a rule that decides nothing for Chrome (such as an Agent site permissions Allow) leaves it running.

`manageResource("browser-settings", …)` carries Chrome alongside the shared policy:

- `get` adds `chrome: {settings, globalSettings, restrictions, projectRestrictions}`: the effective Chrome settings, the global layer (or `null`), and what project layers tightened. `restrictions` lists it as data for clients to put into their own words: `{"kind":"off"}`, `{"kind":"default","category":"browsing"|"downloads"|"uploads"|"scripts","decision":"ask"|"deny"}` or `{"kind":"site","origin":"https://…","category":…,"decision":…}`. `projectRestrictions` holds the same in English, for older clients.
- `set` accepts `{policy?, chrome?, revision, scope}`. `chrome` must be a complete `ChromeSettings` object and is always written to the global layer; clients send `scope: "global"`, and a request with `chrome` and `scope: "project"` is rejected with "Chrome settings apply to every project on this computer; save them globally."
- `reset` accepts `parts: ["policy", "chrome"]`; without `parts` it resets only `policy`, as older clients expect.

The same lock and revision check cover both.
