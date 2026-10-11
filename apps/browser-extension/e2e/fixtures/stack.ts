/**
 * The whole Haarness for Chrome stack, for real: a haarness daemon in a
 * throwaway git project, `haarness browser setup` registering the bridge in a
 * throwaway Chromium profile, the built extension in that Chromium, and the
 * TypeScript SDK playing the client (tool calls, approvals, turns).
 *
 * Isolation: every test gets its own HAARNESS_DATA_DIR and HOME (the bridge
 * looks for servers under HOME's default data root too), its own project and
 * its own Chromium profile, and stops what it started.
 *
 * Binary: $HAARNESS_BIN (copy it out of target/, since the bridge exits when
 * the file it runs changes), else target/debug or target/release.
 */
import { test as base, chromium, type BrowserContext, type Page, type Worker } from "@playwright/test";
import { ApiError, Haarness, type BrowserToolResponse, type JsonValue, type PendingPermissionInfo, type PermissionReplyDecision, type Thread } from "@haarness/sdk";
import { execFileSync, spawn } from "node:child_process";
import { existsSync, mkdirSync, mkdtempSync, realpathSync, rmSync, statSync, writeFileSync } from "node:fs";
import { homedir, tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { EXTENSION, EXTENSION_ID } from "./extension";
import { startSite, type TestSite } from "./site";

const here = dirname(fileURLToPath(import.meta.url));
const repository = resolve(here, "../../../..");
const LOCAL_CHROMIUM = join(
  homedir(),
  "Library/Caches/ms-playwright/chromium-1200/chrome-mac-arm64/Google Chrome for Testing.app/Contents/MacOS/Google Chrome for Testing",
);

/** Runs the binary with `input` on stdin and resolves with its stdout. */
function output(bin: string, args: string[], env: Record<string, string>, input: string) {
  return new Promise<string>((resolve, reject) => {
    const child = spawn(bin, args, { env, stdio: ["pipe", "pipe", "pipe"] });
    let stdout = "";
    let stderr = "";
    const timer = setTimeout(() => child.kill(), 60_000);
    child.stdout.on("data", (chunk) => (stdout += chunk));
    child.stderr.on("data", (chunk) => (stderr += chunk));
    child.on("error", reject);
    child.on("close", (code) => {
      clearTimeout(timer);
      if (code === 0) resolve(stdout);
      else reject(new Error(`${args.join(" ")} exited with ${code}: ${stderr.slice(-2_000)}`));
    });
    child.stdin.end(input);
  });
}

/** The haarness binary under test, or undefined when none was built. */
export function haarnessBinary(): string | undefined {
  if (process.env.HAARNESS_BIN) return resolve(process.env.HAARNESS_BIN);
  const built = ["target/debug/haarness", "target/release/haarness"]
    .map((path) => join(repository, path))
    .filter((path) => existsSync(path))
    .sort((a, b) => statSync(b).mtimeMs - statSync(a).mtimeMs);
  return built[0];
}

export interface ToolCall {
  /** The tool's answer, once every prompt it raised was answered. */
  done: Promise<BrowserToolResponse>;
}

export interface Stack {
  bin: string;
  root: string;
  env: Record<string, string>;
  project: string;
  profile: string;
  downloads: string;
  site: TestSite;
  api: Haarness;
  context: BrowserContext;
  worker: Worker;
  /** A new chat in the project. */
  thread(title?: string): Promise<Thread>;
  /** Calls a browser tool and fails the test if it asks the user anything. */
  tool(thread: Thread, name: string, args?: Record<string, unknown>): Promise<BrowserToolResponse>;
  /** Starts a browser tool call that may ask; answer with `answer`. */
  call(thread: Thread, name: string, args?: Record<string, unknown>): ToolCall;
  /** Waits for the chat's next Chrome approval and answers it. */
  answer(thread: Thread, decision: PermissionReplyDecision): Promise<PendingPermissionInfo>;
  /** The chat's waiting approvals right now. */
  prompts(thread: Thread): Promise<PendingPermissionInfo[]>;
  /** Starts the project's daemon (again) and points `api` at it. */
  startDaemon(): Promise<void>;
  stopDaemon(): void;
  /** Starts Chromium with the extension (again). */
  launch(): Promise<void>;
  close(): Promise<void>;
  /** Resolves once Chrome is connected to the daemon (and not paused). */
  connected(): Promise<void>;
  /** The extension's host, as `globalThis.haarness` in the service worker. */
  host<T>(fn: (host: any, arg: any) => T | Promise<T>, arg?: unknown): Promise<T>;
  /** A Playwright handle on the Chrome tab behind a Haarness tab id. */
  page(tabID: string): Promise<Page>;
}

export interface StackOptions {
  /** `.haarness/config.toml` of the project. */
  projectConfig: string;
  /**
   * How Chromium loads the extension: from the command line (fast; Chrome
   * disables it if it reloads itself), or over CDP like Load unpacked.
   */
  install: "command-line" | "unpacked";
}

export const ECHO_CONFIG = '[defaults]\nprovider = "echo"\nmodel = "echo-1"\n';

export const test = base.extend<StackOptions & { stack: Stack }>({
  projectConfig: [ECHO_CONFIG, { option: true }],
  install: ["command-line", { option: true }],
  stack: async ({ projectConfig, install }, use, info) => {
    const bin = haarnessBinary();
    test.skip(!bin, "Build haarness (cargo build -p haarness-cli) or set HAARNESS_BIN to run the full-stack tests.");
    if (!existsSync(join(EXTENSION, "manifest.json")))
      throw new Error("Build the extension first: npm --prefix apps/browser-extension run build");
    const root = realpathSync(mkdtempSync(join(tmpdir(), "haarness-chrome-stack-")));
    const project = join(root, "project");
    const profile = join(root, "profile");
    const downloads = join(root, "downloads");
    for (const directory of [project, profile, downloads, join(root, "home"), join(project, ".haarness")])
      mkdirSync(directory, { recursive: true });
    writeFileSync(join(project, ".haarness/config.toml"), projectConfig);
    writeFileSync(join(project, "notes.txt"), "Upload me 日本\n");
    for (const args of [
      ["init", "-q", "-b", "main"],
      ["add", "."],
      ["-c", "user.name=Chrome Fixture", "-c", "user.email=fixture@example.test", "commit", "-q", "-m", "Fixture"],
    ])
      execFileSync("git", args, { cwd: project, stdio: "ignore" });

    const env: Record<string, string> = {};
    for (const [key, value] of Object.entries(process.env))
      if (value !== undefined && !/^(HAARNESS_|XDG_)/.test(key)) env[key] = value;
    Object.assign(env, {
      HOME: join(root, "home"),
      HAARNESS_DATA_DIR: join(root, "state"),
      HAARNESS_TEST_PROVIDER: "1",
      HAARNESS_BROWSER_EXTENSION_DIR: EXTENSION,
      HAARNESS_LOG: "debug",
    });

    const setup = JSON.parse(
      execFileSync(bin!, ["browser", "setup", "--manifest-dir", join(profile, "NativeMessagingHosts"), "--json"], {
        env,
        encoding: "utf8",
        timeout: 60_000,
      }),
    );
    if (!setup.browsers?.some((entry: { registered: boolean }) => entry.registered))
      throw new Error(`haarness browser setup did not register the host: ${JSON.stringify(setup)}`);
    const extension: string = setup.extension?.path;
    if (!extension || !existsSync(join(extension, "manifest.json")))
      throw new Error(`haarness browser setup did not copy the extension: ${JSON.stringify(setup)}`);

    const site = await startSite();
    let api: Haarness | undefined;
    let context: BrowserContext | undefined;
    let worker: Worker | undefined;
    let daemon = false;

    const stack: Stack = {
      bin: bin!,
      root,
      env,
      project,
      profile,
      downloads,
      site,
      get api() {
        if (!api) throw new Error("The daemon is not running");
        return api;
      },
      get context() {
        if (!context) throw new Error("Chromium is not running");
        return context;
      },
      get worker() {
        if (!worker) throw new Error("Chromium is not running");
        return worker;
      },
      thread: (title = "Plan the trip") => stack.api.createThread({ title, provider: "echo", model: "echo-1" }),
      async tool(thread, name, args = {}) {
        const call = stack.call(thread, name, args);
        let settled = false;
        const watch = (async () => {
          while (!settled) {
            const waiting = await stack.prompts(thread).catch(() => []);
            if (waiting.length)
              throw new Error(`${name} asked for approval unexpectedly: ${JSON.stringify(waiting.map((entry) => entry.patterns))}`);
            await new Promise((resolve) => setTimeout(resolve, 100));
          }
        })();
        try {
          return await Promise.race([call.done, watch.then(() => call.done)]);
        } finally {
          settled = true;
        }
      },
      call(thread, name, args = {}) {
        const done = stack.api.callBrowserTool(thread.id, { name, arguments: args as JsonValue }, { timeoutMs: 120_000 }).catch((error: unknown) => {
          // A failed tool answers with an HTTP error; read it as the model would see it.
          if (error instanceof ApiError && error.message.startsWith("tool error: "))
            return { isError: true, output: error.message.slice("tool error: ".length), metadata: null } as BrowserToolResponse;
          throw error;
        });
        // A rejected call is read by the test; never an unhandled rejection meanwhile.
        done.catch(() => {});
        return { done };
      },
      async prompts(thread) {
        return (await stack.api.listPermissions()).filter((entry) => entry.threadId === thread.id);
      },
      async answer(thread, decision) {
        const deadline = Date.now() + 30_000;
        while (Date.now() < deadline) {
          const [prompt] = await stack.prompts(thread);
          if (prompt) {
            await stack.api.replyPermission(prompt.permissionId, { decision });
            return prompt;
          }
          await new Promise((resolve) => setTimeout(resolve, 100));
        }
        throw new Error(`No approval was asked in ${thread.title}`);
      },
      async startDaemon() {
        const endpoint = JSON.parse(
          await output(bin!, ["--workspace", project, "--trust-project", "desktop"], env, '{"action":"connect"}'),
        ) as { url: string; token: string };
        api = new Haarness({ baseUrl: endpoint.url, token: endpoint.token });
        daemon = true;
      },
      stopDaemon() {
        if (!daemon) return;
        daemon = false;
        api?.close?.();
        api = undefined;
        try {
          execFileSync(bin!, ["--workspace", project, "--trust-project", "serve", "--stop"], { env, stdio: "ignore", timeout: 30_000 });
        } catch {
          // Already gone.
        }
      },
      async launch() {
        // Developer mode is on, as unpacked installs require: without it Chrome
        // disables a command-line extension that reloads itself.
        const preferences = join(profile, "Default", "Preferences");
        if (!existsSync(preferences)) {
          mkdirSync(dirname(preferences), { recursive: true });
          writeFileSync(preferences, JSON.stringify({ extensions: { ui: { developer_mode: true } } }));
        }
        const executablePath = process.env.HAARNESS_E2E_CHROMIUM || (existsSync(LOCAL_CHROMIUM) ? LOCAL_CHROMIUM : undefined);
        // The copy `haarness browser setup` made, which is what users load.
        const unpacked = install === "unpacked";
        context = await chromium.launchPersistentContext(profile, {
          ...(executablePath ? { executablePath } : { channel: "chromium" }),
          headless: !process.env.HAARNESS_E2E_HEADED,
          viewport: { width: 1200, height: 800 },
          acceptDownloads: true,
          downloadsPath: downloads,
          ...(unpacked ? { ignoreDefaultArgs: ["--disable-extensions"] } : {}),
          args: [
            ...(unpacked
              ? ["--enable-unsafe-extension-debugging"]
              : [`--disable-extensions-except=${extension}`, `--load-extension=${extension}`]),
            "--silent-debugger-extension-api",
            // Every *.test name is the fixture site, so one port serves several origins.
            "--host-resolver-rules=MAP *.test 127.0.0.1",
          ],
          env,
        });
        // Like Load unpacked on chrome://extensions: an extension that can reload itself.
        if (unpacked) {
          const browser = await context.browser()!.newBrowserCDPSession();
          await browser.send("Extensions.loadUnpacked" as never, { path: extension } as never);
          await browser.detach();
        }
        // Leave the page's JavaScript dialogs to Haarness; Playwright would dismiss them.
        context.on("dialog", () => {});
        worker =
          context.serviceWorkers().find((candidate) => candidate.url().startsWith(`chrome-extension://${EXTENSION_ID}/`)) ??
          (await context.waitForEvent("serviceworker", {
            predicate: (candidate) => candidate.url().startsWith(`chrome-extension://${EXTENSION_ID}/`),
            timeout: 30_000,
          }));
        // HAARNESS_E2E_WORKER_LOG=1 prints the service worker's console.
        if (process.env.HAARNESS_E2E_WORKER_LOG) worker.on("console", (message) => console.log("[worker]", message.text()));
      },
      async close() {
        const current = context;
        context = undefined;
        worker = undefined;
        await current?.close().catch(() => {});
      },
      async connected() {
        const deadline = Date.now() + 45_000;
        let last: unknown;
        while (Date.now() < deadline) {
          last = await stack.api.listBrowserHosts().catch((error) => String(error));
          const hosts = (last as { hosts?: { kind: string; paused: boolean; stopped: boolean }[] }).hosts ?? [];
          if (hosts.some((host) => host.kind === "chrome" && !host.paused && !host.stopped)) return;
          await new Promise((resolve) => setTimeout(resolve, 200));
        }
        throw new Error(`Chrome never connected to the daemon: ${JSON.stringify(last)}`);
      },
      host(fn, arg) {
        return stack.worker.evaluate(
          ([source, value]) => {
            const call = new Function("host", "arg", `return (${source})(host, arg);`);
            return call((globalThis as unknown as { haarness: unknown }).haarness, value);
          },
          [fn.toString(), arg] as const,
        ) as never;
      },
      async page(tabID) {
        const tabId = await stack.host(
          (host, id) => host.debug.registry.all().flatMap((entry: any) => entry.tabs).find((tab: any) => tab.id === id)?.tabId,
          tabID,
        );
        if (typeof tabId !== "number") throw new Error(`No Chrome tab for ${tabID}`);
        const url = await stack.worker.evaluate((id) => chrome.tabs.get(id).then((tab) => tab.url ?? ""), tabId);
        const found = stack.context.pages().find((candidate) => candidate.url() === url);
        if (!found) throw new Error(`Playwright does not see the tab at ${url}`);
        return found;
      },
    };

    try {
      await stack.startDaemon();
      await stack.launch();
      await stack.connected();
      await use(stack);
    } finally {
      await stack.close();
      stack.stopDaemon();
      await site.close();
      if (info.status !== info.expectedStatus) {
        // Keep the bridge and daemon logs next to the failure.
        for (const [name, path] of [
          ["browser-host.log", join(root, "state/data/logs/browser-host.log")],
          ["daemon.log", join(root, "state/data/logs/haarness.log")],
        ])
          if (existsSync(path)) await info.attach(name, { path });
      }
      rmSync(root, { recursive: true, force: true });
    }
  },
});

export const expect = test.expect;

/** The JSON a browser tool answered with. */
export function value<T = any>(response: BrowserToolResponse): T {
  if (response.isError) throw new Error(`The tool failed: ${response.output}`);
  return JSON.parse(response.output) as T;
}
