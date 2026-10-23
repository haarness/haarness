import { test } from "node:test";
import assert from "node:assert/strict";
import type { ToolCatalogEntry } from "@haarness/sdk";
import {
  definitionSource,
  homePath,
  OPEN_GROUP_LIMIT,
  splitPath,
  toolGroupKey,
  toolGroups,
  toolSummary,
  toolTitle,
} from "../src/renderer/settings-lists";

const tool = (id: string, source = "native", description = ""): ToolCatalogEntry => ({
  id,
  source,
  description,
  advertised: true,
  enabled: true,
  essential: false,
  estimatedTokens: 10,
  group: "core",
  groupTitle: "Core",
  loaded: false,
});

test("a path shows the home folder as ~ and keeps its last folder whole", () => {
  assert.equal(homePath("/Users/alex/work/haarness"), "~/work/haarness");
  assert.equal(homePath("/home/sam/notes"), "~/notes");
  assert.equal(homePath("/Users/alex"), "~");
  assert.equal(homePath("C:\\Users\\Ana\\code\\app"), "~\\code\\app");
  // A folder outside a home folder, or one only named like it, stays as it is.
  assert.equal(homePath("/private/var/folders/x/T/fixture"), "/private/var/folders/x/T/fixture");
  assert.equal(homePath("/Users2/alex/x"), "/Users2/alex/x");
  assert.deepEqual(splitPath("/Users/alex/work/haarness"), { head: "~/work", tail: "/haarness" });
  assert.deepEqual(splitPath("/tmp"), { head: "", tail: "/tmp" });
  assert.deepEqual(splitPath("relative"), { head: "", tail: "relative" });
  assert.deepEqual(splitPath("C:\\Users\\Ana\\code\\app"), { head: "~\\code", tail: "\\app" });
});

test("tools group by source: built-in first, then each MCP server, then each extension", () => {
  const groups = toolGroups([
    tool("mcp__weather__show_weather", "mcp:weather"),
    tool("write"),
    tool("ext__lint__run_lint", "extension:lint"),
    tool("apply_patch"),
    tool("mcp__docs__search", "mcp:docs"),
    tool("mcp__docs__read_page", "mcp:docs"),
  ]);
  assert.deepEqual(
    groups.map((group) => [group.key, group.kind, group.name, group.tools.map((item) => item.id)]),
    [
      ["builtin", "builtin", "", ["apply_patch", "write"]],
      ["mcp:docs", "mcp", "docs", ["mcp__docs__read_page", "mcp__docs__search"]],
      ["mcp:weather", "mcp", "weather", ["mcp__weather__show_weather"]],
      ["extension:lint", "extension", "lint", ["ext__lint__run_lint"]],
    ],
  );
  assert.equal(toolGroupKey(tool("mcp__docs__search", "mcp:docs")), "mcp:docs");
  assert.equal(toolGroupKey(tool("bash")), "builtin");
  assert.ok(OPEN_GROUP_LIMIT >= 1);
});

test("a tool reads by a human title and the first sentence of its description", () => {
  assert.equal(toolTitle(tool("apply_patch")), "Apply patch");
  assert.equal(toolTitle(tool("bash")), "Bash");
  assert.equal(toolTitle(tool("mcp__weather__show_weather")), "Show weather");
  assert.equal(toolTitle(tool("mcp__my_server__list-items")), "List items");
  assert.equal(toolTitle(tool("ext__lint__run_lint")), "Run lint");
  assert.equal(toolTitle(tool("mcp_resource_read")), "Mcp resource read");
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
  ];
  assert.equal(definitionSource({ name: "build", native: true }, plugins), "builtin");
  assert.equal(definitionSource({ name: "kit:reviewer" }, plugins), "kit");
  assert.equal(definitionSource({ name: "deploy", plugin: "kit" }, plugins), "kit");
  assert.equal(
    definitionSource({ name: "check", path: "/data/plugins/kit-extra/skills/check/SKILL.md", pluginManaged: true }, plugins),
    "kit-extra",
  );
  // The person's own definitions say their scope instead; others are not guessed.
  assert.equal(definitionSource({ name: "mine", scope: "global" }, plugins), undefined);
  assert.equal(definitionSource({ name: "base-agent", scope: "base" }, plugins), undefined);
  assert.equal(definitionSource({ name: "other:agent" }, plugins), undefined);
  assert.equal(definitionSource({ name: "claude-agent" }, plugins), undefined);
});
