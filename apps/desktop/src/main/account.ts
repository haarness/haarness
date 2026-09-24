import { app, shell } from "electron";
import { spawn, type ChildProcess } from "node:child_process";
import { Haarness, type ServerAuth } from "@haarness/sdk";
import { executable, environment, invoke, connections } from "./runtime";
import type { Connection } from "../shared";

let login: ChildProcess | undefined;
export async function accountRequest(input: {
  action: string;
  [key: string]: unknown;
}) {
  return (await invoke(app.getPath("home"), false, ["account", "--rpc"], input))
    .value as any;
}
export function cancelAccountLogin() {
  login?.kill();
  login = undefined;
}
export async function accountLogin(
  device: boolean,
  progress: (value: unknown) => void,
) {
  if (login) throw new Error("Sign-in is already open in your browser.");
  const binary = await executable();
  return new Promise<void>((resolve, reject) => {
    const child = spawn(
      binary,
      ["login", "--json", "--no-browser", ...(device ? ["--device"] : [])],
      {
        env: environment(),
        stdio: ["ignore", "pipe", "pipe"],
        windowsHide: true,
      },
    );
    login = child;
    let buffer = "",
      error = "";
    const timer = setTimeout(() => child.kill(), 16 * 60_000);
    child.stderr.on("data", (data) => {
      error = (error + String(data)).slice(-4000);
    });
    child.stdout.on("data", (data) => {
      buffer += String(data);
      if (buffer.length > 128_000) {
        child.kill();
        return;
      }
      let end: number;
      while ((end = buffer.indexOf("\n")) >= 0) {
        const line = buffer.slice(0, end);
        buffer = buffer.slice(end + 1);
        try {
          const value = JSON.parse(line);
          if (value.type === "login") {
            const url = new URL(value.url);
            if (
              url.protocol !== "https:" &&
              !(
                url.protocol === "http:" &&
                ["127.0.0.1", "localhost", "[::1]"].includes(url.hostname)
              )
            )
              throw new Error("Invalid sign-in address");
            progress({ url: url.href, code: value.code });
            if (!process.env.HAARNESS_DESKTOP_TEST)
              void shell.openExternal(url.href);
          }
        } catch {
          error = "Could not start sign-in";
          child.kill();
        }
      }
    });
    child.once("error", reject);
    child.once("close", (code) => {
      clearTimeout(timer);
      if (login === child) login = undefined;
      if (code === 0) resolve();
      else reject(new Error(error || "Sign-in cancelled. You can try again."));
    });
  });
}

/** Only main holds tickets. Saved projects contain identifiers, never credentials. */
export async function connectCloud(
  workspace: string,
  expected?: { cloudAccount?: string; cloudApiUrl?: string },
  session = false,
): Promise<Connection> {
  const status = await accountRequest({ action: "status" });
  if (!status.loggedIn)
    throw new Error("Sign in to Haarness to open this session.");
  if (
    expected?.cloudAccount &&
    (expected.cloudAccount !== status.selectedAccountId ||
      expected.cloudApiUrl !== status.apiUrl)
  )
    throw new Error("Switch to this session’s Haarness account to open it.");
  const metadata = session ? await accountRequest({ action: "session", id: workspace }) : undefined;
  const attachAction = session ? "session-attach" : "attach";
  let ticket = await accountRequest({ action: attachAction, id: workspace });
  let pending: Promise<void> | undefined;
  const resolveAuth = async (): Promise<ServerAuth> => {
    if (Date.parse(ticket.expiresAt) < Date.now() + 30_000) {
      pending ??= accountRequest({ action: attachAction, id: workspace })
        .then((value) => {
          ticket = value;
        })
        .finally(() => {
          pending = undefined;
        });
      await pending;
    }
    return { type: "bearer", token: ticket.ticket };
  };
  const url = new URL(ticket.gatewayUrl);
  if (
    url.protocol !== "https:" &&
    !(
      url.protocol === "http:" &&
      ["127.0.0.1", "localhost", "[::1]"].includes(url.hostname)
    )
  )
    throw new Error("Cloud sessions require HTTPS.");
  const id = `${session ? "cloud-session" : "cloud"}:${status.apiUrl}:${status.selectedAccountId}:${workspace}`;
  const client = new Haarness({
    baseUrl: url.href,
    resolveAuth,
    timeoutMs: 30_000,
  });
  const info = await client.info();
  // A captured template supplies the default during rolling upgrades of older runtimes.
  if (metadata?.agentSnapshot && !info.defaultAgent) info.defaultAgent = "cloud-agent";
  const view: Connection = {
    id,
    label: metadata?.title ?? "Cloud session",
    workspace: String(info.workspace),
    remote: true,
    trust: false,
    url: url.href,
    cloudWorkspace: metadata?.workspace.id ?? workspace,
    ...(session ? { cloudSession: workspace } : {}),
    cloudAccount: status.selectedAccountId,
    cloudApiUrl: status.apiUrl,
    info,
  };
  connections.get(id)?.client.close();
  connections.set(id, { view, client, url: url.href, auth: { type: "none" } });
  return view;
}

export function closeCloudConnections() {
  for (const [id, value] of connections)
    if (value.view.cloudWorkspace) {
      value.client.close();
      connections.delete(id);
    }
}
