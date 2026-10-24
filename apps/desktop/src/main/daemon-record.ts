import { readdir, readFile, realpath } from "node:fs/promises";
import { isAbsolute, join } from "node:path";

/**
 * Whether a folder's Haarness daemon is running now, read from the record
 * it leaves for its clients, `<cache>/workspaces/<id>/runtime.json`, without
 * starting, locking or replacing anything (as Chrome's bridge finds daemons,
 * `crates/haarness-cli/src/browser/discovery.rs`). Records outlive crashed
 * servers and ports are reused, so a record counts only when its process is
 * alive and the server at its address answers to its token.
 */

type Environment = Record<string, string | undefined>;

/** The cache folder `haarness` uses with this environment, as `Paths::new`
 * resolves it (`layout.rs`): `HAARNESS_DATA_DIR`'s, else `HAARNESS_HOME`'s,
 * else the XDG cache folder's `haarness`. */
export function cliCacheFolder(env: Environment, home: string): string | undefined {
  const data = env.HAARNESS_DATA_DIR;
  if (data && isAbsolute(data)) return join(data, "cache");
  const root = env.HAARNESS_HOME;
  if (root) return isAbsolute(root) ? join(root, "cache") : undefined;
  const xdg = env.XDG_CACHE_HOME;
  return join(xdg && isAbsolute(xdg) ? xdg : join(home, ".cache"), "haarness");
}

/** A record's address when it is on this computer: a token is only ever
 * sent there. */
function loopback(url: unknown): string | undefined {
  if (typeof url !== "string") return undefined;
  try {
    const parsed = new URL(url);
    if (parsed.protocol !== "http:" || !parsed.port) return undefined;
    const host = parsed.hostname.replace(/^\[|\]$/g, "");
    if (!["127.0.0.1", "::1", "localhost", "0.0.0.0", "::"].includes(host)) return undefined;
    if (host === "0.0.0.0") parsed.hostname = "127.0.0.1";
    if (host === "::") parsed.hostname = "[::1]";
    return parsed.origin;
  } catch {
    return undefined;
  }
}

function processAlive(pid: number) {
  try {
    process.kill(pid, 0);
    return true;
  } catch (error) {
    // Alive, and someone else's to signal.
    return (error as NodeJS.ErrnoException).code === "EPERM";
  }
}

/** What `/v1/info` says of the server answering at a record's address. */
export interface ServerInfo {
  name?: unknown;
  pid?: unknown;
  workspace?: unknown;
}

/** The server's own word, read with the record's token. A redirect is not
 * followed (the token would go along) and counts as no answer. */
async function info(origin: string, token: string): Promise<ServerInfo | undefined> {
  try {
    const response = await fetch(origin + "/v1/info", {
      headers: { authorization: "Bearer " + token },
      redirect: "manual",
      signal: AbortSignal.timeout(750),
    });
    if (response.status !== 200) return undefined;
    const value: unknown = await response.json();
    return value && typeof value === "object" ? (value as ServerInfo) : undefined;
  } catch {
    return undefined;
  }
}

/**
 * The folder a record's server serves, when the server answering is the one
 * the record names, on the terms of the Chrome bridge's `judge()`: it calls
 * itself Haarness, its process is the record's, and the folder it names is
 * the record's. Folders are compared as text: both come from the server's
 * own canonical path.
 */
export function servedFolder(
  record: { pid: number; workspace?: string },
  answer: ServerInfo,
): string | undefined {
  if (answer.name !== "haarness") return undefined;
  if (answer.pid !== undefined && answer.pid !== null && answer.pid !== record.pid) return undefined;
  const actual = typeof answer.workspace === "string" ? answer.workspace : undefined;
  if (actual !== undefined && record.workspace !== undefined && actual !== record.workspace)
    return undefined;
  return actual ?? record.workspace;
}

export async function daemonRunning(
  workspace: string,
  options: {
    env?: Environment;
    home: string;
    alive?: (pid: number) => boolean;
    probe?: (origin: string, token: string) => Promise<ServerInfo | undefined>;
  },
): Promise<boolean> {
  const cache = cliCacheFolder(options.env ?? process.env, options.home);
  if (!cache) return false;
  // The runtime records canonical paths: one resolution, of the chat home.
  const wanted = await realpath(workspace).catch(() => undefined);
  if (!wanted) return false;
  const alive = options.alive ?? processAlive;
  const probe = options.probe ?? info;
  const folders = await readdir(join(cache, "workspaces")).catch(() => [] as string[]);
  for (const folder of folders) {
    let record: { url?: unknown; token?: unknown; pid?: unknown; workspace?: unknown };
    try {
      record = JSON.parse(await readFile(join(cache, "workspaces", folder, "runtime.json"), "utf8"));
    } catch {
      continue;
    }
    if (record.workspace !== wanted) continue;
    const origin = loopback(record.url);
    if (!origin || typeof record.token !== "string") continue;
    if (typeof record.pid !== "number" || record.pid <= 0 || !alive(record.pid)) continue;
    const answer = await probe(origin, record.token);
    if (answer && servedFolder({ pid: record.pid, workspace: wanted }, answer) === wanted) return true;
  }
  return false;
}
