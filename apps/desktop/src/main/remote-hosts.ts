import { ApiError, Haarness, type ServerAuth, type WorkspaceListResponse } from "@haarness/sdk";
import type { Connection } from "../shared";

/** What the account bridge hands main: the current Cloud sign-in and the
 * Cloud it belongs to. It never reaches the renderer or saved projects. */
export interface RelaySignIn {
  token: string;
  apiUrl: string;
}

/** A host's projects as Desktop opens them through Cloud's relay. */
export interface HostLocation {
  view: Connection;
  client: Haarness;
  url: string;
  resolveAuth: () => Promise<ServerAuth>;
  fetch: typeof fetch;
}

// Access tokens live ten minutes; renew well before that so a request never
// leaves with one that expires on the way.
const reuseMs = 4 * 60_000;

/** Another of your computers, reached through Haarness Cloud's relay with
 * the account's own short-lived sign-in. One sign-in serves every host. */
export function createRelay(signIn: (rejected?: string) => Promise<RelaySignIn>) {
  let current: (RelaySignIn & { at: number }) | undefined;
  let pending: Promise<RelaySignIn> | undefined;
  let generation = 0;
  /** The sign-in to send now; `rejected` names one Cloud refused. */
  function credential(rejected?: string): Promise<RelaySignIn> {
    if (current && current.token !== rejected && Date.now() - current.at < reuseMs)
      return Promise.resolve(current);
    const asked = generation;
    pending ??= signIn(rejected)
      .then((value) => {
        const apiUrl = cloudOrigin(value.apiUrl);
        if (typeof value.token !== "string" || !value.token) throw new Error("Sign in to Haarness to open your other computers.");
        // A sign-out or account switch meanwhile forgot this one.
        if (asked === generation) current = { token: value.token, apiUrl, at: Date.now() };
        return { token: value.token, apiUrl };
      })
      .finally(() => {
        pending = undefined;
      });
    return pending;
  }
  /** Sends the request with the current sign-in, and once more with a fresh
   * one when Cloud refuses it. A body that cannot be read twice is sent once. */
  const relayFetch: typeof fetch = async (input, init) => {
    const first = await credential();
    const send = (token: string) => {
      const headers = new Headers(init?.headers ?? (input instanceof Request ? input.headers : undefined));
      headers.set("Authorization", `Bearer ${token}`);
      return fetch(input, { ...init, headers });
    };
    const response = await send(first.token);
    const body = init?.body;
    const replayable = body == null || typeof body === "string" || body instanceof Uint8Array || body instanceof ArrayBuffer || body instanceof URLSearchParams || body instanceof Blob;
    if (response.status !== 401 || !replayable || input instanceof Request) return response;
    await response.body?.cancel().catch(() => {});
    return send((await credential(first.token)).token);
  };
  return {
    credential,
    fetch: relayFetch,
    auth: async (): Promise<ServerAuth> => ({ type: "bearer", token: (await credential()).token }),
    /** Drops the sign-in, after a sign-out or an account switch. */
    forget() {
      generation++;
      current = undefined;
      pending = undefined;
    },
  };
}
export type Relay = ReturnType<typeof createRelay>;

/** Cloud's address, without a trailing slash. HTTPS, or HTTP on this computer
 * for a local Cloud. */
export function cloudOrigin(value: string) {
  const url = new URL(value);
  if (
    (url.protocol !== "https:" &&
      !(url.protocol === "http:" && ["localhost", "127.0.0.1", "[::1]"].includes(url.hostname))) ||
    url.username ||
    url.password ||
    url.search ||
    url.hash
  )
    throw new Error("Haarness Cloud must use HTTPS.");
  return url.href.replace(/\/+$/, "");
}

/** The host gateway of one computer, as Cloud relays it. */
export const hostRuntime = (apiUrl: string, host: string) =>
  `${cloudOrigin(apiUrl)}/api/v1/remote-hosts/${encodeURIComponent(host)}/runtime`;

export const hostServerId = (host: string) => "host:" + host;
export const hostLocationId = (host: string, workspace: string) =>
  `host:${host}/workspaces/${encodeURIComponent(workspace)}`;

/** Plain words for what Cloud says when a computer cannot be reached. */
export function hostError(error: unknown, say: (key: string) => string): Error {
  if (error instanceof ApiError) {
    if (error.status === 401) return new Error(say("Sign in to Haarness to open your other computers."));
    if (error.status === 403 || error.status === 404)
      return new Error(say("This computer is not shared with your Haarness account anymore. Share it again with haarness remote enable on that computer."));
    if (error.status === 409 && /offline|connection changed/i.test(error.message))
      return new Error(say("This computer is offline. Try again when it is awake and connected."));
  }
  if (error instanceof Error && /Sign in to Haarness|not signed in|NotLoggedIn/i.test(error.message))
    return new Error(say("Sign in to Haarness to open your other computers."));
  return error instanceof Error ? error : new Error(String(error));
}

/** A computer's host gateway (its project list, folders, opening a folder),
 * through the relay. */
async function hostGateway<T>(
  relay: Relay,
  host: string,
  say: (key: string) => string,
  call: (client: Haarness) => Promise<T>,
): Promise<T> {
  const { apiUrl } = await relay.credential().catch((error) => {
    throw hostError(error, say);
  });
  const client = new Haarness({ baseUrl: hostRuntime(apiUrl, host), fetch: relay.fetch, timeoutMs: 45_000 });
  try {
    return await call(client);
  } catch (error) {
    throw hostError(error, say);
  } finally {
    client.close();
  }
}
/** The projects a computer offers, as its own catalog lists them. */
export const hostCatalog = (relay: Relay, host: string, say: (key: string) => string) =>
  hostGateway(relay, host, say, (client) => client.hostProjects());
/** One folder of a computer, for the folder browser. */
export const hostBrowse = (
  relay: Relay,
  host: string,
  say: (key: string) => string,
  query: { path?: string; prefix?: string; hidden?: boolean; limit?: number; after?: string },
) => hostGateway(relay, host, say, (client) => client.hostDirectories(query));

/** Opens one project of another computer: a folder of its catalog, or a
 * folder to open there, as an ordinary remote project. */
export async function connectHost(
  relay: Relay,
  input: {
    host: string;
    /** The host's workspace id, from its catalog or a saved project. */
    workspace?: string;
    /** A folder to open on that computer instead. */
    path?: string;
    /** The Cloud a saved project of this computer came from. */
    apiUrl?: string;
    label?: string;
    project?: Connection["project"];
    checkout?: Connection["checkout"];
  },
  say: (key: string) => string,
): Promise<HostLocation> {
  const { apiUrl } = await relay.credential().catch((error) => {
    throw hostError(error, say);
  });
  if (input.apiUrl && cloudOrigin(input.apiUrl) !== apiUrl)
    throw new Error(say("Sign in to the Haarness Cloud this computer was opened from to open it again."));
  const runtime = hostRuntime(apiUrl, input.host);
  let workspace = input.workspace;
  let project = input.project;
  let checkout = input.checkout;
  if (input.path || !project) {
    const opened = input.path
      ? await hostGateway(relay, input.host, say, (client) => client.hostOpen({ path: input.path! }))
      : undefined;
    // The catalog names the project a folder belongs to; a project opens
    // without that when the catalog cannot be read.
    const catalog: WorkspaceListResponse | undefined = await hostCatalog(relay, input.host, say).catch(() => undefined);
    const found = opened ?? catalog?.workspaces.find((item) => item.id === workspace);
    if (opened) workspace = opened.id;
    const owner = found && catalog?.projects.find((item) => item.id === found.projectId);
    if (owner) project = { id: `host-project:${input.host}:${owner.id}`, name: owner.name, root: owner.root, git: owner.git };
    if (found) checkout = { id: found.id, name: found.name, kind: found.kind, branch: found.branch, managed: found.managed, exists: found.exists };
  }
  if (!workspace) throw new Error(say("Choose a project on that computer."));
  const url = `${runtime}/v1/host/workspaces/${encodeURIComponent(workspace)}`;
  const client = new Haarness({ baseUrl: url, resolveAuth: relay.auth, fetch: relay.fetch, timeoutMs: 30_000 });
  let info;
  try {
    info = await client.info();
  } catch (error) {
    client.close();
    throw hostError(error, say);
  }
  if (info.name !== "haarness" || info.schemaVersion !== "1") {
    client.close();
    throw new Error("Incompatible Haarness server");
  }
  const path = String(info.workspace);
  const view: Connection = {
    id: hostLocationId(input.host, workspace),
    label: path.split(/[\\/]/).filter(Boolean).pop() || input.label || path,
    workspace: path,
    remote: true,
    remoteHost: input.host,
    remoteWorkspace: workspace,
    cloudApiUrl: apiUrl,
    trust: false,
    info,
    ...(project ? { project } : {}),
    ...(checkout ? { checkout } : {}),
  };
  return { view, client, url, resolveAuth: relay.auth, fetch: relay.fetch };
}
