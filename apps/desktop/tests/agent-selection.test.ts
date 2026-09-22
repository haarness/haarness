import test from "node:test";
import assert from "node:assert/strict";
import type { AgentProfile } from "@haarness/sdk";
import {
  conversationAgents,
  selectConversationAgent,
} from "../src/renderer/agent-selection";

const agent = (
  id: string,
  extras: Partial<AgentProfile> = {},
): AgentProfile => ({
  id,
  name: id,
  description: "",
  hidden: false,
  maxSubagents: 0,
  ...extras,
});

test("conversation choices follow metadata rather than a list of internal names", () => {
  const catalog = [
    agent("build"),
    agent("title", { hidden: true }),
    agent("compaction", { hidden: true }),
    agent("private", { hidden: true }),
    agent("delegate", { mode: "subagent" }),
    agent("custom", { mode: "all" }),
    agent("planner", { mode: "primary" }),
  ];
  assert.deepEqual(
    conversationAgents(catalog).map((value) => value.id),
    ["build", "custom", "planner"],
  );
  assert.equal(catalog.length, 7);
  assert.equal(selectConversationAgent(catalog, "custom"), "custom");
  assert.equal(selectConversationAgent(catalog, "compaction"), "build");
  assert.equal(selectConversationAgent(catalog, "removed"), "build");
  assert.equal(selectConversationAgent([agent("planner")]), "planner");
  assert.equal(
    selectConversationAgent([agent("private", { hidden: true })]),
    undefined,
  );
});
