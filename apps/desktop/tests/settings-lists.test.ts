import { test } from "node:test";
import assert from "node:assert/strict";
import { definitionSource, homePath, splitPath } from "../src/renderer/settings-lists";

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
