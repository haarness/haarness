import test from "node:test";
import assert from "node:assert/strict";
import {
  lastAlternative,
  matcherKind,
  matcherMatches,
  neverRuns,
  permissionNames,
  valueLabel,
} from "../src/renderer/hook-matcher";

test("a matcher matches as the runtime does", () => {
  assert.equal(matcherMatches("bash", "bash"), true);
  assert.equal(matcherMatches("Bash|Edit", "edit"), true);
  assert.equal(matcherMatches("MultiEdit", "edit"), true);
  assert.equal(matcherMatches("TodoWrite", "todo"), true);
  assert.equal(matcherMatches("todo", "todowrite"), true);
  assert.equal(matcherMatches("Bash", "write"), false);
  assert.equal(matcherMatches("^(manual|auto)", "automatic"), true);
  assert.equal(matcherMatches("auto", "automatic"), false);
  assert.equal(matcherMatches("[", "bash"), false);
  assert.equal(matcherMatches("AskUserQuestion", "question"), true);
});

test("an older server's catalog without matchers gets the generic field", () => {
  assert.equal(matcherKind(undefined), "unknown");
  assert.equal(matcherKind({ compares: "tool" }), "tool");
  assert.equal(neverRuns(undefined, "anything"), false);
});

test("only a matcher that can match nothing its event sends never runs", () => {
  const compaction = { compares: "value" as const, values: ["manual", "automatic"] };
  assert.equal(neverRuns(compaction, "manual"), false);
  assert.equal(neverRuns(compaction, "*"), false);
  assert.equal(neverRuns(compaction, " "), false);
  assert.equal(neverRuns(compaction, "manual|automatic"), false);
  assert.equal(neverRuns(compaction, "auto"), true);
  assert.equal(neverRuns(compaction, "Bash"), true);
  // Tools and permissions are open-ended, an event with nothing to match
  // ignores its matcher, and an older server says nothing.
  assert.equal(neverRuns({ compares: "tool" }, "mcp__github__create_issue"), false);
  assert.equal(neverRuns({ compares: "nothing" }, "bash"), false);
  assert.equal(neverRuns(undefined, "bash"), false);
  // Names a server accepts without suggesting them are matches too.
  const failure = { compares: "value" as const, values: ["rate_limit", "provider"], aliases: ["billing_error", "unknown"] };
  assert.equal(neverRuns(failure, "billing_error"), false);
  assert.equal(neverRuns(failure, "unknown"), false);
  assert.equal(neverRuns(failure, "network_error"), true);
});

test("suggestions complete the alternative being typed", () => {
  assert.deepEqual(lastAlternative("ed").query, "ed");
  assert.equal(lastAlternative("ed").replace("edit"), "edit");
  assert.equal(lastAlternative("bash|ed").query, "ed");
  assert.equal(lastAlternative("bash|ed").replace("edit"), "bash|edit");
  assert.equal(lastAlternative("bash|").replace("read"), "bash|read");
  assert.equal(lastAlternative("bash|ed").replace("*"), "*");
});

test("permission suggestions lead with the common ones and follow the tools", () => {
  const names = permissionNames(["bash", "write", "browser_click", "image_generate", "lsp"]);
  assert.deepEqual(names.slice(0, 4), ["bash", "read", "edit", "external_directory"]);
  assert.ok(names.includes("browser") && names.includes("media_generate") && names.includes("lsp"));
  assert.ok(!names.includes("write"));
  assert.equal(new Set(names).size, names.length);
});

test("every value an event sends has words for it", () => {
  for (const value of [
    "manual", "automatic", "authentication_failed", "rate_limit", "overloaded", "server_error",
    "invalid_request", "provider", "incomplete", "content_filter", "tool", "other", "startup",
    "archived", "permission_prompt", "turn_completed",
  ])
    assert.ok(valueLabel(value), value);
  assert.equal(valueLabel("write"), undefined);
});
