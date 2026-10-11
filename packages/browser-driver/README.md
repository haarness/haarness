# @haarness/browser-driver

The Chromium page driver behind Haarness's `browser_*` tools, shared by its two
browser hosts:

- **Haarness Desktop** (`apps/desktop`) drives its isolated Haarness browser:
  Electron `WebContentsView`s whose traffic goes through the connected server.
- **Haarness for Chrome** (`apps/browser-extension`) drives tabs in the user's
  own Chrome through `chrome.debugger`.

Both speak the same tool contract to the Haarness server and run the same code
for everything a tool does through the Chrome DevTools Protocol (CDP). Each host
only adapts its browser to two small interfaces.

The package is private and source-only: `exports` maps
`@haarness/browser-driver/<module>` to `src/<module>.ts`, and each consumer
compiles it with its own TypeScript and bundler. It has no runtime
dependencies of its own; `effect` and `devtools-protocol` are peer
dependencies, pinned to the exact versions Desktop uses.

## Modules

| Module | What it holds |
| --- | --- |
| `rpc` | The `Browser` namespace: every tool's input and output schema (`Operations`), `Command`/`Result`/`Outcome` (server ↔ host), `HostCommand`/`HostResult`/`HostOutcome` and `FileRef` (Chrome bridge ↔ extension), and codecs bound to this package's `effect`. |
| `types` | The host boundary: `CdpTransport`, `PageSurface`, `FileStore`, `FileInput`, `PageOptions`, `Page`. |
| `page` | `createPage(surface, cdp, options)`: snapshots and element refs, input, frames, dialogs, evaluation, waits, screenshots, uploads, diagnostics and the permission targets of every page tool. `lastRef()` reads the ref counter. |
| `cdp` | `createCdpEvents` (the event fan-out and iframe-session filter both transports use), `abortError`, `waitFor`. |
| `errors` | `protocolError` (CDP failures with a recovery hint), `browserFailure` (an error as a tool outcome) and `BrowserCodeError` (a failure with a host code such as `unsupported`). Hints default to the Haarness browser's wording; pass `"chrome"` for Chrome's. |
| `policy` | `normalizeURL`, `destinationOrigin`, `browserSiteAccess` (site rules), `wwwTwin` and `navigationResources` (the permission target of loading a URL, for a host's `tabs.open`). |
| `diagnostics` | Console and network capture for `browser_console` and `browser_network_*`, with optional request-body omission and extra header redaction (`isSensitiveHeader`). |
| `analysis` | Trace, CPU profile and heap snapshot analysis (Desktop's profiling tools). |
| `agent-cursor` | The in-page agent cursor (`cursorScript`), its labels (`cursorLabel`, `cursorLabelKeys`) and `createAgentCursor`. |

`scripts/contract.ts` generates the server's tool catalog,
`crates/haarness-server/src/browser-tools.json`, from `Operations`.

## One `effect` per consumer

The driver imports `effect` from its own `node_modules` when a consumer's
tests run it through `tsx`, while the consumer's own code uses the consumer's
copy. Schemas from two `effect` instances cannot decode each other's values, so
**never pass a `Browser.*` schema to your own `Schema.*`**. Use the codecs in
`rpc` instead (`decodeCommand`, `encodeOutcome`, `decodeHostCommand`,
`encodeHostOutcome`, `decodeAction`, `decodeState`, `decodeTunnelRead`,
`decodeTunnelTarget`, …). Desktop and the extension each have a guard test that
fails on `Schema.<fn>(Browser.` outside this package.

In bundles there is one copy: Desktop externalises `effect` (a production
dependency) and sets `resolve.dedupe: ["effect"]`; the extension bundles with
the same dedupe.

## Hosting a page

```ts
import { createPage } from "@haarness/browser-driver/page";

const page = createPage(surface, cdp, {
  id,                      // Browser.TabID
  publish: () => sendInventory(),
  files,                   // FileStore: captures and downloads
  input,                   // FileInput: uploads to local paths
  translate,               // cursor labels in the interface language
});
await page.ready;
// From the browser's main-frame, cross-document navigation-start event:
page.navigationStarted();
// For each server command (already decoded):
const result = await page.execute(command, signal);
```

A host keeps tab management (`tabs.list/open/focus/close`), its connection to
the server, site-policy enforcement on the network and the tab's lifetime. The
driver never closes a tab or disposes the transport.

`PageSurface` choices per host:

| | Desktop | Chrome |
| --- | --- | --- |
| `resourceOrigins` | `"url"` | `"securityOrigin"` |
| `gateTopDocumentOnly` | `false` | `true` |
| `wwwTwin` | off | on |
| `selectAll` | `{keys: "Meta+A"}` (macOS) / `"Control+A"` | `{commands: ["selectAll"]}` |
| `supports` | every tool | not `lighthouse`, `heap.*`, `cpu.*`, `trace.*` |
| `cursor` | `executeJavaScriptInIsolatedWorld` | `Page.createIsolatedWorld` |
| `FileInput.stage` | writes bytes to its temp store | accepts staged `FileRef` paths |
| `diagnostics` | defaults | `{requestBodies: false, redactHeader: isSensitiveHeader}` |

Desktop runs its Node- and Electron-bound tools (tracing, CPU and heap
profiling, Lighthouse) through `options.extension`.

## Development

```sh
npm --prefix packages/browser-driver ci
npm --prefix packages/browser-driver run typecheck
npm --prefix packages/browser-driver test
# Regenerate the server's tool catalog after changing a schema or description,
# or check that it is current (CI does):
npm --prefix packages/browser-driver run contract
npm --prefix packages/browser-driver run contract:check
```

Install the driver before Desktop or the extension: their `npm ci` links it.

Parts of this package are adapted from MIT-licensed code; see
[THIRD-PARTY-NOTICES.md](THIRD-PARTY-NOTICES.md).
