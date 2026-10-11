import { session, type WebContents } from "electron";
import { BrowserProxy } from "./proxy";
import { decodeTunnelRead } from "@haarness/browser-driver/rpc";
export type BrowserNetwork = Awaited<ReturnType<typeof createBrowserNetwork>>;
export async function createBrowserNetwork(
  partition: string,
  rpc: (action: string, input: object, signal?: AbortSignal) => Promise<any>,
  permitted?: (url: string) => boolean,
) {
  const proxy = await BrowserProxy.make({
    open: (target, signal) => rpc("open", { target }, signal),
    read: async (id, signal) => decodeTunnelRead(await rpc("read", { id }, signal)),
    write: (id, data, end, signal) =>
      rpc(
        "write",
        { id, data: Buffer.from(data).toString("base64"), end },
        signal,
      ),
    close: (id) => rpc("close", { id }).catch(() => {}),
  });
  const profile = session.fromPartition(partition);
  try {
    await profile.setProxy({
      mode: "fixed_servers",
      proxyRules: proxy.url,
      proxyBypassRules: "<-loopback>",
    });
    await profile.closeAllConnections();
  } catch (error) {
    await proxy.close();
    throw error;
  }
  profile.webRequest.onBeforeRequest((details, callback) => callback({ cancel: permitted ? !permitted(details.url) : false }));
  return {
    attach(contents: WebContents) {
      const login = (
        event: Electron.Event,
        _details: Electron.AuthenticationResponseDetails,
        auth: Electron.AuthInfo,
        callback: (username?: string, password?: string) => void,
      ) => {
        if (
          !auth.isProxy ||
          auth.scheme !== "basic" ||
          auth.host !== proxy.host ||
          auth.port !== proxy.port ||
          auth.realm !== "Haarness Browser Proxy"
        )
          return;
        event.preventDefault();
        callback(proxy.credentials.username, proxy.credentials.password);
      };
      contents.on("login", login);
      contents.setWebRTCIPHandlingPolicy("disable_non_proxied_udp");
      return () => {
        contents.off("login", login);
      };
    },
    async dispose() {
      profile.webRequest.onBeforeRequest(null);
      await profile.closeAllConnections();
      await proxy.close();
    },
  };
}
