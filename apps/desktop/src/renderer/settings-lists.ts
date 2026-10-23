import type { PluginInfo, ToolCatalogEntry } from "@haarness/sdk";

/**
 * How Settings lists read: tools by where they come from, a path as a person
 * reads it, and where an agent, command or skill comes from. Pure, so the
 * rules are pinned in tests rather than found in the panes.
 */

/**
 * A path as a person reads it: this computer's own home folder as `~`. Only
 * the home Desktop runs in is known, so another user's folder, or a remote
 * server's, keeps its whole path.
 */
export function homePath(path: string, home?: string) {
  if (!home || home === "/" || home === "\\") return path;
  const root = home.replace(/[/\\]+$/, "");
  if (path === root) return "~";
  return path.startsWith(root + "/") || path.startsWith(root + "\\") ? "~" + path.slice(root.length) : path;
}

/**
 * A long path keeps its last folder in sight and gives way in the middle:
 * the head is the part that may shrink to an ellipsis, the tail (with the
 * separator before it, so a cut reads "…/folder") never does.
 */
export function splitPath(path: string, home?: string) {
  const shown = homePath(path, home);
  const cut = Math.max(shown.lastIndexOf("/"), shown.lastIndexOf("\\"));
  if (cut <= 0) return { head: "", tail: shown };
  return { head: shown.slice(0, cut), tail: shown.slice(cut) };
}

export type ToolGroupKind = "builtin" | "mcp" | "extension";

export interface ToolGroup {
  /** "builtin", "mcp:<server>", "extension:<name>"; a built-in part is "builtin/<group>". */
  key: string;
  kind: ToolGroupKind;
  /**
   * The server or extension name; for a part of the built-in tools, the
   * catalog's group title ("Core", "Memory") or the family's ("Browser").
   * The whole built-in group takes its title from the caller.
   */
  name: string;
  tools: ToolCatalogEntry[];
  /** Built-in tools only: the catalog's groups, families split out. */
  groups?: ToolGroup[];
  /** A family split out by name: the prefix its titles leave out. */
  prefix?: string;
}

/** Groups this size and smaller start open; larger ones start folded. */
export const OPEN_GROUP_LIMIT = 6;
/** Tools of one catalog group sharing a name prefix this often are their own part. */
const FAMILY_SIZE = 5;

function sourceOf(tool: ToolCatalogEntry): { kind: ToolGroupKind; name: string } {
  const [kind, ...rest] = tool.source.split(":");
  const name = rest.join(":");
  if (kind === "mcp" && name) return { kind: "mcp", name };
  if (kind === "extension" && name) return { kind: "extension", name };
  return { kind: "builtin", name: "" };
}

const byTitle = (prefix?: string) => (a: ToolCatalogEntry, b: ToolCatalogEntry) =>
  toolTitle(a, prefix).localeCompare(toolTitle(b, prefix)) || a.id.localeCompare(b.id);

/**
 * The built-in tools by the catalog's own groups (the TUI's), Core first. A
 * family a group holds, such as the browser_* tools in Core, is a part of its
 * own after its group, named by its prefix.
 */
function builtinParts(tools: ToolCatalogEntry[]): ToolGroup[] {
  const buckets = new Map<string, { title: string; tools: ToolCatalogEntry[] }>();
  for (const tool of tools) {
    const key = tool.group || "core";
    const bucket = buckets.get(key) ?? { title: tool.groupTitle || key, tools: [] };
    bucket.tools.push(tool);
    buckets.set(key, bucket);
  }
  const parts: ToolGroup[] = [];
  const order = [...buckets.keys()].sort((a, b) =>
    a === "core" ? -1 : b === "core" ? 1 : buckets.get(a)!.title.localeCompare(buckets.get(b)!.title),
  );
  for (const key of order) {
    const bucket = buckets.get(key)!;
    const families = new Map<string, ToolCatalogEntry[]>();
    for (const tool of bucket.tools) {
      const prefix = tool.id.includes("_") ? tool.id.slice(0, tool.id.indexOf("_")) : "";
      // A group named after the prefix ("memory", "sessions") is that family already.
      if (!prefix || key.startsWith(prefix) || prefix.startsWith(key)) continue;
      families.set(prefix, [...(families.get(prefix) ?? []), tool]);
    }
    const split = [...families].filter(([, members]) => members.length >= FAMILY_SIZE && members.length < bucket.tools.length);
    const rest = bucket.tools.filter((tool) => !split.some(([, members]) => members.includes(tool)));
    if (rest.length)
      parts.push({ key: "builtin/" + key, kind: "builtin", name: bucket.title, tools: rest.sort(byTitle()) });
    for (const [prefix, members] of split.sort(([a], [b]) => a.localeCompare(b)))
      parts.push({
        key: "builtin/" + prefix,
        kind: "builtin",
        name: humanize([prefix]),
        prefix,
        tools: members.sort(byTitle(prefix)),
      });
  }
  return parts;
}

/** Built-in tools first, in their parts, then each MCP server, then each extension, by name. */
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
  for (const group of groups.values()) {
    group.tools.sort(byTitle());
    if (group.kind === "builtin") group.groups = builtinParts(group.tools);
  }
  return [...groups.values()].sort(
    (a, b) => rank[a.kind] - rank[b.kind] || a.name.localeCompare(b.name),
  );
}

/** The keys of the group, and part, that list a tool, for opening them on a search jump. */
export function toolGroupKeys(groups: ToolGroup[], id: string): string[] {
  for (const group of groups) {
    const part = group.groups?.find((candidate) => candidate.tools.some((tool) => tool.id === id));
    if (part) return [group.key, part.key];
    if (group.tools.some((tool) => tool.id === id)) return [group.key];
  }
  return [];
}

/** Words written in capitals wherever they appear in a tool's name. */
const ACRONYMS = new Set([
  "ai", "api", "cli", "cpu", "css", "csv", "dns", "gpu", "html", "http", "https", "id", "io", "ip", "js",
  "json", "jsonl", "llm", "lsp", "mcp", "os", "pdf", "ram", "sdk", "sql", "ssh", "svg", "tcp", "tls",
  "ts", "tui", "ui", "uri", "url", "urls", "xml", "yaml",
]);
/** Haarness's own tools whose name runs two words together. */
const COMPOUNDS: Record<string, string[]> = {
  multiedit: ["multi", "edit"],
  todoread: ["todo", "read"],
  todowrite: ["todo", "write"],
  webfetch: ["web", "fetch"],
};

function humanize(words: string[]) {
  const text = words
    .map((word) => (ACRONYMS.has(word.toLowerCase()) ? word.toUpperCase() : word.toLowerCase()))
    .join(" ");
  return text.charAt(0).toUpperCase() + text.slice(1);
}

/**
 * A tool's title, from its name: "apply_patch" → "Apply patch", "lsp" → "LSP",
 * "browser_cpu_analyze" in the Browser part → "CPU analyze". An MCP or
 * extension tool drops its server prefix; one whose name is a single word of
 * its own ("getweather") keeps it as written rather than guess at it.
 */
export function toolTitle(tool: Pick<ToolCatalogEntry, "id">, prefix?: string) {
  let bare = tool.id.replace(/^(?:mcp|ext)__.*?__/, "");
  const native = bare === tool.id;
  if (prefix && bare.startsWith(prefix + "_") && bare.length > prefix.length + 1) bare = bare.slice(prefix.length + 1);
  const words = bare
    .replace(/([a-z0-9])([A-Z])/g, "$1 $2")
    .split(/[\s_-]+/)
    .filter(Boolean);
  if (!words.length) return tool.id;
  if (words.length === 1) {
    const word = words[0].toLowerCase();
    if (COMPOUNDS[word]) return humanize(COMPOUNDS[word]);
    if (!native && !ACRONYMS.has(word)) return bare;
  }
  return humanize(words);
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
    // Compared with one separator, so a Windows server's paths match too.
    const plain = (path: string) => path.replace(/\\/g, "/").replace(/\/+$/, "");
    const path = plain(row.path);
    const owner = plugins
      .filter((plugin) => plugin.path && (path === plain(plugin.path) || path.startsWith(plain(plugin.path) + "/")))
      .sort((a, b) => b.path.length - a.path.length)[0];
    if (owner) return owner.name;
  }
  return undefined;
}
