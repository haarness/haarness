/**
 * One agent tab under the debugger: site rules enforced while a command runs,
 * page loads, the driver page, and downloads the page starts.
 */
import type { CreatePage, Page, PageOptions } from "../../core/driver-contract";
import type { createAttribution } from "../../core/downloads";
import { CANCELLED, HostError } from "../../core/errors";
import { destinationOrigin } from "../../core/policy";
import { blockedMessage, decideRequest, fetchPatterns, landing, type FetchContext, type FetchDecision } from "../../core/fetch-policy";
import type { TabSession } from "../../core/platform";
import type { ChromeTransport } from "./cdp";
import { createFileInput, type ChromeFileStore } from "./files";
import { createSurface, type TabCache } from "./page";
import { abortable } from "./wait";

export interface SessionDeps {
  cache: TabCache;
  files: ChromeFileStore;
  attribution: ReturnType<typeof createAttribution>;
  uploadsRoot(): string | null;
  cursor(): boolean;
  now(): number;
}

/** The agent command a download belongs to. */
type Owner = { server: string; thread: string; command: string };

/** How long a main-frame navigation right after a command is still checked against the chat's rules. */
const BACKSTOP_MS = 2_000;
/**
 * How long after a command a download its tab begins is still that
 * command's: a server answers "Export" with an attachment after the click
 * has returned.
 */
const DOWNLOAD_GRACE_MS = 10_000;
const LOAD_TIMEOUT_MS = 30_000;
/** How long a command waits for the session's service-worker and prerender settings (a dialog can hold them). */
const ISOLATION_WAIT_MS = 2_000;

export type ChromeTabSession = TabSession & { dispose(): void };

export function createSession(transport: ChromeTransport, deps: SessionDeps): ChromeTabSession {
  const tabId = transport.tabId;
  let command: Owner | undefined;
  /** The tab's last command, until downloads it set off no longer count as its. */
  let last: { owner: Owner; until: number } | undefined;
  let after: (() => Promise<void>)[] = [];
  let page: Page | undefined;
  let surface: ReturnType<typeof createSurface> | undefined;
  let main: string | undefined;
  let started: string | undefined;
  let backstop: (() => void) | undefined;

  const enabled = transport.send("Page.enable").catch(() => {});
  // Agent tabs skip service workers and prerendering for the whole debugging
  // session. Sent once: like other page calls they wait while a JavaScript
  // dialog is open, and a command must not.
  const isolated = Promise.all([
    transport.send("Network.setBypassServiceWorker", { bypass: true }).catch(() => {}),
    transport.send("Page.setPrerenderingAllowed", { isAllowed: false }).catch(() => {}),
  ]);
  const mainFrame = async () => (main ??= (await transport.send("Page.getFrameTree")).frameTree.frame.id);

  const offs = [
    transport.on("Page.downloadWillBegin", (event) => {
      const owner = command ?? (last && deps.now() < last.until ? last.owner : undefined);
      if (!owner) return;
      const origin = destinationOrigin(event.url);
      const resources = [...(page?.sources() ?? []), ...(origin ? [origin] : [])];
      deps.attribution.began({ url: event.url, tabId, name: event.suggestedFilename, resources, ...owner });
    }),
    // The driver counts documents: a main-frame navigation to a new document expires its refs.
    transport.on("Page.frameStartedNavigating", (event, session) => {
      if (session || event.frameId !== main || /sameDocument/i.test(event.navigationType)) return;
      started = event.loaderId;
      page?.navigationStarted();
    }),
    transport.on("Page.frameNavigated", (event, session) => {
      if (session || event.frame.parentId) return;
      main = event.frame.id;
      // Chrome before 133 has no frameStartedNavigating; count the commit instead.
      if (event.frame.loaderId !== started) page?.navigationStarted();
      started = undefined;
    }),
  ];

  async function blank() {
    await transport.send("Page.stopLoading").catch(() => {});
    await transport.send("Page.navigate", { url: "about:blank" }).catch(() => {});
  }

  /**
   * The URL the tab committed. Read from chrome.tabs, which answers while a
   * JavaScript dialog holds the page; the page's own CDP calls wait for it.
   */
  async function landed() {
    try {
      return (await chrome.tabs.get(tabId)).url || deps.cache.url;
    } catch {
      return deps.cache.url;
    }
  }

  async function load(url: string, signal: AbortSignal) {
    await enabled;
    let done!: () => void;
    const loaded = new Promise<void>((resolve) => (done = resolve));
    const off = transport.on("Page.loadEventFired", () => done());
    try {
      const result = await abortable(transport.send("Page.navigate", { url }), signal);
      if (result.errorText) throw new Error(`Chrome could not load ${url}: ${result.errorText}`);
      // A same-document navigation has no loader and fires no load event.
      if (!result.loaderId) return;
      await abortable(loaded, signal, LOAD_TIMEOUT_MS).catch((error) => {
        if (error instanceof Error && error.message === CANCELLED) throw error;
        // A page that never finishes loading is still usable; the tab reports loading.
      });
    } catch (error) {
      if (signal.aborted) await transport.send("Page.stopLoading").catch(() => {});
      throw error;
    } finally {
      off();
    }
  }

  return {
    cdp: transport,
    load,
    async guard(context: FetchContext, signal: AbortSignal, work) {
      if (signal.aborted) throw new Error(CANCELLED);
      backstop?.();
      await enabled;
      const patterns = fetchPatterns(context.policy);
      const frame = await mainFrame();
      /** The tab's own page was blocked: the command fails and the tab leaves the site. */
      let blocked: Extract<FetchDecision, { action: "block" }> | undefined;
      const sessions = new Set<string | undefined>([undefined]);
      const enable = (session: string | undefined) =>
        transport.send("Fetch.enable", { patterns }, session).then(() => {
          sessions.add(session);
        });
      const offPaused = transport.on("Fetch.requestPaused", (event, session) => {
        const request = {
          url: event.request.url + (event.request.urlFragment ?? ""),
          resourceType: event.resourceType,
          mainFrame: !session && event.frameId === frame,
        };
        const decision = decideRequest(context, request);
        if (decision.action === "block") {
          // A frame or resource from a blocked site is simply not loaded: the
          // allowed page goes on, and the command's own effect stands.
          if (request.mainFrame && request.resourceType === "Document") blocked ??= decision;
          void transport.send("Fetch.failRequest", { requestId: event.requestId, errorReason: "BlockedByClient" }, session).catch(() => {});
        } else void transport.send("Fetch.continueRequest", { requestId: event.requestId }, session).catch(() => {});
      });
      const offChild = transport.on("Target.attachedToTarget", (event) => {
        if (event.targetInfo.type === "iframe") void enable(event.sessionId).catch(() => {});
      });
      let result: Awaited<ReturnType<typeof work>> | undefined;
      let failure: unknown;
      try {
        // Await every enable before the command's first input.
        await enable(undefined);
        await Promise.all(transport.children().map((child) => enable(child).catch(() => {})));
        await Promise.race([isolated, new Promise((resolve) => setTimeout(resolve, ISOLATION_WAIT_MS))]);
        result = await work();
      } catch (error) {
        failure = error;
      } finally {
        offPaused();
        offChild();
        await Promise.all([...sessions].map((session) => transport.send("Fetch.disable", {}, session).catch(() => {})));
      }
      if (!blocked) {
        const decision = landing(context, await landed());
        if (decision.action === "block") blocked = decision;
      }
      if (blocked) {
        await blank();
        throw new HostError("site-blocked", blockedMessage(blocked));
      }
      watch(context);
      if (failure !== undefined) throw failure;
      return result as Awaited<ReturnType<typeof work>>;
    },
    page(create: CreatePage, options: Omit<PageOptions, "files" | "input">) {
      if (page) return page;
      surface = createSurface(transport, {
        cache: deps.cache,
        load,
        after: (work) => void after.push(work),
        cursor: deps.cursor,
        mainFrame,
      });
      page = create(surface, transport, { ...options, files: deps.files, input: createFileInput(deps.uploadsRoot) });
      void mainFrame().catch(() => {});
      return page;
    },
    begin(current) {
      command = current;
    },
    async end() {
      if (command) last = { owner: command, until: deps.now() + DOWNLOAD_GRACE_MS };
      command = undefined;
      const pending = after;
      after = [];
      for (const work of pending) await work().catch(() => {});
    },
    dispose() {
      backstop?.();
      for (const off of offs) off();
      surface?.dispose();
      page = undefined;
    },
  };

  /**
   * For two seconds after a command, a main-frame navigation the command
   * set off (a timer, a late redirect) is still checked; one that lands
   * somewhere the chat may not be is sent back to a blank page.
   */
  function watch(context: FetchContext) {
    const off = transport.on("Page.frameNavigated", (event, session) => {
      if (session || event.frame.parentId) return;
      if (landing(context, event.frame.url).action === "block") void blank();
    });
    const timer = setTimeout(stop, BACKSTOP_MS);
    function stop() {
      clearTimeout(timer);
      off();
      if (backstop === stop) backstop = undefined;
    }
    backstop = stop;
  }
}
