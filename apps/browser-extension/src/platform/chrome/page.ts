/**
 * The Chrome side of the shared page driver (`PageSurface`): what a page needs
 * from the browser beyond CDP. The driver asks synchronously for the tab's
 * URL, title and history, so those are cached from `chrome.tabs` events and
 * `Page.getNavigationHistory`.
 */
import type { ProtocolMapping } from "devtools-protocol/types/protocol-mapping.js";
import type { Method } from "../../core/browser";
import { capturePlan } from "../../core/capture";
import type { PageSurface } from "../../core/driver-contract";
import { abortable } from "./wait";
import { HostError } from "../../core/errors";
import { supportedInChrome } from "../../core/policy";
import type { ChromeTransport } from "./cdp";

type History = ProtocolMapping.Commands["Page.getNavigationHistory"]["returnType"];

export interface TabCache {
  url: string;
  title: string;
  loading: boolean;
  zoom: number;
}

export interface SurfaceDeps {
  cache: TabCache;
  load(url: string, signal: AbortSignal): Promise<void>;
  /** Registers work to run once the current command ends. */
  after(work: () => Promise<void>): void;
  cursor(): boolean;
  mainFrame(): Promise<string>;
}

export function createSurface(transport: ChromeTransport, deps: SurfaceDeps): PageSurface & { dispose(): void } {
  const tabId = transport.tabId;
  let history: History | undefined;
  let world: { frame: string; context: number } | undefined;
  const refresh = () =>
    void transport.send("Page.getNavigationHistory").then(
      (value) => (history = value),
      () => {},
    );
  const offs = [
    transport.on("Page.frameNavigated", (event, session) => {
      if (session || event.frame.parentId) return;
      world = undefined;
      refresh();
    }),
    transport.on("Page.navigatedWithinDocument", (_event, session) => {
      if (!session) refresh();
    }),
  ];
  refresh();
  const entry = (delta: -1 | 1) => history?.entries[history.currentIndex + delta];
  const index = (delta: -1 | 1) => (history ? history.currentIndex + delta : delta);

  const surface: PageSurface & { dispose(): void } = {
    url: () => deps.cache.url,
    title: () => deps.cache.title,
    loading: () => deps.cache.loading,
    canGoBack: () => !!entry(-1),
    canGoForward: () => !!entry(1),
    historyEntry: (delta) => ({ index: index(delta), url: entry(delta)?.url }),
    navigate: (url, signal) => deps.load(url, signal),
    back: () => chrome.tabs.goBack(tabId).catch(() => {}),
    forward: () => chrome.tabs.goForward(tabId).catch(() => {}),
    reload: () => transport.send("Page.reload").then(() => {}, () => {}),
    stop: () => transport.send("Page.stopLoading").then(() => {}, () => {}),
    async viewport() {
      const metrics = await transport.send("Page.getLayoutMetrics");
      return { width: metrics.cssVisualViewport.clientWidth, height: metrics.cssVisualViewport.clientHeight };
    },
    async pixelRatio() {
      const result = await transport.send("Runtime.evaluate", { expression: "devicePixelRatio", returnByValue: true });
      return typeof result.result.value === "number" ? result.result.value : 1;
    },
    async readyForCapture(signal) {
      const [tab, focused] = await Promise.all([chrome.tabs.get(tabId), chrome.windows.getLastFocused().catch(() => undefined)]);
      const plan = capturePlan(tab, focused?.focused ? focused.id : undefined);
      if (plan === "ready") return;
      if (plan === "refuse") throw new HostError("background-tab");
      const [previous] = await chrome.tabs.query({ windowId: tab.windowId, active: true });
      await chrome.tabs.update(tabId, { active: true });
      deps.after(async () => {
        if (previous?.id !== undefined) await chrome.tabs.update(previous.id, { active: true }).catch(() => {});
      });
      // A tab that just became visible paints on its next frame.
      await abortable(
        transport.send("Runtime.evaluate", {
          expression: "new Promise((done) => requestAnimationFrame(() => requestAnimationFrame(done)))",
          awaitPromise: true,
        }),
        signal,
        1_500,
      ).catch(() => {});
    },
    selectAll: { commands: ["selectAll"] },
    supports: (method: Method) => supportedInChrome(method),
    gateTopDocumentOnly: true,
    resourceOrigins: "securityOrigin",
    wwwTwin: true,
    cursor: {
      async run(code) {
        if (!deps.cursor()) return;
        try {
          const frame = await deps.mainFrame();
          if (world?.frame !== frame) {
            const created = await transport.send("Page.createIsolatedWorld", { frameId: frame, worldName: "haarness-cursor" });
            world = { frame, context: created.executionContextId };
          }
          await transport.send("Runtime.evaluate", { expression: code, contextId: world.context });
        } catch {
          // The cursor is decoration; a page mid-navigation must never fail the action it shows.
          world = undefined;
        }
      },
      zoom: () => deps.cache.zoom || 1,
    },
    dispose() {
      for (const off of offs) off();
    },
  };
  return surface;
}
