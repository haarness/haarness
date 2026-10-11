/**
 * Chromium with the built extension (dist/chrome) loaded unpacked, in a fresh
 * profile whose NativeMessagingHosts folder points `ai.haarness.browser` at
 * the fake host. Nothing is written outside the test's temporary folders.
 *
 * Browser: $HAARNESS_E2E_CHROMIUM, else a local Chrome for Testing from
 * Playwright's cache, else Playwright's own `chromium` channel (CI runs
 * `npx playwright install chromium`). Headless uses Chrome's new headless
 * mode, which runs extensions.
 */
import { test as base, chromium, type BrowserContext, type Page, type Worker } from "@playwright/test";
import { chmodSync, existsSync, mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { createServer, type Server } from "node:http";
import { homedir, tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { FakeBridge } from "./bridge";

const here = dirname(fileURLToPath(import.meta.url));
export const EXTENSION = resolve(here, "../../dist/chrome");
export const EXTENSION_ID = "accdjhmcgljbadpeanajllmcembhkomn";
const HOST = resolve(here, "fake-host.py");
const LOCAL_CHROMIUM = join(
  homedir(),
  "Library/Caches/ms-playwright/chromium-1200/chrome-mac-arm64/Google Chrome for Testing.app/Contents/MacOS/Google Chrome for Testing",
);

export interface Site {
  origin: string;
  /** The same server under another origin (localhost instead of 127.0.0.1). */
  other: string;
}

interface Fixtures {
  /** Write the native host manifest before Chrome starts (as `haarness browser setup` would). */
  setUp: boolean;
  bridge: FakeBridge;
  site: Site;
  /** Chrome's user data folder; its NativeMessagingHosts folder is where Chrome looks first. */
  profile: string;
  installHost(): void;
  context: BrowserContext;
  worker: Worker;
  openExtensionPage(path: string, options?: { width?: number; height?: number; dark?: boolean }): Promise<Page>;
}

export const test = base.extend<Fixtures>({
  setUp: [true, { option: true }],
  bridge: async ({}, use) => {
    const bridge = await new FakeBridge().listen();
    await use(bridge);
    await bridge.close();
  },
  site: async ({}, use) => {
    const server: Server = createServer((request, response) => {
      const url = new URL(request.url ?? "/", "http://placeholder");
      if (url.pathname === "/redirect") {
        response.writeHead(302, { location: url.searchParams.get("to") ?? "/" });
        return response.end();
      }
      const title = url.pathname === "/" ? "Acme home" : url.pathname.slice(1).replace(/^\w/, (c) => c.toUpperCase());
      response.writeHead(200, { "content-type": "text/html; charset=utf-8" });
      response.end(`<!doctype html><title>${title}</title><h1>${title}</h1><button>Go</button>`);
    });
    await new Promise<void>((done) => server.listen(0, "127.0.0.1", done));
    const address = server.address();
    const port = typeof address === "object" && address ? address.port : 0;
    await use({ origin: `http://127.0.0.1:${port}`, other: `http://localhost:${port}` });
    server.closeAllConnections();
    await new Promise((done) => server.close(done));
  },
  profile: async ({}, use) => {
    const directory = mkdtempSync(join(tmpdir(), "haarness-chrome-e2e-"));
    await use(directory);
    rmSync(directory, { recursive: true, force: true });
  },
  installHost: async ({ profile }, use) => {
    await use(() => {
      chmodSync(HOST, 0o755);
      mkdirSync(join(profile, "NativeMessagingHosts"), { recursive: true });
      writeFileSync(
        join(profile, "NativeMessagingHosts", "ai.haarness.browser.json"),
        JSON.stringify({
          name: "ai.haarness.browser",
          description: "Haarness for Chrome (test bridge)",
          path: HOST,
          type: "stdio",
          allowed_origins: [`chrome-extension://${EXTENSION_ID}/`],
        }),
      );
    });
  },
  context: async ({ setUp, bridge, profile, installHost }, use, info) => {
    if (!existsSync(join(EXTENSION, "manifest.json")))
      throw new Error("Build the extension first: npm --prefix apps/browser-extension run build");
    if (setUp) installHost();
    const executablePath = process.env.HAARNESS_E2E_CHROMIUM || (existsSync(LOCAL_CHROMIUM) ? LOCAL_CHROMIUM : undefined);
    const context = await chromium.launchPersistentContext(profile, {
      ...(executablePath ? { executablePath } : { channel: "chromium" }),
      headless: !process.env.HAARNESS_E2E_HEADED,
      args: [
        `--disable-extensions-except=${EXTENSION}`,
        `--load-extension=${EXTENSION}`,
        // Chrome's "started debugging this browser" bar takes 52 px from every tab; tests do not need it.
        "--silent-debugger-extension-api",
      ],
      env: {
        ...process.env,
        HAARNESS_E2E_RELAY: `127.0.0.1:${bridge.port}`,
        HAARNESS_E2E_LOG: info.outputPath("host.log"),
      },
    });
    await use(context);
    await context.close();
  },
  worker: async ({ context }, use) => {
    const worker =
      context.serviceWorkers().find((candidate) => candidate.url().startsWith(`chrome-extension://${EXTENSION_ID}/`)) ??
      (await context.waitForEvent("serviceworker", {
        predicate: (candidate) => candidate.url().startsWith(`chrome-extension://${EXTENSION_ID}/`),
        timeout: 15_000,
      }));
    await use(worker);
  },
  openExtensionPage: async ({ context, worker }, use) => {
    void worker;
    await use(async (path, options = {}) => {
      const page = await context.newPage();
      await page.setViewportSize({ width: options.width ?? 360, height: options.height ?? 600 });
      if (options.dark !== undefined) await page.emulateMedia({ colorScheme: options.dark ? "dark" : "light" });
      await page.goto(`chrome-extension://${EXTENSION_ID}/${path}`);
      return page;
    });
  },
});

export const expect = test.expect;

/** Reads the worker's host state (a JSON-safe projection) for assertions. */
export function snapshot(worker: Worker) {
  return worker.evaluate(() => (globalThis as unknown as { haarness: { snapshot(): unknown } }).haarness.snapshot()) as Promise<{
    connection: string;
    pill: string;
    paused: boolean;
    stopped: boolean;
    chats: { key: string; title: string; tabs: number; working: boolean; refused: string | null }[];
    projects: unknown[];
  }>;
}
