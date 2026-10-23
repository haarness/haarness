import type { PluginInfo } from "@haarness/sdk";

/**
 * How Settings lists read: a path as a person reads it, and where an agent,
 * command or skill comes from. Pure, so the
 * rules are pinned in tests rather than found in the panes.
 */

/** A home folder in the usual places, which a path then shows as `~`. The
 * renderer is not told the home folder, and a remote server has its own. */
const home = /^(?:\/Users\/[^/]+|\/home\/[^/]+|\/root|[A-Za-z]:\\Users\\[^\\]+)(?=[/\\]|$)/;

export function homePath(path: string) {
  return path.replace(home, "~");
}

/**
 * A long path keeps its last folder in sight and gives way in the middle:
 * the head is the part that may shrink to an ellipsis, the tail (with the
 * separator before it, so a cut reads "…/folder") never does.
 */
export function splitPath(path: string) {
  const shown = homePath(path);
  const cut = Math.max(shown.lastIndexOf("/"), shown.lastIndexOf("\\"));
  if (cut <= 0) return { head: "", tail: shown };
  return { head: shown.slice(0, cut), tail: shown.slice(cut) };
}

export interface DefinitionRow {
  name: string;
  scope?: string;
  native?: boolean;
  /** A command's plugin, as the server names it. */
  plugin?: string | null;
  path?: string;
  pluginManaged?: boolean;
}

/**
 * Where an agent, command or skill that is not the person's own comes from:
 * "builtin", the plugin's name, or undefined when neither is known (a
 * compatible folder of another tool). Plugin agents are named `plugin:agent`.
 */
export function definitionSource(
  row: DefinitionRow,
  plugins: Pick<PluginInfo, "name" | "path">[],
): string | undefined {
  if (row.scope) return undefined;
  if (row.native) return "builtin";
  if (row.plugin) return row.plugin;
  const prefix = row.name.includes(":") ? row.name.slice(0, row.name.indexOf(":")) : "";
  if (prefix && plugins.some((plugin) => plugin.name === prefix)) return prefix;
  if (row.path) {
    const owner = plugins
      .filter((plugin) => plugin.path && (row.path === plugin.path || row.path!.startsWith(plugin.path.replace(/[/\\]$/, "") + "/")))
      .sort((a, b) => b.path.length - a.path.length)[0];
    if (owner) return owner.name;
  }
  return undefined;
}
