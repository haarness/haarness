# Haarness for Chrome

The Chrome extension that lets Haarness agents use the user's own Chrome, with their permission. Users install it with `haarness browser setup`, `/chrome` in the TUI or Settings → Browser → Chrome in Desktop; the user guide is [Use your Chrome](https://haarness.ai/docs/desktop/use-your-chrome).

This README is for working on the extension. The architecture, both wire protocols, setup paths, packaging and Web Store publishing are in [docs/CHROME-EXTENSION.md](../../docs/CHROME-EXTENSION.md); tool routing and Chrome settings are in [docs/BROWSER.md](../../docs/BROWSER.md).

## Layout

| Path | What |
| --- | --- |
| `manifest.config.ts` | Generates `manifest.json`: MV3, the pinned `key`, permissions, the version mapping |
| `src/core/` | Browser-neutral logic: registry, tab ownership, command queue, policy decisions, bridge messages |
| `src/platform/chrome/` | Chrome adapters: `page.ts` (the driver's `PageSurface`), `cdp.ts` (`CdpTransport` over `chrome.debugger`), tabs, groups, downloads |
| `src/` popup and welcome page | Solid UI: `popup.html` (toolbar button) and `welcome.html` (setup guide and options) |
| `tests/` | Unit tests with `chrome.*` fakes (`node --test`) |
| `dist/chrome/` | Build output, the folder users load (gitignored) |

<!-- verify: exact file names once the package lands -->

The extension runs the shared browser driver from [`packages/browser-driver`](../../packages/browser-driver) (installed as `file:../../packages/browser-driver`, with `resolve.dedupe: ["effect"]`), the same page core Desktop uses. Use the driver's exported codecs (`decodeHostCommand`, `encodeHostOutcome`, …) and never pass a driver schema to this package's own `Schema.*`: two copies of `effect` do not recognise each other's schemas, and a guard test fails on `Schema.<fn>(Browser.…)` here.

## Build

From the repository root:

```sh
npm run extension:install    # npm ci for the driver and this package
npm run extension:build      # typecheck + vite build → apps/browser-extension/dist/chrome
```

or inside this folder: `npm ci`, `npm run build`, `npm run typecheck`. The build also writes per-language string subsets to `dist/chrome/locales/` from the repository's `locales/*.json`.

## Load it in Chrome

1. Build the CLI and the extension: `cargo build -p haarness-cli` and `npm run extension:build`.
2. Register the native host: `target/debug/haarness browser setup`. A `target/` binary finds `apps/browser-extension/dist/chrome`, copies it to `<data>/browser/chrome/Haarness for Chrome/` and prints the path.
3. Open `chrome://extensions`, turn on **Developer mode**, click **Load unpacked**, press ⌘⇧G (Ctrl+L on Linux) and paste that path. Pin **Haarness** to the toolbar.
4. Open a project with the same binary (`target/debug/haarness` in a project folder) and ask for a site "in Chrome". The popup should say **Connected**.

After a rebuild, run setup again (it refreshes the copy when the version differs) or reload the extension on `chrome://extensions`. To keep a development build away from your everyday Chrome and Haarness data, use a separate Chrome profile (`--user-data-dir`), an isolated `HAARNESS_DATA_DIR`, and `haarness browser setup --manifest-dir <that profile>/NativeMessagingHosts`. `haarness browser remove --all` undoes a setup.

Logs: the service worker's console (`chrome://extensions` → **Inspect views: service worker**) and the bridge log at `<data>/logs/browser-host.log`.

## Test

```sh
npm test                      # unit tests: chunks, registry, groups, queue, Fetch decisions, downloads, version mapping, i18n keys
npm run typecheck
npm run test:e2e              # Playwright with Chromium and a real haarness binary
```

The end-to-end suite launches Chromium with `--load-extension`, registers the host inside the temporary profile with `haarness browser setup --manifest-dir <profile>/NativeMessagingHosts`, and starts a daemon in a disposable workspace with an isolated `HAARNESS_DATA_DIR`. It needs Playwright's Chromium (`npx playwright install chromium`) or `HAARNESS_E2E_CHROMIUM`, and a `HAARNESS_BIN` copied out of `target/` so a parallel build cannot replace it mid-run. Branded Google Chrome ignores `--load-extension`, and the headless shell runs no extensions; see [docs/CHROME-EXTENSION.md](../../docs/CHROME-EXTENSION.md#end-to-end).

## Rules

- Only the service worker calls `connectNative`. Register every `chrome.*` listener synchronously at top level, and check `sender.id === chrome.runtime.id` on messages.
- Never focus a window from an agent command; only the popup's **Show tabs** may.
- Never fetch from the network, add content scripts, `web_accessible_resources` or `externally_connectable`, or widen permissions without updating the Web Store justifications in the docs.
- UI strings go through `t("English text")` with the key present in all 17 `locales/*.json`; CSS uses logical properties so Arabic works right to left.
- Keep the code that is adapted from MIT-licensed sources marked with its header and covered by the third-party notices shipped in `dist`.
