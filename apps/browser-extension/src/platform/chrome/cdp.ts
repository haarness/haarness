/**
 * `chrome.debugger` as the driver's CDP transport.
 *
 * Chrome delivers every debugger event through one global `onEvent`, so the
 * platform demultiplexes it by tab (and by child session: out-of-process
 * iframes attach as flattened sessions, which `chrome.debugger` supports from
 * Chrome 125). Like Desktop's transport, a session only passes on events from
 * the root and from the iframe sessions it saw attach.
 */
import type { ProtocolMapping } from "devtools-protocol/types/protocol-mapping.js";
import type { CdpTransport } from "../../core/driver-contract";
import { HostError } from "../../core/errors";

type Listener = (params: unknown, sessionID?: string) => void;

export interface ChromeTransport extends CdpTransport {
  readonly tabId: number;
  /** Child (iframe) sessions currently attached. */
  children(): string[];
  receive(method: string, params: unknown, sessionID?: string): void;
  closed(): boolean;
}

export function createTransport(tabId: number): ChromeTransport {
  const listeners = new Map<string, Set<Listener>>();
  const sessions = new Set([""]);
  let disposed = false;
  return {
    tabId,
    async send<M extends keyof ProtocolMapping.Commands>(method: M, params: object = {}, sessionID?: string) {
      if (disposed)
        throw new Error("Haarness let go of this tab (it closed, or the debugger detached). Call browser_tabs_list and try again.");
      const target: chrome.debugger.DebuggerSession = sessionID ? { tabId, sessionId: sessionID } : { tabId };
      try {
        return (await chrome.debugger.sendCommand(target, method, params as Record<string, unknown>)) as ProtocolMapping.Commands[M]["returnType"];
      } catch (error) {
        throw protocolError(method, error);
      }
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
        if (event.targetInfo.type === "iframe") sessions.add(event.sessionId);
      }
      if (method === "Target.detachedFromTarget")
        sessions.delete((params as ProtocolMapping.Events["Target.detachedFromTarget"][0]).sessionId);
      for (const handler of [...(listeners.get(method) ?? [])]) handler(params, sessionID || undefined);
    },
    children: () => [...sessions].filter(Boolean),
    closed: () => disposed,
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

/**
 * Attaches the debugger to agent tabs on first use. Attaching shows Chrome's
 * "started debugging this browser" bar; the host detaches a chat's tabs after
 * 30 s without a command so the bar does not stay up for nothing.
 */
export function createDebugger<Session extends { dispose(): void }>(
  hooks: DebuggerHooks,
  open: (transport: ChromeTransport) => Session,
) {
  const sessions = new Map<number, { transport: ChromeTransport; session: Session }>();
  const attaching = new Map<number, Promise<{ transport: ChromeTransport; session: Session }>>();

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

  return {
    async session(tabId: number): Promise<Session> {
      const existing = sessions.get(tabId);
      if (existing && !existing.transport.closed()) return existing.session;
      let pending = attaching.get(tabId);
      if (!pending) {
        pending = (async () => {
          await attach(tabId);
          const transport = createTransport(tabId);
          const entry = { transport, session: open(transport) };
          sessions.set(tabId, entry);
          return entry;
        })().finally(() => attaching.delete(tabId));
        attaching.set(tabId, pending);
      }
      return (await pending).session;
    },
    get(tabId: number) {
      return sessions.get(tabId)?.session;
    },
    attached(tabId: number) {
      return sessions.has(tabId);
    },
    async detach(tabId: number) {
      const entry = sessions.get(tabId);
      if (!entry) return;
      sessions.delete(tabId);
      entry.session.dispose();
      entry.transport.dispose();
      await chrome.debugger.detach({ tabId }).catch(() => {});
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
