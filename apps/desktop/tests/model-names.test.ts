import { test } from "node:test";
import assert from "node:assert/strict";
import { modelDisplayName } from "../src/model-names";
test("friendly names preserve live catalog names and unknown deployment IDs", () => {
  assert.equal(modelDisplayName("gpt-6-astra"), "GPT-6 Astra");
  assert.equal(
    modelDisplayName("claude-fable-5-1", "claude-fable-5-1"),
    "Claude Fable 5.1",
  );
  assert.equal(
    modelDisplayName("claude-fable-5-1", "Team reasoning model"),
    "Team reasoning model",
  );
  assert.equal(modelDisplayName("my-company/future-7"), "my-company/future-7");
  assert.equal(modelDisplayName("openai/o3-pro"), "o3 Pro");
  assert.equal(modelDisplayName("haarness/openai/gpt-6.1-sol"), "GPT-6.1 Sol");
  assert.equal(
    modelDisplayName("us.anthropic.claude-sonnet-4-5-20250929-v1:0"),
    "Claude Sonnet 4.5 (2025-09-29)",
  );
});
