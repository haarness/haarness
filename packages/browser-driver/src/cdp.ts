// Includes MIT-licensed code; see THIRD-PARTY-NOTICES.md.
import type { ProtocolMapping } from "devtools-protocol/types/protocol-mapping.js";
import type { CdpTransport } from "./types";

export type Cdp = CdpTransport;

/**
 * The event half of a transport: listeners per event, fed with every message
 * the browser delivers for one tab. Only the tab's own session ("") and the
 * iframe sessions it announced with `Target.attachedToTarget` pass, so a
 * worker or another client's session never reaches the page.
 */
export function createCdpEvents() {
  const listeners = new Map<
    string,
    Set<(params: unknown, sessionID?: string) => void>
  >();
  const sessions = new Set([""]);
  return {
    receive(name: string, params: unknown, sessionID?: string) {
      if (!sessions.has(sessionID ?? "")) return;
      if (name === "Target.attachedToTarget") {
        const event =
          params as ProtocolMapping.Events["Target.attachedToTarget"][0];
        if (event.targetInfo.type === "iframe") sessions.add(event.sessionId);
      }
      if (name === "Target.detachedFromTarget")
        sessions.delete(
          (params as ProtocolMapping.Events["Target.detachedFromTarget"][0])
            .sessionId,
        );
      listeners
        .get(name)
        ?.forEach((callback) => callback(params, sessionID || undefined));
    },
    on<Method extends keyof ProtocolMapping.Events>(
      method: Method,
      callback: (
        params: ProtocolMapping.Events[Method][0],
        sessionID?: string,
      ) => void,
    ) {
      const handler = (params: unknown, sessionID?: string) =>
        callback(params as ProtocolMapping.Events[Method][0], sessionID);
      const handlers = listeners.get(method) ?? new Set();
      handlers.add(handler);
      listeners.set(method, handlers);
      return () => {
        handlers.delete(handler);
        if (!handlers.size) listeners.delete(method);
      };
    },
    clear() {
      listeners.clear();
    },
  };
}

export function abortError(signal: AbortSignal) {
  if (signal.aborted)
    throw new Error(
      "Browser operation was cancelled. Inspect the tab before deciding to repeat an action; cancellation does not undo changes already made.",
    );
}

export async function waitFor(
  check: () => boolean | Promise<boolean>,
  signal: AbortSignal,
  timeoutMs = 10_000,
) {
  const deadline = Date.now() + timeoutMs;
  const timeout = () =>
    new Error(`Condition was not met within ${timeoutMs} ms.`);
  // A busy renderer can hold one check past the deadline, so each check races the remaining time.
  while (true) {
    abortError(signal);
    const remaining = deadline - Date.now();
    if (remaining <= 0) throw timeout();
    let timer: ReturnType<typeof setTimeout> | undefined;
    const expired = new Promise<never>((_, reject) => {
      timer = setTimeout(() => reject(timeout()), remaining);
    });
    if (
      await Promise.race([check(), expired]).finally(() => clearTimeout(timer))
    )
      return;
    await new Promise((resolve) => setTimeout(resolve, 50));
  }
}
