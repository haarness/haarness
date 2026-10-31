import { test } from "node:test";
import assert from "node:assert/strict";
import { modelDisplayName, tidyName } from "../src/model-names";
import names from "../../../crates/haarness-protocol/src/model-names.json";
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
  // A Claude id the table does not know reads by its words; lookalikes stay
  // as written.
  assert.equal(
    modelDisplayName("apac.anthropic.claude-sonnet-9-9-v1:0"),
    "Claude Sonnet 9.9",
  );
  for (const id of ["mycorp.claude-sonnet-5-5", "meta.llama3-70b-instruct-v1:0"])
    assert.equal(modelDisplayName(id), id);
});
test("a model new to the table reads by its brand, as the server names it", () => {
  for (const [id, name] of [
    ["gpt-7-mini", "GPT-7 Mini"],
    ["gpt-6.2-sol", "GPT-6.2 Sol"],
    ["openai/gpt-7", "GPT-7"],
    ["o7-mini", "o7 Mini"],
    ["claude-opus-6", "Claude Opus 6"],
    ["claude-opus-6-1", "Claude Opus 6.1"],
    ["claude-sonnet-6-20270101", "Claude Sonnet 6 (2027-01-01)"],
    ["claude-sonnet-6@20270101", "Claude Sonnet 6 (2027-01-01)"],
    ["deepseek-v5", "DeepSeek V5"],
    ["gemini-5-pro-preview", "Gemini 5 Pro Preview"],
    ["grok-5-fast", "Grok 5 Fast"],
    ["glm-6", "GLM-6"],
    ["gpt-oss-240b", "GPT-OSS 240B"],
    ["kimi-k4", "Kimi K4"],
    ["qwen-4-coder", "Qwen 4 Coder"],
  ]) {
    assert.equal(modelDisplayName(id), name, id);
    assert.equal(modelDisplayName(id, id), name, id);
  }
  for (const id of ["future-7", "my-company/future-7", "GPT-7", "gpt-7 mini", "gpt-", "o-mini"])
    assert.equal(modelDisplayName(id), id, id);
});
test("the brand reading agrees with the table", () => {
  const ownWay = new Set([
    "gpt-5.6",
    "grok-4.20-0309-reasoning",
    "grok-4.20-0309-non-reasoning",
    "grok-4-1-fast-non-reasoning",
    "mistral-large-4-0",
  ]);
  for (const [id, name] of Object.entries(names as Record<string, string>)) {
    if (ownWay.has(id)) continue;
    const tidy = tidyName(id);
    if (tidy !== undefined) assert.equal(tidy, name, id);
  }
});
