# Haarness for Chrome

The Chrome extension that lets Haarness agents use the user's own Chrome, with their permission. Users install it with `haarness browser setup`, `/chrome` in the TUI or Settings → Browser → Chrome in Desktop; the user guide is [Use your Chrome](https://haarness.ai/docs/desktop/use-your-chrome).

This README is for working on the extension. The architecture, both wire protocols, setup paths, packaging and Web Store publishing are in [docs/CHROME-EXTENSION.md](../../docs/CHROME-EXTENSION.md); tool routing and Chrome settings are in [docs/BROWSER.md](../../docs/BROWSER.md).

## Layout

| Path | What |
| --- | --- |
| `manifest.config.ts` | Generates `manifest.json`: MV3, the pinned `key`, permissions, the version mapping |
| `src/background.ts` | The service worker: wires the Chrome platform into the host |
| `src/core/` | Browser-neutral logic: `host.ts` (commands, tab ownership, pause and stop), `registry.ts`, `queue.ts`, `policy.ts` and `fetch-policy.ts` (site decisions), `bridge.ts` and `protocol.ts` (bridge messages), `driver.ts` (the only import of the driver's page) |
| `src/platform/chrome/` | Chrome adapters: `page.ts` (the driver's `PageSurface`), `cdp.ts` (`CdpTransport` over `chrome.debugger`), `session.ts` (the per-tab debugger session and its `Fetch` guard), `files.ts`, `downloads.ts`, `wait.ts` (timeouts and cancellation), `index.ts` (tabs, groups, storage) |
| `src/ui/`, `src/popup.html`, `src/welcome.html` | Solid UI: the popup (toolbar button) and the welcome page (setup guide and options) |
| `tests/` | Unit tests with `chrome.*` fakes (`node --test`) |
| `e2e/` | Playwright: `bridge.spec.ts` and `ui.spec.ts` (fake bridge), `stack.spec.ts` (real haarness) |
| `dist/chrome/` | Build output, the folder users load (gitignored) |

The extension runs the shared browser driver from [`packages/browser-driver`](../../packages/browser-driver) (installed as `file:../../packages/browser-driver`, with `resolve.dedupe: ["effect", "solid-js"]`), the same page core Desktop uses. Use the driver's exported codecs (`decodeHostCommand`, `encodeHostOutcome`, …) and never pass a driver schema to this package's own `Schema.*`: two copies of `effect` do not recognise each other's schemas, and a guard test fails on `Schema.<fn>(Browser.…)` here.

## Build

From the repository root:

```sh
npm run extension:install    # ci + build the TypeScript SDK (e2e types), then npm ci for the driver and this package
npm run extension:build      # typecheck + vite build → apps/browser-extension/dist/chrome
```

or inside this folder: `npm ci`, `npm run build`, `npm run typecheck` (install the driver and build `sdks/typescript` first). The build also writes per-language string subsets to `dist/chrome/locales/` from the repository's `locales/*.json`, and copies the driver's `THIRD-PARTY-NOTICES.md`.

## Load it in Chrome

1. Build the CLI and the extension: `cargo build -p haarness-cli` and `npm run extension:build`.
2. Register the native host: `target/debug/haarness browser setup`. A `target/` binary finds `apps/browser-extension/dist/chrome`, copies it to `<data>/browser/chrome/Haarness for Chrome/` and prints the path.
3. Open `chrome://extensions`, turn on **Developer mode**, click **Load unpacked**, press ⌘⇧G (Ctrl+L on Linux; on Windows paste into the folder box) and paste that path. Pin **Haarness** to the toolbar.
4. Open a project with the same binary (`target/debug/haarness` in a project folder) and ask for a site "in Chrome". The popup should say **Connected**.

Setup copies the extension again only when its `version_name` (the workspace version) changes, and a rebuild does not change it, so reloading the copy on `chrome://extensions` after a rebuild still runs the old build. While iterating, load `apps/browser-extension/dist/chrome` itself with **Load unpacked** and reload it after each `npm run extension:build`, or run `haarness browser remove --all` and then setup again for a fresh copy. To keep a development build away from your everyday Chrome and Haarness data, use a separate Chrome profile (`--user-data-dir`), an isolated `HAARNESS_DATA_DIR`, and `haarness browser setup --manifest-dir <that profile>/NativeMessagingHosts` (setup refuses an isolated data folder without a manifest folder). `haarness browser remove --all` undoes a setup.

Logs: the service worker's console (`chrome://extensions` → **Inspect views: service worker**) and the bridge log at `<data>/logs/browser-host.log`.

## Test

```sh
npm test                      # unit tests: host, approvals, chunks, registry, queue, site and Fetch decisions, downloads, CDP, the driver boundary, version mapping, i18n keys
npm run typecheck
npm run test:e2e              # Playwright with Chromium: the fake-bridge project, then the full-stack project
npx playwright test --project full-stack   # only the real stack
```

To test released files instead of a build, set `HAARNESS_E2E_EXTENSION` to the unzipped `haarness-chrome-<version>.zip` and `HAARNESS_BIN` to the released CLI: setup then copies that folder, and every test drives it.

The `fake-bridge` project plays the bridge and servers itself. The `full-stack` project registers the host inside the temporary profile with `haarness browser setup --manifest-dir <profile>/NativeMessagingHosts`, launches Chromium with `--load-extension` on the copy setup made (the reload test loads it over CDP `Extensions.loadUnpacked` instead, like Load unpacked), starts a daemon in a disposable git project with an isolated `HAARNESS_DATA_DIR` and `HOME`, and drives every tool through the TypeScript SDK (build it first: `npm --prefix ../../sdks/typescript run build`). It needs Playwright's Chromium (`npx playwright install chromium`) or `HAARNESS_E2E_CHROMIUM`, and `HAARNESS_BIN` copied out of `target/` so a parallel build cannot replace it mid-run (without it, the newest `target/` build is used). Branded Google Chrome ignores `--load-extension`, and the headless shell runs no extensions; see [docs/CHROME-EXTENSION.md](../../docs/CHROME-EXTENSION.md#end-to-end).

## Rules

- Only the service worker calls `connectNative`. Register every `chrome.*` listener synchronously at top level, and check `sender.id === chrome.runtime.id` on messages.
- Never focus a window from an agent command; only the popup's **Show tabs** may.
- Never fetch from the network, add content scripts, `web_accessible_resources` or `externally_connectable`, or widen permissions without updating the Web Store justifications in the docs.
- UI strings go through `t("English text")` with the key present in all 17 `locales/*.json`; CSS uses logical properties so Arabic works right to left.
- Keep the code that is adapted from MIT-licensed sources marked with its header and covered by the third-party notices shipped in `dist`.
