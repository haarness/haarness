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
test("Bedrock Claude ids read by the model they name", () => {
  for (const [id, name] of [
    ["apac.anthropic.claude-sonnet-5-5-v1:0", "Claude Sonnet 5.5"],
    ["anthropic.claude-opus-4-6-v1", "Claude Opus 4.6"],
    [
      "us-gov.anthropic.claude-haiku-4-5-20251001-v1:0",
      "Claude Haiku 4.5 (2025-10-01)",
    ],
    [
      "amazon-bedrock/au.anthropic.claude-sonnet-4-5-20250929-v1:0",
      "Claude Sonnet 4.5 (2025-09-29)",
    ],
  ])
    assert.equal(modelDisplayName(id, id), name, id);
  for (const id of [
    "apac.anthropic.claude-sonnet-9-9-v1:0",
    "mycorp.claude-sonnet-5-5",
    "meta.llama3-70b-instruct-v1:0",
  ])
    assert.equal(modelDisplayName(id), id);
});
