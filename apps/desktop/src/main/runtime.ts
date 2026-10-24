import { remoteLocationId } from "./remote-projects";
import { app } from "electron";
import { spawn, type ChildProcess } from "node:child_process";
import { access, realpath } from "node:fs/promises";
import { constants } from "node:fs";
import { join, resolve, delimiter } from "node:path";
import { fileURLToPath } from "node:url";
import { Haarness, type ServerAuth } from "@haarness/sdk";
import type { Connection, DesktopEvent } from "../shared";
import { localArgs } from "./validation";
import { isChatHome } from "./chat-home";
export interface ProjectConnection {
  view: Connection;
  client: Haarness;
  url: string;
  auth: ServerAuth;
  /** Short-lived credentials asked for before each request or stream (a
   * computer reached through Haarness Cloud); `auth` is unused then. */
  resolveAuth?: () => Promise<ServerAuth>;
  /** Sends the connection's requests, renewing credentials Cloud refused. */
  fetch?: typeof fetch;
}
export const connections = new Map<string, ProjectConnection>();
/** The credentials for one request or stream on this connection now. */
export const connectionAuth = (item: ProjectConnection) =>
  item.resolveAuth ? item.resolveAuth() : Promise.resolve(item.auth);
/** A new client of this connection's server, for a stream of its own. */
export function connectionClient(item: ProjectConnection, wrap?: (response: Response) => Response) {
  const send = item.fetch ?? fetch;
  return new Haarness({
    baseUrl: item.url,
    ...(item.resolveAuth ? { resolveAuth: item.resolveAuth } : { auth: item.auth }),
    ...(wrap || item.fetch ? { fetch: (input: RequestInfo | URL, init?: RequestInit) => send(input, init).then(wrap ?? ((response) => response)) } : {}),
  });
}
export const commands = new Map<string, ChildProcess>();
export function stopCommand(child: ChildProcess) {
  if (child.exitCode !== null || child.signalCode !== null) return;
  if (process.platform !== "win32" && child.pid) {
    try {
      process.kill(-child.pid, "SIGTERM");
    } catch {
      child.kill();
    }
  } else child.kill();
}
const root = resolve(fileURLToPath(new URL("../../../..", import.meta.url)));
export async function executable() {
  const candidates = [
    process.env.HAARNESS_BIN,
    app.isPackaged
      ? join(
          process.resourcesPath,
          "bin",
          process.platform === "win32" ? "haarness.exe" : "haarness",
        )
      : undefined,
    resolve(
      root,
      "target/debug",
      process.platform === "win32" ? "haarness.exe" : "haarness",
    ),
    resolve(
      root,
      "target/release",
      process.platform === "win32" ? "haarness.exe" : "haarness",
    ),
  ].filter(Boolean) as string[];
  for (const path of candidates) {
    try {
      await access(path, constants.X_OK);
      return path;
    } catch {}
  }
  throw new Error(
    "Haarness executable is missing. Run npm run desktop:dev from the repository, or reinstall the desktop app.",
  );
}
export function environment() {
  const paths = [
    ...(app.isPackaged ? [join(process.resourcesPath, "bin")] : []),
    process.env.PATH ?? "",
    join(app.getPath("home"), ".bun/bin"),
    "/opt/homebrew/bin",
    "/usr/local/bin",
    "/usr/bin",
    "/bin",
  ];
  return { ...process.env, PATH: paths.join(delimiter) };
}
export async function invoke(
  workspace: string,
  trust: boolean,
  args: string[],
  input?: unknown,
  id?: string,
  emit?: (event: DesktopEvent) => void,
) {
  const binary = await executable();
  // The chat home always runs with project configuration off, which its flag
  // says: it marks the folder as the chat home too.
  const home = await isChatHome(workspace);
  return new Promise<{ output: string; value: unknown }>((resolve, reject) => {
    const child = spawn(
      binary,
      [
        "--workspace",
        workspace,
        ...(home ? ["--chat-home"] : !trust ? ["--disable-project-config"] : []),
        ...args,
      ],
      {
        cwd: workspace,
        env: environment(),
        stdio: ["pipe", "pipe", "pipe"],
        windowsHide: true,
        detached: !!id && process.platform !== "win32",
      },
    );
    if (id) commands.set(id, child);
    let output = "",
      errors = "";
    let exceeded = false,
      timedOut = false;
    const timer = setTimeout(
      () => {
        timedOut = true;
        if (id) stopCommand(child);
        else child.kill();
      },
      args[0] === "auth"
        ? 600000
        : args[0] === "desktop" &&
            ["connect", "projects"].includes(
              (input as { action?: string })?.action ?? "",
            )
          ? 30000
          : 180000,
    );
    child.stdout.on("data", (chunk) => {
      output += chunk.toString();
      if (output.length > 16 * 1024 * 1024) {
        exceeded = true;
        if (id) stopCommand(child);
        else child.kill();
      }
      if (id) emit?.({ kind: "command", id, data: chunk.toString() });
    });
    child.stderr.on("data", (chunk) => {
      errors = (errors + chunk.toString()).slice(-12000);
    });
    let settled = false;
    let drain: ReturnType<typeof setTimeout> | undefined;
    child.once("error", (error) => {
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      clearTimeout(drain);
      if (id) commands.delete(id);
      reject(error);
    });
    const finish = (code: number | null) => {
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      clearTimeout(drain);
      if (id) commands.delete(id);
      if (code !== 0 || exceeded) {
        reject(
          new Error(
            timedOut
              ? "Opening this project took too long. Please retry."
              : exceeded
                ? "Command output exceeded its limit"
                : errors.trim() || "Command was cancelled or failed",
          ),
        );
        return;
      }
      let value: unknown = output.trim();
      try {
        value = JSON.parse(output);
      } catch {}
      resolve({ output, value });
    };
    // Output ends with the process. A server it started that kept the pipes
    // (Windows hands inheritable handles to every child) would hold "close"
    // back for as long as that server runs, so the exit is final once the
    // output has had a moment to drain.
    child.once("exit", (code) => {
      drain = setTimeout(() => finish(code), 1000);
    });
    child.once("close", (code) => finish(code));
    child.stdin.end(input === undefined ? "" : JSON.stringify(input));
  });
}
const localConnections = new Map<string, Promise<Connection>>();
export async function connectLocal(workspace: string, trust: boolean) {
  // Match the CLI launch policy, including an explicit application-wide opt-out.
  trust = trust && !["1", "true", "yes", "on"].includes(
    process.env.HAARNESS_DISABLE_PROJECT_CONFIG?.toLowerCase() ?? "",
  );
  workspace = await realpath(workspace);
  // Sidebar hydration, activation and other windows share one launch per
  // workspace. A slow project never serializes unrelated project connections.
  const key = workspace + ":" + trust;
  let pending = localConnections.get(key);
  if (!pending) {
    pending = openLocal(workspace, trust).finally(() =>
      localConnections.delete(key),
    );
    localConnections.set(key, pending);
  }
  return pending;
}
async function openLocal(workspace: string, trust: boolean) {
  const result = await invoke(workspace, trust, ["desktop"], {
    action: "connect",
  });
  const value = result.value as {
    url: string;
    token: string;
    upgradePending?: boolean;
  };
  if (typeof value.url !== "string" || typeof value.token !== "string")
    throw new Error("Invalid local server connection");
  return establish(
    workspace,
    value.url,
    value.token,
    false,
    trust,
    undefined,
    value.upgradePending,
  );
}
export async function establish(
  workspace: string,
  url: string,
  credential: string | ServerAuth,
  remote: boolean,
  trust = false,
  label?: string,
  upgradePending = false,
  remoteWorkspace?: string,
) {
  const parsed = new URL(url);
  if (
    !["http:", "https:"].includes(parsed.protocol) ||
    parsed.username ||
    parsed.password ||
    parsed.search ||
    parsed.hash
  )
    throw new Error("Use a plain HTTP(S) server URL");
  if (
    parsed.protocol === "http:" &&
    !["localhost", "127.0.0.1", "[::1]"].includes(parsed.hostname)
  )
    throw new Error(
      "Remote servers require HTTPS. For HTTP servers, connect through a local SSH tunnel.",
    );
  const id = remote
    ? remoteLocationId(url, remoteWorkspace)
    : "local:" + workspace;
  const endpoint =
    parsed.origin +
    (remoteWorkspace
      ? "/v1/host/workspaces/" + encodeURIComponent(remoteWorkspace)
      : "");
  const auth: ServerAuth =
    typeof credential === "string"
      ? credential
        ? { type: "bearer", token: credential }
        : { type: "none" }
      : credential;
  const client = new Haarness({
    baseUrl: endpoint,
    auth,
    timeoutMs: 30000,
  });
  const info = await client.info();
  if (info.name !== "haarness" || info.schemaVersion !== "1")
    throw new Error("Incompatible Haarness server");
  const view: Connection = {
    id,
    ...(remote ? { url: parsed.origin } : {}),
    label:
      label ||
      (remote ? String(info.workspace) : workspace).split(/[\\/]/).pop() ||
      parsed.hostname,
    workspace: remote ? String(info.workspace) : workspace,
    remote,
    remoteWorkspace,
    trust,
    info,
    upgradePending,
  };
  const old = connections.get(id);
  old?.client.close();
  connections.set(id, { view, client, url: endpoint, auth });
  return view;
}
export function connection(id: string) {
  const value = connections.get(id);
  if (!value) throw new Error("Open this project first");
  return value;
}
export async function command(
  id: string,
  args: string[],
  task: string,
  emit: (event: DesktopEvent) => void,
) {
  const item = connection(id);
  if (item.view.remote)
    throw new Error(
      "This operation manages the local installation. Open a local project to use it.",
    );
  localArgs(args);
  return invoke(
    item.view.workspace,
    item.view.trust,
    args,
    undefined,
    task,
    emit,
  );
}
