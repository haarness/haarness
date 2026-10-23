import type { PluginInfo, ToolCatalogEntry } from "@haarness/sdk";

/**
 * How Settings lists read: tools by where they come from, a path as a person
 * reads it, and where an agent, command or skill comes from. Pure, so the
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

export type ToolGroupKind = "builtin" | "mcp" | "extension";

export interface ToolGroup {
  key: string;
  kind: ToolGroupKind;
  /** The server or extension name; built-in tools take their title from the caller. */
  name: string;
  tools: ToolCatalogEntry[];
}

/** Groups this size and smaller start open; larger ones start folded. */
export const OPEN_GROUP_LIMIT = 6;

function sourceOf(tool: ToolCatalogEntry): { kind: ToolGroupKind; name: string } {
  const [kind, ...rest] = tool.source.split(":");
  const name = rest.join(":");
  if (kind === "mcp" && name) return { kind: "mcp", name };
  if (kind === "extension" && name) return { kind: "extension", name };
  return { kind: "builtin", name: "" };
}

/** Built-in tools first, then each MCP server, then each extension, by name. */
export function toolGroups(tools: ToolCatalogEntry[]): ToolGroup[] {
  const groups = new Map<string, ToolGroup>();
  for (const tool of tools) {
    const { kind, name } = sourceOf(tool);
    const key = kind === "builtin" ? "builtin" : `${kind}:${name}`;
    const group = groups.get(key) ?? { key, kind, name, tools: [] };
    group.tools.push(tool);
    groups.set(key, group);
  }
  const rank: Record<ToolGroupKind, number> = { builtin: 0, mcp: 1, extension: 2 };
  for (const group of groups.values())
    group.tools.sort((a, b) => toolTitle(a).localeCompare(toolTitle(b)) || a.id.localeCompare(b.id));
  return [...groups.values()].sort(
    (a, b) => rank[a.kind] - rank[b.kind] || a.name.localeCompare(b.name),
  );
}

/** The group a tool is listed under, for opening it on a search jump. */
export function toolGroupKey(tool: ToolCatalogEntry) {
  const { kind, name } = sourceOf(tool);
  return kind === "builtin" ? "builtin" : `${kind}:${name}`;
}

/** "apply_patch" → "Apply patch"; an MCP or extension tool drops its server prefix. */
export function toolTitle(tool: Pick<ToolCatalogEntry, "id">) {
  const bare = tool.id.replace(/^(?:mcp|ext)__.*?__/, "");
  const words = bare.replace(/[_-]+/g, " ").replace(/\s+/g, " ").trim() || tool.id;
  return words.charAt(0).toUpperCase() + words.slice(1);
}

const SUMMARY_LIMIT = 140;

/** The description's first sentence, on one line and cut at a word. */
export function toolSummary(description: string) {
  const text = description.replace(/\s+/g, " ").trim();
  const end = text.search(/[.!?。](?=\s|$)/);
  const sentence = end >= 0 ? text.slice(0, end + 1) : text;
  if (sentence.length <= SUMMARY_LIMIT) return sentence;
  const cut = sentence.lastIndexOf(" ", SUMMARY_LIMIT - 1);
  return sentence.slice(0, cut > SUMMARY_LIMIT / 2 ? cut : SUMMARY_LIMIT - 1).replace(/[,;:]$/, "") + "…";
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
