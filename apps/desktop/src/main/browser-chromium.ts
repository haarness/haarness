// Includes MIT-licensed code; see browser/THIRD-PARTY-NOTICES.md.
// The Haarness browser's Electron side of a tab: its WebContentsView, session,
// network, popups, downloads and corners. Everything a tool does through the
// DevTools Protocol runs in the shared driver (@haarness/browser-driver/page).
import { Browser } from "@haarness/browser-driver/rpc";
import electron, { type BrowserWindow, type WebContents } from "electron";
import { waitFor } from "@haarness/browser-driver/cdp";
import { createPage } from "@haarness/browser-driver/page";
import { destinationOrigin, normalizeURL } from "@haarness/browser-driver/policy";
import type {
  Page,
  PageExtension,
  PageSurface,
} from "@haarness/browser-driver/types";
import { createCdp } from "./browser/cdp";
import { createBrowserFiles } from "./browser/files";
import { createProfiling } from "./browser/profiling";
import { createCornerImages } from "./browser/corners";
import { returnKeyboard } from "./browser-keyboard";
import { createPicker } from "./browser-picker";
import type { BrowserNetwork } from "./browser/network";
import type { PickRequest, PickedElement, PickEnd } from "../browser-pick";

/** Any stable id outside Electron's own worlds, for the agent cursor; one per page document. */
const CURSOR_WORLD = 1_779;
export type BrowserPage = ReturnType<typeof createBrowserPage>;

export function createBrowserPage(
  win: BrowserWindow,
  options: {
    id: Browser.TabID;
    partition: string;
    network: BrowserNetwork | null;
    publish: (error?: string) => void;
    fail: () => void;
    downloaded?: (file: { id: string; name: string; path: string; url: string; bytes: number; state: "completed" | "failed" }) => void;
    /** The page's icon candidates, for the Desktop tab strip only. */
    favicon?: (urls: string[]) => void;
    /** Whether the agent's cursor is drawn over the page while it acts. */
    agentCursor?: () => boolean;
    /** The interface language's text for an English key. */
    translate?: (key: string) => string;
    /** Which modifier keys the window's own page last heard down, for picking. */
    modifiers?: () => { shift: boolean; alt: boolean };
    popup: (options: Electron.BrowserWindowConstructorOptions) => WebContents;
    initialize?: boolean;
    restore?: Browser.Tab;
    popupOptions?: Electron.BrowserWindowConstructorOptions;
  },
) {
  const view = new electron.WebContentsView({
    ...options.popupOptions,
    webPreferences: {
      partition: options.partition,
      nodeIntegration: false,
      contextIsolation: true,
      sandbox: true,
      webSecurity: true,
      webviewTag: false,
      devTools: false,
      backgroundThrottling: false,
      // Agent navigation, including in hidden tabs, must not take the user's keyboard focus.
      focusOnNavigation: false,
    },
  });
  const contents = view.webContents;
  const detachNetwork = options.network?.attach(contents);
  const picker = createPicker(contents, {
    zoom: () => contents.getZoomFactor(),
    origin: () => {
      const content = win.getContentBounds();
      const bounds = view.getBounds();
      return { x: content.x + bounds.x, y: content.y + bounds.y };
    },
    window: options.modifiers,
  });
  contents.on("blur", () => picker.blur());
  contents.on("before-input-event", (event, input) => {
    if (picker.key(event, input)) return;
    if (input.type !== "keyDown") return;
    page.personActed();
    if (
      input.key === "F5" &&
      !input.meta &&
      !input.control &&
      !input.alt &&
      !input.shift
    ) {
      event.preventDefault();
      contents.reload();
      return;
    }
    if (
      input.alt ||
      !(process.platform === "darwin" ? input.meta : input.control)
    )
      return;
    const step =
      input.key === "=" || input.key === "+" || input.code === "NumpadAdd"
        ? 0.5
        : input.key === "-" || input.code === "NumpadSubtract"
          ? -0.5
          : 0;
    if (!step && input.key !== "0") return;
    event.preventDefault();
    contents.setZoomLevel(
      input.key === "0" ? 0 : contents.getZoomLevel() + step,
    );
  });
  // A press of the person's in the page ends any hold on file choosers the
  // agent's last action left (an agent's own input arrives during its action).
  contents.on("before-mouse-event", (event, mouse) => {
    if (picker.mouse(event, mouse)) return;
    if (mouse.type === "mouseDown") page.personActed();
  });
  const transport = createCdp(contents);
  // The agent's own input reaches the page through these DevTools commands;
  // the picker lets exactly the presses and keys they make through.
  const cdp: typeof transport = {
    ...transport,
    send(method, params, sessionID) {
      if (!method.startsWith("Input.")) return transport.send(method, params, sessionID);
      return picker.dispatch(method, (params ?? {}) as Record<string, unknown>, () =>
        transport.send(method, params, sessionID),
      );
    },
  };
  // Electron reports loadURL's navigation start synchronously, before
  // createPage returns; the driver hands the page to `start` for that event.
  let page!: Page<Browser.File>;
  const files = createBrowserFiles(() => page.sources());
  const profiling = createProfiling(contents, cdp, files, () => page.sources());
  let closed = false;
  const publish = () => {
    if (!closed) options.publish();
  };
  const surface: PageSurface = {
    url: () => contents.getURL(),
    title: () => contents.getTitle(),
    loading: () => contents.isLoading(),
    canGoBack: () => contents.navigationHistory.canGoBack(),
    canGoForward: () => contents.navigationHistory.canGoForward(),
    historyEntry(delta) {
      const history = contents.navigationHistory;
      const index = history.getActiveIndex() + delta;
      return { index, url: history.getEntryAtIndex(index)?.url };
    },
    async navigate(url, signal) {
      const cancel = () => contents.stop();
      signal.addEventListener("abort", cancel, { once: true });
      try {
        await contents.loadURL(url);
      } finally {
        signal.removeEventListener("abort", cancel);
      }
    },
    back: () => contents.navigationHistory.goBack(),
    forward: () => contents.navigationHistory.goForward(),
    reload: () => contents.reload(),
    stop: () => contents.stop(),
    viewport: async () => {
      const { width, height } = view.getBounds();
      return { width, height };
    },
    pixelRatio: async () =>
      contents.getZoomFactor() *
      electron.screen.getDisplayMatching(win.getBounds()).scaleFactor,
    readyForCapture: (signal) =>
      waitFor(
        () => view.getVisible() && win.isVisible() && !win.isMinimized(),
        signal,
        3_000,
      ).catch((error) => {
        if (signal.aborted) throw error;
        throw new Error(
          "Screenshot needs a visible tab. Call browser.tabs.focus and keep its desktop window visible.",
        );
      }),
    // Force a compositor update after scrolling. requestAnimationFrame can
    // stall in a hidden WebContentsView, even with background throttling off.
    async repaint() {
      await contents.capturePage(undefined, {
        stayHidden: false,
        stayAwake: true,
      });
    },
    selectAll: { keys: process.platform === "darwin" ? "Meta+A" : "Control+A" },
    supports: () => true,
    gateTopDocumentOnly: false,
    resourceOrigins: "url",
    cursor: {
      async run(code) {
        if (contents.isDestroyed() || !(options.agentCursor?.() ?? true)) return;
        await contents.executeJavaScriptInIsolatedWorld(CURSOR_WORLD, [{ code }]);
      },
      zoom: () => (contents.isDestroyed() ? 1 : contents.getZoomFactor()),
    },
  };
  // Tracing, CPU and heap profiling and Lighthouse need Node and this view, so they stay here.
  const extension: PageExtension<Browser.File> = {
    methods: new Set<Browser.Method>([
      "trace.start",
      "trace.stop",
      "trace.analyze",
      "cpu.start",
      "cpu.stop",
      "cpu.analyze",
      "heap.snapshot",
      "heap.summary",
      "heap.query",
      "heap.object",
      "heap.compare",
      "lighthouse",
    ]),
    inspect: (action) =>
      action.type === "trace.stop" || action.type === "cpu.stop"
        ? profiling.target(action.type === "trace.stop" ? "trace" : "cpu")
        : undefined,
    async execute(action, { state, sources, transfer, result }) {
      switch (action.type) {
        case "trace.start":
          await profiling.startTrace(action.durationMs);
          return result({ tab: state(), recording: true });
        case "trace.stop": {
          const value = await profiling.stopTrace();
          return result(
            {
              tab: state(),
              durationMs: value.durationMs,
              incomplete: value.incomplete,
            },
            [await transfer(value.id)],
          );
        }
        case "cpu.start":
          await profiling.startCpu();
          return result({ tab: state(), recording: true });
        case "cpu.stop": {
          const value = await profiling.stopCpu();
          return result({ tab: state(), durationMs: value.durationMs }, [
            await transfer(value.id),
          ]);
        }
        case "heap.snapshot":
          return result({ tab: state() }, [
            await transfer(await profiling.heap()),
          ]);
        case "trace.analyze":
        case "cpu.analyze":
        case "heap.summary":
        case "heap.query":
        case "heap.object":
        case "heap.compare":
          return result({ tab: state(), ...(await profiling.analyze(action)) });
        case "lighthouse": {
          const { audit } = await import("./browser/lighthouse");
          const report = await audit(contents, files, cdp, sources);
          return result(
            { tab: state(), scores: report.scores, failures: report.failures },
            await Promise.all(report.files.map(transfer)),
          );
        }
        default:
          throw new Error(
            "This operation was routed to a page instead of the tab manager. Report a desktop/plugin routing mismatch; changing tab IDs or repeating the operation will not fix it.",
          );
      }
    },
  };
  contents.on("did-start-navigation", (event) => {
    if (!event.isMainFrame || event.isSameDocument) return;
    // The picker lives in the document that is going.
    picker.end("navigated");
    page.navigationStarted();
  });
  contents.on("did-stop-loading", publish);
  contents.on("did-navigate-in-page", publish);
  contents.on("page-title-updated", publish);
  contents.on("page-favicon-updated", (_event, urls) => {
    if (!closed) options.favicon?.(urls);
  });
  contents.on("render-process-gone", () => {
    picker.end("closed");
    if (!closed) options.fail();
  });
  contents.debugger.on("detach", () => {
    if (!closed) options.fail();
  });
  contents.session.setPermissionRequestHandler(
    (_contents, _permission, callback) => callback(false),
  );
  contents.session.setPermissionCheckHandler(() => false);
  contents.session.setDevicePermissionHandler(() => false);
  contents.session.setDisplayMediaRequestHandler((_request, callback) =>
    callback({}),
  );
  contents.on("content-bounds-updated", (event) => event.preventDefault());
  // Sub-frames keep Chromium's own rules so blob:/data: viewers and sandboxed previews still load.
  const guard = (
    event: Electron.Event<{ url: string; isMainFrame: boolean }>,
  ) => {
    if (
      !event.isMainFrame ||
      event.url === "about:blank" ||
      destinationOrigin(event.url)
    )
      return;
    event.preventDefault();
    options.publish("ERR_BLOCKED_BY_CLIENT");
  };
  contents.on("will-frame-navigate", guard);
  contents.on("will-redirect", guard);
  contents.setWindowOpenHandler(({ url }) =>
    url === "about:blank" || destinationOrigin(url)
      ? {
          action: "allow",
          outlivesOpener: true,
          overrideBrowserWindowOptions: {
            webPreferences: {
              nodeIntegration: false,
              contextIsolation: true,
              sandbox: true,
              webSecurity: true,
              webviewTag: false,
              devTools: false,
              // Electron applies these preferences before the popup is adopted by our view.
              focusOnNavigation: false,
              partition: options.partition,
            },
          },
          createWindow: (popupOptions) => options.popup(popupOptions),
        }
      : { action: "deny" },
  );
  const download = (
    _event: Electron.Event,
    item: Electron.DownloadItem,
    source: WebContents,
  ) => {
    if (source !== contents) return;
    try {
      const file = files.add(
        item.getFilename(),
        item.getMimeType() || "application/octet-stream",
        [...page.sources(), ...item.getURLChain()],
      );
      item.setSavePath(file.path);
      item.on("updated", () => {
        file.bytes = item.getReceivedBytes();
        if (file.bytes > Browser.MAX_FILE_BYTES) item.cancel();
      });
      item.once("done", (_event, status) => {
        file.bytes = item.getReceivedBytes();
        file.state = status === "completed" ? "completed" : "failed";
        options.downloaded?.({ ...file, state: file.state, url: item.getURL() });
      });
    } catch {
      item.cancel();
      options.publish("download_failed");
    }
  };
  contents.session.on("will-download", download);
  view.setBounds({ x: 0, y: 0, width: 1000, height: 700 });
  view.setVisible(false);
  win.contentView.addChildView(view);
  const corners = [new electron.ImageView(), new electron.ImageView()];
  let cornerKey = "";
  corners.forEach((corner) => {
    corner.setVisible(false);
    win.contentView.addChildView(corner);
  });
  const giveBackKeyboard = () => void returnKeyboard(win, contents);
  page = createPage<Browser.File>(surface, cdp, {
    id: options.id,
    publish: () => options.publish(),
    // The first restored navigation consumes the generation reserved in the unloaded inventory.
    generation: options.restore ? options.restore.generation - 1 : 0,
    files,
    input: {
      // Trusted uploads: Chromium reads files the desktop wrote, as a user's file picker would.
      stage: (transfers, sources) =>
        Promise.all(
          transfers.map(async (file) => {
            if (!("data" in file))
              throw new Error(
                "Upload command has no file bytes. Supply server-local paths to browser.files.upload/drop; do not call the desktop RPC directly with desktop paths. If paths were supplied, report a client/server transfer mismatch.",
              );
            return files.get(
              await files.save(file.name, file.mime, file.data, sources),
            ).path;
          }),
        ),
    },
    translate: options.translate,
    start:
      options.initialize === false
        ? undefined
        : (self) => {
            page = self;
            return contents
              .loadURL(normalizeURL(options.restore?.url || "about:blank"))
              .catch((error: Error) => {
                if (!options.restore) throw error;
                // A dev server may have stopped while this page was unloaded. Keep its tab available to retry.
                options.publish(error.message);
              });
          },
    extension,
  });

  return {
    view,
    contents,
    state: page.state,
    ready: page.ready,
    layout(
      bounds: Electron.Rectangle,
      background?: readonly [number, number, number, number],
      radius = 10,
    ) {
      view.setBounds(bounds);
      const size = Math.min(
        radius,
        Math.floor(bounds.width / 2),
        Math.floor(bounds.height / 2),
      );
      const scale = electron.screen.getDisplayMatching(
        win.getBounds(),
      ).scaleFactor;
      const key =
        background && size > 0 ? `${background}:${size}:${scale}` : "";
      if (key && key !== cornerKey && background) {
        createCornerImages(background, size, scale).forEach((image, index) =>
          corners[index].setImage(image),
        );
      }
      cornerKey = key;
      corners.forEach((corner, index) => {
        // A composited layer is required above WebContentsView. A zero-duration
        // bounds update creates that layer without a visible animation.
        corner.setBounds(
          {
            x: bounds.x + (index ? bounds.width - size : 0),
            y: bounds.y + bounds.height - size,
            width: size,
            height: size,
          },
          { animate: { duration: 0 } },
        );
      });
    },
    setVisible(visible: boolean) {
      if (!visible) {
        // Picking belongs to the page on screen.
        picker.end("hidden");
        giveBackKeyboard();
      }
      view.setVisible(visible);
      corners.forEach((corner) => corner.setVisible(visible && !!cornerKey));
    },
    execute(command: Browser.Command, signal: AbortSignal): Promise<Browser.Result> {
      // The agent never sees the person's picking outline.
      if (command.action.type === "screenshot")
        return picker.hidden(() => page.execute(command, signal));
      return page.execute(command, signal);
    },
    /**
     * Lets the person pick elements in this page until they stop or a click
     * picks one; false when picking ended again before the page was ready.
     */
    pick(
      request: PickRequest,
      callbacks: {
        picked: (element: PickedElement, more: boolean) => void;
        ended: (reason: PickEnd) => void;
      },
    ) {
      return picker.start(request, callbacks);
    },
    stopPicking(reason: PickEnd = "stopped") {
      picker.end(reason);
    },
    get picking() {
      return picker.active;
    },
    async dispose() {
      if (closed) return;
      closed = true;
      picker.end("closed");
      page.dispose();
      detachNetwork?.();
      contents.session.off("will-download", download);
      await profiling.dispose();
      cdp.dispose();
      giveBackKeyboard();
      if (!win.isDestroyed()) {
        corners.forEach((corner) => win.contentView.removeChildView(corner));
        win.contentView.removeChildView(view);
      }
      if (!contents.isDestroyed())
        contents.close({ waitForBeforeUnload: false });
      await files.dispose();
    },
  };
}
