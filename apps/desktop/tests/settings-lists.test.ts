import { test } from "node:test";
import assert from "node:assert/strict";
import type { ToolCatalogEntry } from "@haarness/sdk";
import {
  definitionSource,
  homePath,
  OPEN_GROUP_LIMIT,
  splitPath,
  toolGroupKeys,
  toolGroups,
  toolSummary,
  toolTitle,
} from "../src/renderer/settings-lists";

const tool = (
  id: string,
  source = "native",
  description = "",
  [group, groupTitle] = ["core", "Core"],
): ToolCatalogEntry => ({
  id,
  source,
  description,
  advertised: true,
  enabled: true,
  essential: false,
  estimatedTokens: 10,
  group,
  groupTitle,
  loaded: false,
});

test("a path shows this computer's own home folder as ~ and keeps its last folder whole", () => {
  const home = "/Users/alex";
  assert.equal(homePath("/Users/alex/work/haarness", home), "~/work/haarness");
  assert.equal(homePath("/Users/alex", home), "~");
  assert.equal(homePath("/Users/alex/work", home + "/"), "~/work");
  assert.equal(homePath("C:\\Users\\Ana\\code\\app", "C:\\Users\\Ana"), "~\\code\\app");
  // Another user's home, a folder only named like it, or a path when the home
  // is not known (a remote server's) keeps its whole path.
  assert.equal(homePath("/Users/sam/notes", home), "/Users/sam/notes");
  assert.equal(homePath("/Users/alexandra/x", home), "/Users/alexandra/x");
  assert.equal(homePath("/Users/alex/work"), "/Users/alex/work");
  assert.equal(homePath("/home/alex/work", "/"), "/home/alex/work");
  assert.deepEqual(splitPath("/Users/alex/work/haarness", home), { head: "~/work", tail: "/haarness" });
  assert.deepEqual(splitPath("/tmp"), { head: "", tail: "/tmp" });
  assert.deepEqual(splitPath("relative"), { head: "", tail: "relative" });
  assert.deepEqual(splitPath("C:\\Users\\Ana\\code\\app", "C:\\Users\\Ana"), { head: "~\\code", tail: "\\app" });
});

test("tools group by source, and built-in tools by the catalog's groups with families split out", () => {
  const browser = ["back", "click", "cpu_analyze", "fill", "tabs_open"].map((name) => tool("browser_" + name));
  const groups = toolGroups([
    tool("mcp__weather__show_weather", "mcp:weather"),
    tool("write"),
    tool("ext__lint__run_lint", "extension:lint"),
    tool("apply_patch"),
    tool("mcp__docs__search", "mcp:docs"),
    tool("mcp__docs__read_page", "mcp:docs"),
    tool("memory_save", "native", "", ["memory", "Memory"]),
    tool("memory_list", "native", "", ["memory", "Memory"]),
    tool("agent_manage", "native", "", ["agents", "Agents"]),
    tool("mcp_manage", "native", "", ["extensions", "Extensions"]),
    ...browser,
  ]);
  assert.deepEqual(
    groups.map((group) => [group.key, group.kind, group.name, group.tools.length]),
    [
      ["builtin", "builtin", "", 11],
      ["mcp:docs", "mcp", "docs", 2],
      ["mcp:weather", "mcp", "weather", 1],
      ["extension:lint", "extension", "lint", 1],
    ],
  );
  // Core first, its browser_* family after it, then the other groups by title.
  assert.deepEqual(
    groups[0].groups!.map((part) => [part.key, part.name, part.prefix, part.tools.map((item) => item.id)]),
    [
      ["builtin/core", "Core", undefined, ["apply_patch", "write"]],
      ["builtin/browser", "Browser", "browser", ["browser_back", "browser_click", "browser_cpu_analyze", "browser_fill", "browser_tabs_open"]],
      ["builtin/agents", "Agents", undefined, ["agent_manage"]],
      // The tools that manage extensions are not the extensions beside Built-in.
      ["builtin/extensions", "Manage extensions", undefined, ["mcp_manage"]],
      ["builtin/memory", "Memory", undefined, ["memory_list", "memory_save"]],
    ],
  );
  assert.equal(groups[1].groups, undefined);
  assert.deepEqual(toolGroupKeys(groups, "browser_click"), ["builtin", "builtin/browser"]);
  assert.deepEqual(toolGroupKeys(groups, "mcp__docs__search"), ["mcp:docs"]);
  assert.deepEqual(toolGroupKeys(groups, "missing"), []);
  // A family as large as its group, or named like it, is that group already.
  const small = toolGroups(browser.slice(0, 4).concat(tool("bash")));
  assert.deepEqual(small[0].groups!.map((part) => part.key), ["builtin/core"]);
  assert.ok(OPEN_GROUP_LIMIT >= 1);
});

test("a tool reads by a careful title and the first sentence of its description", () => {
  assert.equal(toolTitle(tool("apply_patch")), "Apply patch");
  assert.equal(toolTitle(tool("bash")), "Bash");
  assert.equal(toolTitle(tool("lsp")), "LSP");
  assert.equal(toolTitle(tool("mcp_manage")), "MCP manage");
  assert.equal(toolTitle(tool("webfetch")), "Web fetch");
  assert.equal(toolTitle(tool("todowrite")), "Todo write");
  assert.equal(toolTitle(tool("multiedit")), "Multi edit");
  assert.equal(toolTitle(tool("browser_cpu_analyze")), "Browser CPU analyze");
  assert.equal(toolTitle(tool("browser_cpu_analyze"), "browser"), "CPU analyze");
  assert.equal(toolTitle(tool("browser_network_get"), "browser"), "Network get");
  assert.equal(toolTitle(tool("mcp__weather__show_weather")), "Show weather");
  assert.equal(toolTitle(tool("mcp__my_server__list-items")), "List items");
  assert.equal(toolTitle(tool("mcp__web__fetchUrl")), "Fetch URL");
  assert.equal(toolTitle(tool("ext__lint__run_lint")), "Run lint");
  // A server's one-word name is not guessed at.
  assert.equal(toolTitle(tool("mcp__weather__getweather")), "getweather");
  assert.equal(toolTitle(tool("mcp__weather__api")), "API");
  assert.equal(
    toolSummary("Run a terminal command through the workspace shell. Use bash for builds, tests, git."),
    "Run a terminal command through the workspace shell.",
  );
  assert.equal(toolSummary("  Manage\nlocal agents  "), "Manage local agents");
  // A version number is not the end of a sentence.
  assert.equal(toolSummary("Apply a V4A patch v1.2 envelope. Then more."), "Apply a V4A patch v1.2 envelope.");
  const long = toolSummary("word ".repeat(60).trim() + ".");
  assert.ok(long.length <= 141, long);
  assert.ok(long.endsWith("…"));
  assert.ok(!long.includes("  "));
  assert.equal(toolSummary(""), "");
});

test("a definition says it is built in or names its plugin, never both at once", () => {
  const plugins = [
    { name: "kit", path: "/data/plugins/kit" },
    { name: "kit-extra", path: "/data/plugins/kit-extra" },
    { name: "win", path: "C:\\Users\\Ana\\plugins\\win\\" },
  ];
  assert.equal(definitionSource({ name: "build", native: true }, plugins), "builtin");
  assert.equal(definitionSource({ name: "kit:reviewer" }, plugins), "kit");
  assert.equal(definitionSource({ name: "deploy", plugin: "kit" }, plugins), "kit");
  assert.equal(
    definitionSource({ name: "check", path: "/data/plugins/kit-extra/skills/check/SKILL.md", pluginManaged: true }, plugins),
    "kit-extra",
  );
  assert.equal(
    definitionSource({ name: "lint", path: "C:\\Users\\Ana\\plugins\\win\\skills\\lint\\SKILL.md" }, plugins),
    "win",
  );
  // The person's own definitions say their scope instead; others are not guessed.
  assert.equal(definitionSource({ name: "mine", scope: "global" }, plugins), undefined);
  assert.equal(definitionSource({ name: "base-agent", scope: "system" }, plugins), undefined);
  assert.equal(definitionSource({ name: "other:agent" }, plugins), undefined);
  assert.equal(definitionSource({ name: "claude-agent" }, plugins), undefined);
  assert.equal(definitionSource({ name: "near", path: "/data/plugins/kitchen/x.md" }, plugins), undefined);
});
