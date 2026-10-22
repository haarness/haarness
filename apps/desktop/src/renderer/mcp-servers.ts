/*
 * Which MCP servers have their interactive apps turned off, per connection.
 *
 * A page under a tool's row loads from its server, so a row shows the page
 * only once the server's apps are known to be on; a server whose apps are
 * off refuses its pages, and its older rows show as the plain tool. A switch
 * flipped here counts at once: the server reconnects without the app
 * extension only once its configuration has reloaded, after the work in
 * progress.
 */
import { createSignal } from "solid-js";
import type * as T from "@haarness/sdk";

/** What each connection's server last said: server name → apps off. */
const [reported, setReported] = createSignal<Record<string, Record<string, boolean>>>({});
/** Switches flipped here that the server has not reported back yet, by connection and server. */
const [flipped, setFlipped] = createSignal<Record<string, boolean>>({});
/** Servers whose settings Haarness does not edit (a plugin's, or set
 * outside the person's own files), per connection: their switch is shown
 * but cannot be flipped, and pages offer no off item. */
const [locked, setLocked] = createSignal<Record<string, Set<string>>>({});
/** The list asked for, once per connection and catalog generation. */
const asked = new Set<string>();

const key = (connection: string, server: string) => `${connection}\u0000${server}`;

/** Record a fresh server list, such as the one MCP settings just loaded. */
export function noteMcpServers(connection: string, servers: T.McpServerInfo[]) {
  setReported((all) => ({
    ...all,
    [connection]: Object.fromEntries(servers.map((server) => [server.name, server.apps === false])),
  }));
  // A flip the server now agrees with has done its work.
  setFlipped((all) => {
    const next = { ...all };
    for (const server of servers) {
      const id = key(connection, server.name);
      if (id in next && next[id] === (server.apps === false)) delete next[id];
    }
    return next;
  });
}

/** Record which servers are read-only, from the management list (`readOnly`). */
export function noteReadOnlyServers(connection: string, entries: { name?: unknown; readOnly?: unknown }[]) {
  setLocked((all) => ({
    ...all,
    [connection]: new Set(entries.filter((entry) => entry.readOnly === true).map((entry) => String(entry.name))),
  }));
}

/** The management list says which of `names` are read-only; it needs the local installation. */
export async function loadReadOnlyServers(connection: string, names: string[]) {
  const entries = await window.haarness.native<{ name?: unknown; readOnly?: unknown }[]>(connection, {
    action: "asset",
    kind: "mcp",
    input: { action: "list", names },
  });
  if (Array.isArray(entries)) noteReadOnlyServers(connection, entries);
}

/** Whether Haarness cannot change `server`'s settings on this connection. */
export const readOnlyServer = (connection: string, server: string) => !!locked()[connection]?.has(server);

/** Ask the server once for this catalog generation (`version` changes when its configuration reloads). */
export function loadMcpServers(connection: string, version: number, local = false) {
  const id = `${connection}\u0000${version}`;
  if (!connection || asked.has(id)) return;
  asked.add(id);
  void window.haarness
    .api<T.McpServerInfo[]>(connection, "mcpServers", [])
    .then((servers) => {
      noteMcpServers(connection, servers);
      if (local)
        void loadReadOnlyServers(
          connection,
          servers.map((server) => server.name),
        ).catch(() => undefined);
    })
    .catch(() => {
      asked.delete(id);
      // Without a list, rows still show their pages, as before this setting.
      if (!(connection in reported())) setReported((all) => ({ ...all, [connection]: {} }));
    });
}

/** Whether this connection's list has arrived. */
export const mcpServersKnown = (connection: string) => connection in reported();

/** Whether `server`'s interactive apps are off on this connection. */
export function appsOff(connection: string, server: string) {
  const id = key(connection, server);
  const local = flipped();
  if (id in local) return local[id];
  return reported()[connection]?.[server] ?? false;
}

/** Whether MCP settings offers the switch: a server that has pages, or one whose apps are off. */
export const offersApps = (connection: string, server: T.McpServerInfo) =>
  !!server.appTools || server.apps === false || key(connection, server.name) in flipped();

/**
 * Turns `server`'s apps on or off in the configuration file it comes from.
 * The change shows at once; if saving fails it is undone and the error thrown.
 */
export async function setApps(connection: string, server: string, on: boolean) {
  const id = key(connection, server);
  setFlipped((all) => ({ ...all, [id]: !on }));
  try {
    await window.haarness.native(connection, {
      action: "asset",
      kind: "mcp",
      input: { action: "edit", name: server, apps: on },
    });
  } catch (error) {
    setFlipped((all) => {
      const next = { ...all };
      delete next[id];
      return next;
    });
    throw error;
  }
}
