/**
 * `chrome.debugger` as the driver's CDP transport.
 *
 * Chrome delivers every debugger event through one global `onEvent`, so the
 * platform demultiplexes it by tab (and by child session: out-of-process
 * iframes attach as flattened sessions, which `chrome.debugger` supports from
 * Chrome 125). Like Desktop's transport, a session only passes on events from
 * the root and from the iframe sessions it saw attach.
 *
 * A transport outlives an idle detach (`park`, then `resume` once attached
 * again), so the driver page on it keeps its refs: Chrome forgets the
 * session's settings when the debugger detaches, and `resume` sends them
 * again; iframes attach again under new session ids, which the transport
 * maps the old ones to.
 */
import type { ProtocolMapping } from "devtools-protocol/types/protocol-mapping.js";
import type { CdpTransport } from "../../core/driver-contract";
import { HostError } from "../../core/errors";
import { within } from "./wait";

type Listener = (params: unknown, sessionID?: string) => void;

const LET_GO = "Haarness let go of this tab (it closed, or the debugger detached). Call browser_tabs_list and try again.";
/** Calls that set up the tab's own debugging session until it detaches, besides each domain's `enable` (but Fetch's, which is per command). */
const SETTINGS = new Set(["Target.setAutoAttach", "Network.setBypassServiceWorker", "Page.setPrerenderingAllowed"]);
/** How long attaching again waits for those settings (a JavaScript dialog holds the page's own). */
const RESUME_WAIT_MS = 2_000;

/** Whether a call on the tab's own session sets something up for the whole session. */
export function lasting(method: string) {
  return SETTINGS.has(method) || (/\.enable$/.test(method) && method !== "Fetch.enable");
}

export interface ChromeTransport extends CdpTransport {
  readonly tabId: number;
  /** Child (iframe) sessions currently attached. */
  children(): string[];
  receive(method: string, params: unknown, sessionID?: string): void;
  closed(): boolean;
  /** The debugger detached while the chat is idle; calls fail until `resume`. */
  park(): void;
  /** Attached again: sends the session's settings again. */
  resume(): Promise<void>;
}

export function createTransport(tabId: number): ChromeTransport {
  const listeners = new Map<string, Set<Listener>>();
  let sessions = new Set([""]);
  /** The tab's own session's settings, in the order first sent, to send again after a re-attach. */
  const settings = new Map<string, object>();
  /** Each attached iframe's session, by target. */
  const targets = new Map<string, string>();
  /** The iframes' sessions before the last detach, by target, until they attach again. */
  let previous = new Map<string, string>();
  /** An iframe's session from before a detach → its session now. */
  const aliases = new Map<string, string>();
  let disposed = false;
  let parked = false;

  async function call<M extends keyof ProtocolMapping.Commands>(method: M, params: object, sessionID?: string) {
    const target: chrome.debugger.DebuggerSession = sessionID ? { tabId, sessionId: sessionID } : { tabId };
    try {
      return (await chrome.debugger.sendCommand(target, method, params as Record<string, unknown>)) as ProtocolMapping.Commands[M]["returnType"];
    } catch (error) {
      throw protocolError(method, error);
    }
  }

  return {
    tabId,
    async send<M extends keyof ProtocolMapping.Commands>(method: M, params: object = {}, sessionID?: string) {
      if (disposed || parked) throw new Error(LET_GO);
      // A ref or context the driver kept from before a re-attach names the frame's old session.
      const result = await call(method, params, sessionID && (aliases.get(sessionID) ?? sessionID));
      if (!sessionID && lasting(method)) settings.set(method, params);
      else if (!sessionID && method.endsWith(".disable")) settings.delete(method.replace(/disable$/, "enable"));
      return result;
    },
    on<E extends keyof ProtocolMapping.Events>(
      event: E,
      callback: (params: ProtocolMapping.Events[E][0], sessionID?: string) => void,
    ) {
      const handler: Listener = (params, sessionID) => callback(params as ProtocolMapping.Events[E][0], sessionID);
      const handlers = listeners.get(event) ?? new Set();
      handlers.add(handler);
      listeners.set(event, handlers);
      return () => {
        handlers.delete(handler);
        if (!handlers.size) listeners.delete(event);
      };
    },
    receive(method, params, sessionID) {
      if (disposed || !sessions.has(sessionID ?? "")) return;
      if (method === "Target.attachedToTarget") {
        const event = params as ProtocolMapping.Events["Target.attachedToTarget"][0];
        if (event.targetInfo.type === "iframe") {
          sessions.add(event.sessionId);
          targets.set(event.targetInfo.targetId, event.sessionId);
          const old = previous.get(event.targetInfo.targetId);
          if (old !== undefined) {
            previous.delete(event.targetInfo.targetId);
            for (const [from, to] of aliases) if (to === old) aliases.set(from, event.sessionId);
            aliases.set(old, event.sessionId);
          }
        }
      }
      if (method === "Target.detachedFromTarget") {
        const detached = (params as ProtocolMapping.Events["Target.detachedFromTarget"][0]).sessionId;
        sessions.delete(detached);
        for (const [target, session] of targets) if (session === detached) targets.delete(target);
      }
      for (const handler of [...(listeners.get(method) ?? [])]) handler(params, sessionID || undefined);
    },
    children: () => [...sessions].filter(Boolean),
    closed: () => disposed,
    park() {
      parked = true;
      previous = new Map(targets);
      targets.clear();
      sessions = new Set([""]);
    },
    async resume() {
      parked = false;
      // Sent together, in order; iframes attach again as `Target.setAutoAttach` is restored.
      const restored = [...settings].map(([method, params]) => call(method as keyof ProtocolMapping.Commands, params).catch(() => {}));
      await within(Promise.all(restored), RESUME_WAIT_MS);
    },
    dispose() {
      disposed = true;
      listeners.clear();
    },
  };
}

/** Chrome's errors with a next step the model can take, like Desktop's. */
export function protocolError(method: string, error: unknown) {
  const detail = (error instanceof Error ? error.message : String(error)).slice(0, 400);
  if (/Cannot access|Cannot attach|chrome-extension:|chrome:\/\//i.test(detail)) return new HostError("restricted-page");
  const recovery = /(?:node|object).*(?:not found|not exist)|detached/i.test(detail)
    ? "The element may have detached. Call browser_snapshot and use a fresh ref from that tab."
    : /context.*(?:destroyed|not found)|session.*not found|No target with given id/i.test(detail)
      ? "The document or frame changed. Call browser_frames and browser_snapshot; use current frame IDs and refs."
      : /wasn't found|method not found|not implemented|not allowed/i.test(detail)
        ? "Chrome does not allow this operation for an extension. Use the Haarness browser in Desktop for it."
        : "Inspect browser_tabs_list and the target tab before deciding to retry; a partially completed action is not automatically safe to repeat.";
  return new Error(`${recovery} Chrome command ${method} failed: ${detail}`, { cause: error });
}

export interface DebuggerHooks {
  detached(tabId: number, reason: string): void;
}

/** What the debugger needs from a tab's session to keep it across an idle detach. */
export interface ParkableSession {
  dispose(): void;
  /** The debugger is about to let go while the chat is idle; false when the session cannot be kept (a dialog is open). */
  park(): boolean;
  /** Chrome reported the tab loading a new document while detached. */
  navigated(): void;
  /** Attached again (the transport has its settings back): catch up on what happened meanwhile. */
  resume(): Promise<void>;
}

interface Entry<Session> {
  transport: ChromeTransport;
  session: Session;
  /** Detached while idle; the next `session()` attaches again and keeps both. */
  parked: boolean;
}

/**
 * Attaches the debugger to agent tabs on first use. Attaching shows Chrome's
 * "started debugging this browser" bar; the host parks a chat's tabs after
 * 30 s without a command so the bar does not stay up for nothing, and the
 * next command attaches again to the same session.
 */
export function createDebugger<Session extends ParkableSession>(
  hooks: DebuggerHooks,
  open: (transport: ChromeTransport) => Session,
) {
  const sessions = new Map<number, Entry<Session>>();
  const attaching = new Map<number, Promise<Entry<Session>>>();
  /** A detach in progress per tab; attaching again waits for it. */
  const detaching = new Map<number, Promise<void>>();

  async function attach(tabId: number) {
    try {
      await chrome.debugger.attach({ tabId }, "1.3");
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error);
      // Ours from before a worker restart: take it over.
      if (/already attached/i.test(message)) {
        await chrome.debugger.detach({ tabId }).catch(() => {});
        try {
          await chrome.debugger.attach({ tabId }, "1.3");
        } catch {
          throw new Error(
            "Another debugger (DevTools or another extension) is attached to this tab. Ask the user to close it, then try again.",
          );
        }
        return;
      }
      if (/Cannot access|Cannot attach/i.test(message)) throw new HostError("restricted-page");
      throw new Error(`Chrome could not attach to this tab: ${message.slice(0, 300)}`);
    }
  }

  /** Forgets a tab's session for good; the host hears it as a detach and drops its page. */
  function drop(tabId: number, reason: string) {
    const entry = sessions.get(tabId);
    if (!entry) return;
    sessions.delete(tabId);
    entry.session.dispose();
    entry.transport.dispose();
    hooks.detached(tabId, reason);
  }

  /** chrome.debugger.detach, which a later attach of the same tab waits for. */
  function release(tabId: number) {
    const done = chrome.debugger.detach({ tabId }).then(
      () => {},
      () => {},
    );
    detaching.set(tabId, done);
    void done.finally(() => {
      if (detaching.get(tabId) === done) detaching.delete(tabId);
    });
    return done;
  }

  /** Attaches a tab with a new session, or a parked one again with its own; once for concurrent callers. */
  function connect(tabId: number): Promise<Entry<Session>> {
    let pending = attaching.get(tabId);
    if (!pending) {
      pending = (async () => {
        const parked = sessions.get(tabId);
        if (parked?.parked) {
          try {
            await attach(tabId);
          } catch (error) {
            drop(tabId, "reattach_failed");
            throw error;
          }
          parked.parked = false;
          await parked.transport.resume();
          await parked.session.resume();
          return parked;
        }
        await attach(tabId);
        const transport = createTransport(tabId);
        const entry = { transport, session: open(transport), parked: false };
        sessions.set(tabId, entry);
        return entry;
      })().finally(() => attaching.delete(tabId));
      attaching.set(tabId, pending);
    }
    return pending;
  }

  return {
    async session(tabId: number): Promise<Session> {
      for (;;) {
        await detaching.get(tabId);
        const existing = sessions.get(tabId);
        if (existing && !existing.parked && !existing.transport.closed()) return existing.session;
        // A park that waited for the same attach may have let go of it again: attach once more.
        const entry = await connect(tabId);
        if (!entry.parked && !entry.transport.closed()) return entry.session;
      }
    },
    get(tabId: number) {
      return sessions.get(tabId)?.session;
    },
    /** Attached now (not parked). */
    attached(tabId: number) {
      const entry = sessions.get(tabId);
      return !!entry && !entry.parked;
    },
    /**
     * Lets go of a tab while its chat is idle but keeps its session, and so
     * the driver page and its refs, for the next command. A session that
     * cannot be kept is dropped as before.
     */
    async park(tabId: number) {
      await attaching.get(tabId)?.catch(() => {});
      const entry = sessions.get(tabId);
      if (!entry || entry.parked || entry.transport.closed()) return;
      if (!entry.session.park()) {
        drop(tabId, "idle");
        return release(tabId);
      }
      entry.parked = true;
      entry.transport.park();
      return release(tabId);
    },
    /** `chrome.tabs.onUpdated` reported a load: a parked tab's page learns its document changed. */
    navigated(tabId: number) {
      const entry = sessions.get(tabId);
      if (entry?.parked) entry.session.navigated();
    },
    async detach(tabId: number) {
      const entry = sessions.get(tabId);
      if (!entry) return;
      sessions.delete(tabId);
      entry.session.dispose();
      entry.transport.dispose();
      if (!entry.parked) await release(tabId);
    },
    async detachAll() {
      await Promise.all([...sessions.keys()].map((tabId) => this.detach(tabId)));
    },
    /** `chrome.debugger.onEvent`, registered by the platform at start. */
    event(source: chrome.debugger.DebuggerSession, method: string, params?: object) {
      if (source.tabId === undefined) return;
      sessions.get(source.tabId)?.transport.receive(method, params, source.sessionId);
    },
    /** `chrome.debugger.onDetach`, registered by the platform at start. */
    detachedBy(source: chrome.debugger.Debuggee, reason: string) {
      if (source.tabId === undefined) return;
      const entry = sessions.get(source.tabId);
      sessions.delete(source.tabId);
      entry?.session.dispose();
      entry?.transport.dispose();
      hooks.detached(source.tabId, reason);
    },
  };
}
