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
}) {
  if (!location.remote) return localServer.id;
  return location.url
    ? "remote:" + new URL(location.url).origin
    : location.id;
}

export function migrateServers(recent: Recent[], saved: SavedServer[] = []) {
  const servers = new Map(saved.map((server) => [server.id, server]));
  for (const location of recent) {
    if (!location.remote || !location.url) continue;
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
