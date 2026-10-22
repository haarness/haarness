import type { Recent, SavedServer } from "./shared";

export const localServer: SavedServer = {
  id: "local",
  label: "This computer",
};

/** Local directory daemons share one server choice; remote origins stay separate. */
export function serverKey(location: {
  id: string;
  remote: boolean;
  url?: string;
  cloudAccount?: string;
  cloudApiUrl?: string;
  remoteHost?: string;
}) {
  if (!location.remote) return localServer.id;
  // Each of your other computers is a place of its own, whichever Cloud
  // address relays it.
  if (location.remoteHost) return "host:" + location.remoteHost;
  // Every Cloud session of one account is one place to switch to. Sessions are
  // reached through the Cloud gateway's URL, but that origin is not a server
  // the user added and must not become one.
  if (location.cloudAccount)
    return `cloud:${location.cloudApiUrl ?? ""}:${location.cloudAccount}`;
  if (location.url) return "remote:" + new URL(location.url).origin;
  return location.id;
}

/** The Haarness Cloud entries implied by opened Cloud sessions, one per account. */
export function cloudServers(recent: Recent[]): SavedServer[] {
  const servers = new Map<string, SavedServer>();
  for (const location of recent)
    if (location.remote && location.cloudAccount) {
      const id = serverKey(location);
      if (!servers.has(id)) servers.set(id, { id, label: "Haarness Cloud", cloud: true });
    }
  return [...servers.values()];
}

/** The server a window shows once it leaves a removed project: its own while
 * it is still listed, else this computer (a Cloud account's entry goes with
 * its last project). */
export function serverAfterRemoval(servers: SavedServer[], active: string) {
  return servers.some((item) => item.id === active) ? active : localServer.id;
}

export function migrateServers(recent: Recent[], saved: SavedServer[] = []) {
  // Earlier versions saved a Cloud gateway as a remote server the first time a
  // Cloud session opened. Drop those unless a real remote project uses them.
  const gateways = new Set(
    recent
      .filter((location) => location.cloudAccount && location.url)
      .map((location) => "remote:" + new URL(location.url!).origin),
  );
  const used = new Set(
    recent.filter((location) => !location.cloudAccount).map(serverKey),
  );
  const servers = new Map(
    saved
      .filter((server) => !gateways.has(server.id) || used.has(server.id))
      .map((server) => [server.id, server]),
  );
  for (const location of recent) {
    if (!location.remote || !location.url || location.cloudAccount) continue;
    const id = serverKey(location);
    if (!servers.has(id))
      servers.set(id, {
        id,
        url: new URL(location.url).origin,
        label: location.label,
      });
  }
  return [...servers.values()];
}

export function serverProjects(recent: Recent[], server: string) {
  return recent.filter((location) => serverKey(location) === server);
}

/** Your other computers to show: online ones first, by name, then the ones
 * that are away, most recently seen first. */
export function sortComputers<T extends { name: string; connected: boolean; lastSeenAt?: string | null }>(
  computers: T[],
  locale?: string,
) {
  const seen = (item: T) => Date.parse(item.lastSeenAt ?? "") || 0;
  return [...computers].sort(
    (a, b) =>
      Number(b.connected) - Number(a.connected) ||
      (a.connected ? 0 : seen(b) - seen(a)) ||
      a.name.localeCompare(b.name, locale, { sensitivity: "base" }),
  );
}
