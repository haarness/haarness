/*
 * The MCP resources the composer's `@` list offers, per connection.
 *
 * Listing them asks every connected MCP server, so nothing asks at startup
 * or on a project switch: the first `@` in a project asks, and a later `@`
 * within a minute reuses that answer, in every composer (a chat switch or a
 * remount asks nothing). Each server is asked on its own, so one that fails
 * or is slow leaves the others' resources listed. A failed listing leaves the
 * list empty and the next `@` asks again; each ask drops the answers of other
 * connections older than a minute, so the projects visited do not pile up.
 */
import type * as T from "@haarness/sdk";
import type { ResourceContext } from "./prompt-context";

export type ResourceOption = Omit<ResourceContext, "kind">;
type Entry = { at: number; list: Promise<ResourceOption[]> };

const TTL = 60_000;
/** The most resource rows the `@` list shows. */
export const RESOURCE_ROWS = 50;
const shared = new Map<string, Entry>();

/** `connection`'s resources, asked for with `load` unless an answer younger than `ttl` is kept. */
export function cachedResources(
  connection: string,
  load: () => Promise<ResourceOption[]>,
  { ttl = TTL, cache = shared, now = Date.now() }: { ttl?: number; cache?: Map<string, Entry>; now?: number } = {},
) {
  for (const [key, entry] of cache) if (key !== connection && now - entry.at >= ttl) cache.delete(key);
  const hit = cache.get(connection);
  if (hit && now - hit.at < ttl) return hit.list;
  const entry: Entry = { at: now, list: load() };
  cache.set(connection, entry);
  entry.list.catch(() => {
    if (cache.get(connection) === entry) cache.delete(connection);
  });
  return entry.list;
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

/** Every connected server's resources, asked of each at once. */
export async function listResources(connection: string): Promise<ResourceOption[]> {
  const servers = await window.haarness.api<T.McpServerInfo[]>(connection, "mcpServers", []);
  const lists = await Promise.all(
    servers
      .filter((server) => server.status === "connected")
      .map((server) =>
        window.haarness
          .api<unknown[]>(connection, "mcpResources", [server.name])
          .then((listed) => resourceOptions(server.name, Array.isArray(listed) ? listed : []))
          .catch(() => []),
      ),
  );
  return lists.flat();
}

/**
 * The rows for what was typed after `@`, at most `limit`: matched by name
 * only (a URI or description made unrelated hits), names that start with it
 * first, then names that hold it, each in the servers' order.
 */
export function searchResources(query: string, list: ResourceOption[], limit = RESOURCE_ROWS) {
  const q = query.toLowerCase();
  if (!q) return list.slice(0, limit);
  const starts: ResourceOption[] = [];
  const holds: ResourceOption[] = [];
  for (const item of list) {
    const name = item.name.toLowerCase();
    if (name.startsWith(q)) starts.push(item);
    else if (name.includes(q)) holds.push(item);
  }
  return [...starts, ...holds].slice(0, limit);
}
