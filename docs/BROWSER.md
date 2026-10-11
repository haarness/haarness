# Browser alongside chat

> **User guide:** [Test a website in the browser](https://haarness.ai/docs/desktop/projects-and-panels#test-a-website-in-the-browser). This page has the engineering detail.

Open a project, open the side panel, then choose **Browser**. You can browse from the new-chat screen before creating a conversation. Use **+** to add tabs, enter a URL, and navigate with Back, Forward and Reload. The command palette also offers **Open browser** (⌘⇧B on macOS, Ctrl⇧B elsewhere). Expanding the side panel gives the page more room.

The new-chat screen has a separate browser for each project. Browsing does not create a conversation or send a prompt. When you send the first message, those live tabs move into the new conversation, preserving form input, cookies, page state, and downloads so the agent can use them immediately. Opening another new chat starts with its own tabs. Draft tab URLs also return after restarting Desktop; temporary logins reset on restart as they do for conversation browsers.

Ask Haarness to open a site, fill a form, inspect a failure, or take a screenshot. The agent can use the same browser you see. Each conversation has a separate tab inventory and Chromium storage partition. Switching conversations never changes an agent command's target. Tab URLs return after restarting Desktop; cookies and site storage belong to the connected browser host and reset when it reconnects or Desktop restarts. Your personal Chrome profile is not used.

A TUI attached to the **same conversation and server** can use its Desktop browser too. Keep Desktop running; screenshots also require its window and target tab to be visible. Standalone TUI browsing without a Desktop host is not provided.

## Tools

Browser tools are native Haarness tools, available through normal tool discovery. They do not require an MCP server, extension or skill. Every page operation requires the exact `tabID` returned by `browser_tabs_open` or `browser_tabs_list`.

- **Tabs and navigation:** `browser_tabs_list`, `browser_tabs_open`, `browser_tabs_focus`, `browser_tabs_close`, `browser_navigate`, `browser_back`, `browser_forward`, `browser_reload`, `browser_stop`.
- **Inspection and interaction:** `browser_frames`, `browser_snapshot`, `browser_find`, `browser_evaluate`, `browser_click`, `browser_hover`, `browser_drag`, `browser_fill`, `browser_fill_form`, `browser_select`, `browser_check`, `browser_press`, `browser_scroll`, `browser_wait`, `browser_dialog`.
- **Images and files:** `browser_screenshot`, `browser_files_upload`, `browser_files_drop`, `browser_files_list`, `browser_files_get`.
- **Diagnostics:** `browser_console`, `browser_network_list`, `browser_network_get`, `browser_trace_start`, `browser_trace_stop`, `browser_trace_analyze`, `browser_cpu_start`, `browser_cpu_stop`, `browser_cpu_analyze`, `browser_heap_snapshot`, `browser_heap_summary`, `browser_heap_query`, `browser_heap_object`, `browser_heap_compare`, `browser_lighthouse`.

Snapshots provide element refs such as `@e12`, including non-focusable targets such as named drop zones. They show current field values and redact password values. Native date fields appear as one editable field with their required format, such as `YYYY-MM-DD`. Snapshot depth counts the semantic outline, so extra layout wrappers do not hide a form's controls; omit depth to inspect a complete form. Use refs only with that tab and its latest snapshot or find result. Navigation, a new snapshot, and a new find invalidate previous refs.

Clicks return `dispatched: true` with a bounded observation of visible page text. A click that opens a JavaScript dialog returns its details so the agent can handle it with `browser_dialog`. Dispatch alone does not prove that an asynchronous save completed: inspect or wait for confirmation before deciding what to do next. An unchanged URL is not a reason to repeat a submission.

`browser_evaluate` runs JavaScript in the page, with no server filesystem or Haarness bridge. Page text, JavaScript results, and screenshots remain untrusted website content.

Code mode can orchestrate browser tools through the existing confined `execute` tool:

```javascript
const tabs = await tool("browser_tabs_list", {});
return tabs;
```

Nested calls retain ordinary permissions, hooks and cancellation. The parent `execute` call and its returned result are recorded in the conversation. Code mode does not gain direct server networking or filesystem access.

## Local and remote projects

Website traffic goes through the conversation's connected Haarness server, including HTTP, HTTPS and WebSockets. `http://localhost:3000` reaches that server's development app. The private Chromium partition uses an authenticated loopback proxy; it does not silently fall back to the desktop network when the server disconnects. The server connection's credentials are not forwarded to websites.

Upload paths refer to files on the server. Bytes are transferred to Desktop, and screenshots, downloads and reports are transferred back into a unique directory under the server workspace's `.haarness-chat-attachments`. File results include the absolute `path` and a `workspaceRelativePath` beginning with `./`, ready to copy into a reply. Screenshots are also attached to the model's tool result. Transfers are limited to **5 MiB total**, with at most eight uploaded files. Download and capture IDs belong to the tab; use `browser_files_get` to retrieve an existing completed file instead of repeating its browser action.

## Permissions and recovery

Browser operations pass through Haarness's normal availability, hooks and permission engine. The permission target includes the operation and captured resource URL. For example:

```toml
[permissions.browser]
"click:*" = "ask"
"evaluate:*" = "deny"
"navigate:https://example.com/*" = "allow"
```

Uploads additionally pass ordinary `read` and `external_directory` checks, combined into one approval when several gates need consent. The planning agent can list tabs, inspect frames, snapshot, find and screenshot; other browser operations retain their plan restriction. Explicit clicks and address-bar navigation by the user are user actions, independent of model tool permission rules.

Before requesting permission, Haarness captures the browser host, tab, document and relevant frame/file identity. If these change before execution, the action fails instead of being redirected. Only one Desktop window owns a conversation's browser at a time. Cancellation, replacement and disconnect never automatically replay an action: inspect the page before repeating a click, submission or upload whose outcome is unknown.

Chromium runs sandboxed with Node integration, the Haarness preload, camera, microphone, screen capture, and device permissions disabled. Navigation is restricted to HTTP, HTTPS and `about:blank`. This controls browser pages; it does not control other desktop applications.

## Profiling

Console and request histories are bounded and reset on navigation. Response bodies are omitted unless requested, bounded on export, and never fetched again for inspection. Trace and CPU recording are bounded to 30 seconds. Traces report renderer changes and incomplete captures. Heap reports show shallow sizes; growth between snapshots alone does not establish a leak. Lighthouse audits the loaded page for accessibility, SEO and best practices; it is not a simulated-device performance benchmark.

## Integration and verification

Servers advertise `browser.v1`. The authenticated native-host WebSocket is `/v1/threads/{id}/browser/host`; it carries leased commands, state, results and cancellation. Authenticated tunnel endpoints are private host transport. The public `POST /v1/threads/{id}/browser/tool` executes a named browser tool through normal runtime permissions and persists its result. It returns `output`, `metadata`, and `isError`; screenshots appear in `metadata.attachments`.

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

The catalog is generated from the same bounded schemas used by Desktop:

```sh
cd apps/desktop
node --import tsx scripts/browser-contract.ts
npm run typecheck
npm run test:e2e -- browser.spec.ts
```

The Electron suite uses disposable Git repositories and actual native Chromium views. Optional live verification uses a real model and an attached tmux TUI: `HAARNESS_BROWSER_LIVE=1`. Local test credentials stay in process memory. Set `HAARNESS_BROWSER_EVIDENCE` to retain test artifacts.

The broader natural-language suite submits 27 prompts across 24 scenarios through Desktop chat and an attached tmux TUI. It verifies actual page state, transferred bytes, screenshots, diagnostics and persisted tool results. With an OpenAI API key available:

```sh
HAARNESS_BROWSER_PROMPTS=1 HAARNESS_BROWSER_EVIDENCE=/tmp/browser-language \
  npm --prefix apps/desktop run test:e2e -- browser-prompts.spec.ts
```

It defaults to `gpt-5.6-luna`, which both an API key and a ChatGPT sign-in can run; set `HAARNESS_BROWSER_MODEL` to test another compatible OpenAI model. Live runs make billable model requests.

Chromium control, diagnostics, profiling and proxy code include MIT-licensed components; see [third-party notices](../apps/desktop/src/main/browser/THIRD-PARTY-NOTICES.md).

The OpenAI Responses adapter explicitly preserves optional tool arguments instead of allowing automatic strict-mode schema normalization. Haarness still validates arguments locally before permission checks. See [OpenAI function calling](https://developers.openai.com/api/docs/guides/function-calling#strict-mode).

Run the checks above for current browser regression coverage.

## Browser settings

Open **Settings → Browser**, or search for a control in Settings or global search (**⌘K / Ctrl+K**). Search understands both labels and terms such as `cookies`, `localhost`, `downloads`, and `disable browser`. Selecting a result opens and focuses its setting. The same search catalog covers the other settings panes, shortcuts, and installed runtime resources.

**Agent browser control** enables or disables native browser tool execution for the connected project. The change applies immediately to Desktop and an attached TUI, including calls waiting for approval. In-progress browser work is cancelled when its policy changes; cancellation cannot undo an action already performed. Manual browser tabs and navigation remain available.

Choose **This project** or **Global default** before changing agent control or site rules. An explicit project policy overrides inherited settings. **Use inherited settings** removes that scope’s entire browser policy override. The pane displays the effective current-project setting; when editing Global, a project override may still keep that project enabled or disabled. Concurrent edits are revision checked rather than silently overwriting another client’s changes.

The remaining controls apply to this desktop:

- **Web link destination** and **Local URL destination** choose Haarness or the system browser for conversation links. Haarness opens links in the selected conversation or the project's new-chat browser; localhost still reaches its connected server. Without a connected project, links open in the system browser. Authorization links keep their explicit external-browser flow.
- **Show full URL** hides or shows the path, query, and fragment when the address bar is idle. Editing always reveals the full URL and preserves its navigation destination.
- **Browsing history** keeps the latest 1,000 visits per project on this desktop. Search, reopen a visit in the current conversation, or remove entries.
- **Download history** retains the latest 200 browser downloads per project. **Save as…** copies a completed download to a location you choose. **Ask where to save downloads** opens that save dialog automatically after completion. Removing a download removes its retained copy, not a separately saved file.
- **Clear browsing data** closes this project’s browser tabs and clears cookies, site storage, cache, browsing history, and retained downloads. Conversation attachments and files saved separately are preserved. Other projects’ browsing data is unaffected.

**Agent site permissions** has separate Browsing, Downloads, and Uploads choices: Always allow, Requires approval, or Block. Rules use exact HTTP(S) origins, including ports, such as `https://example.com` or `http://localhost:5173`. Paths, wildcard hosts, and embedded credentials are rejected. Downloads govern browser files exported to the agent; uploads also retain normal file-read and external-directory checks. Normal agent/tool permission restrictions still apply, even when a browser rule says Always allow. Requires approval remains an explicit prompt even if a prior session approval allowed the tool.

During an agent operation, browser requests to blocked sites are refused. A redirect or script request to an origin requiring new approval is refused until the agent navigates there directly and obtains authorization. Back/Forward navigation checks the destination, including cached pages. This policy governs native browser operations; it does not replace permissions for independent shell or HTTP tools.

Policies are stored under `browser.policy` in the existing scoped `haarness.json`/`haarness.jsonc` configuration files. The authenticated `manageResource("browser-settings", …)` API supports `get`, `set`, and `reset`; mutations require the revision returned by `get`. Servers advertise `browser-settings.v1`.

Logins remain temporary and isolated per conversation. Browser profiles/password import, autofill, camera/microphone controls, annotation workflows, WebMCP, and unrestricted CDP are not part of this settings release.

Settings → Browser also supports **Remember site approvals** and leading subdomain rules such as `https://*.example.com`. Remembering is off by default. See [browser approval behavior](PERMISSIONS-AND-EXECUTION.md#browser-approvals) for persistence scopes, origin matching and redirect boundaries.
