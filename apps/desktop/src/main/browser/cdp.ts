// Includes MIT-licensed code; see THIRD-PARTY-NOTICES.md.
import type { WebContents } from "electron";
import type { ProtocolMapping } from "devtools-protocol/types/protocol-mapping.js";
import { createCdpEvents } from "@haarness/browser-driver/cdp";
import { protocolError } from "@haarness/browser-driver/errors";
import type { CdpTransport } from "@haarness/browser-driver/types";

export type Cdp = CdpTransport;

/** The driver's DevTools transport over a view's Electron debugger, attached on first use. */
export function createCdp(contents: WebContents): CdpTransport {
  const events = createCdpEvents();
  const receive = (
    _event: Electron.Event,
    name: string,
    params: unknown,
    sessionID?: string,
  ) => events.receive(name, params, sessionID);
  contents.debugger.on("message", receive);
  return {
    async send<Method extends keyof ProtocolMapping.Commands>(
      method: Method,
      params: object = {},
      sessionID?: string,
    ): Promise<ProtocolMapping.Commands[Method]["returnType"]> {
      if (contents.isDestroyed())
        throw new Error(
          "Browser tab was closed. Call browser.tabs.list({}) and choose an existing tabID, or browser.tabs.open({}) if no tabs remain.",
        );
      // attach can throw synchronously; sendCommand can reject asynchronously.
      try {
        if (!contents.debugger.isAttached()) contents.debugger.attach("1.3");
        return await contents.debugger.sendCommand(method, params, sessionID);
      } catch (error) {
        throw protocolError(method, error);
      }
    },
    on: events.on,
    dispose() {
      contents.debugger.off("message", receive);
      events.clear();
    },
  };
}
