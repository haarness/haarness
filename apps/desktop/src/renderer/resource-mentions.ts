/*
 * The MCP resources the composer's `@` list offers, per connection.
 *
 * Listing them asks every connected MCP server, so nothing asks at startup
 * or on a project switch: the first `@` in a project asks, and a later `@`
 * within a minute reuses that answer, in every composer (a chat switch or a
 * remount asks nothing). Each server is asked on its own and its rows join
 * the list as they come, below the rows already there; the composer shows
 * them only once the files a query finds have come (`PromptContext.tsx`), so
 * they always join below the files too. A slow or failing server holds
 * nothing up, and the highlighted row never moves. A failed listing is
 * forgotten and the next `@` asks again; each ask drops the answers of
 * other connections older than a minute, so the projects visited do not
 * pile up. Which servers a chat's rules take away entirely is asked
 * each time the list opens (a cheap local question) and those are left out.
 */
import { createSignal, type Accessor } from "solid-js";
import type * as T from "@haarness/sdk";
import type { ResourceContext } from "./prompt-context";

export type ResourceOption = Omit<ResourceContext, "kind" | "block">;
export type Entry = {
  at: number;
  /** The rows so far, each server's in the order the servers answered. */
  rows: Accessor<ResourceOption[]>;
  /** Settles once every server has answered or failed. */
  done: Promise<void>;
};

const TTL = 60_000;
/** The most resource rows the `@` list shows. */
export const RESOURCE_ROWS = 50;
const shared = new Map<string, Entry>();

/**
 * `connection`'s rows, listed with `load` unless a listing younger than
 * `ttl` is kept. `load` hands each server's rows to `add` as they come; the
 * rows of an older listing stay until the first of the new ones arrive.
 */
export function resourceEntry(
  connection: string,
  load: (add: (rows: ResourceOption[]) => void) => Promise<void>,
  { ttl = TTL, cache = shared, now = Date.now() }: { ttl?: number; cache?: Map<string, Entry>; now?: number } = {},
): Entry {
  for (const [key, entry] of cache) if (key !== connection && now - entry.at >= ttl) cache.delete(key);
  const hit = cache.get(connection);
  if (hit && now - hit.at < ttl) return hit;
  const [rows, setRows] = createSignal<ResourceOption[]>(hit?.rows() ?? []);
  let fresh = true;
  const entry: Entry = { at: now, rows, done: Promise.resolve() };
  entry.done = load((answer) => {
    setRows(fresh ? answer : [...rows(), ...answer]);
    fresh = false;
  }).then(
    () => {
      if (fresh) setRows([]);
    },
    () => {
      if (cache.get(connection) === entry) cache.delete(connection);
    },
  );
  cache.set(connection, entry);
  return entry;
}

const text = (value: unknown) => (typeof value === "string" && value.trim() ? value : undefined);

/** One server's listing as `@` rows: entries without a URI are left out. */
export function resourceOptions(server: string, listed: unknown[]): ResourceOption[] {
  return listed.flatMap((entry) => {
    if (!entry || typeof entry !== "object") return [];
    const value = entry as Record<string, unknown>;
    const uri = text(value.uri);
    if (!uri) return [];
    return [
      {
        server,
        uri,
        name: text(value.name) ?? uri,
        description: text(value.description) ?? text(value.title),
        mimeType: text(value.mimeType),
      },
    ];
  });
}

/** Asks each of `servers` for its resources at once, handing each answer to `add` as it comes. */
export async function listResources(
  connection: string,
  servers: T.McpServerInfo[],
  add: (rows: ResourceOption[]) => void,
) {
  await Promise.all(
    servers
      .filter((server) => server.status === "connected")
      .map((server) =>
        window.haarness
          .api<unknown[]>(connection, "mcpResources", [server.name])
          .then((listed) => {
            const rows = resourceOptions(server.name, Array.isArray(listed) ? listed : []);
            if (rows.length) add(rows);
          })
          .catch(() => undefined),
      ),
  );
}

/**
 * The rows for what was typed after `@`, at most `limit`: matched by name
 * only (a URI or description made unrelated hits), each server's rows in
 * the order the servers answered, names that start with it first within a
 * server, and none of a server in `off` (one the chat's rules take away).
 */
export function searchResources(
  query: string,
  list: ResourceOption[],
  off: ReadonlySet<string> = new Set(),
  limit = RESOURCE_ROWS,
) {
  const q = query.toLowerCase();
  const servers = [...new Set(list.map((item) => item.server))].filter((server) => !off.has(server));
  return servers
    .flatMap((server) => {
      const rows = list.filter((item) => item.server === server && item.name.toLowerCase().includes(q));
      return [
        ...rows.filter((item) => item.name.toLowerCase().startsWith(q)),
        ...rows.filter((item) => !item.name.toLowerCase().startsWith(q)),
      ];
    })
    .slice(0, limit);
}
