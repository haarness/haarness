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

async function answers(origin: string, token: string) {
  try {
    const response = await fetch(origin + "/v1/info", {
      headers: { authorization: "Bearer " + token },
      signal: AbortSignal.timeout(750),
    });
    return response.ok;
  } catch {
    return false;
  }
}

export async function daemonRunning(
  workspace: string,
  options: {
    env?: Environment;
    home: string;
    alive?: (pid: number) => boolean;
    probe?: (origin: string, token: string) => Promise<boolean>;
  },
): Promise<boolean> {
  const cache = cliCacheFolder(options.env ?? process.env, options.home);
  if (!cache) return false;
  const wanted = await realpath(workspace).catch(() => undefined);
  if (!wanted) return false;
  const alive = options.alive ?? processAlive;
  const probe = options.probe ?? answers;
  const folders = await readdir(join(cache, "workspaces")).catch(() => [] as string[]);
  for (const folder of folders) {
    let record: { url?: unknown; token?: unknown; pid?: unknown; workspace?: unknown };
    try {
      record = JSON.parse(await readFile(join(cache, "workspaces", folder, "runtime.json"), "utf8"));
    } catch {
      continue;
    }
    if (typeof record.workspace !== "string") continue;
    if ((await realpath(record.workspace).catch(() => record.workspace)) !== wanted) continue;
    const origin = loopback(record.url);
    if (!origin || typeof record.token !== "string") continue;
    if (typeof record.pid !== "number" || record.pid <= 0 || !alive(record.pid)) continue;
    if (await probe(origin, record.token)) return true;
  }
  return false;
}
