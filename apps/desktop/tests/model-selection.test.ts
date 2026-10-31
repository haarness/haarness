import { test } from "node:test";
import assert from "node:assert/strict";
import type { ProviderInfo, Thread } from "@haarness/sdk";
import {
  parseModel,
  readModelRecents,
  selectModel,
  unlistedModel,
} from "../src/renderer/model-selection";

const provider = (
  id: string,
  authenticated = true,
  tools = true,
): ProviderInfo => ({
  id,
  name: id,
  authenticated,
  models: ["preferred", "other"].map((id) => ({
    id,
    displayName: id,
    tools,
    reasoning: true,
    images: true,
    reasoningEfforts: ["low", "high"],
  })),
});
const base = {
  providers: [provider("openai"), provider("custom")],
  recent: [],
  threads: [],
};
test("model selection respects explicit, agent and configured choices before recents", () => {
  const input = {
    ...base,
    recent: [{ provider: "openai", model: "other" }],
    defaultProvider: "custom",
    defaultModel: "preferred",
    agentModel: "custom/other",
  };
  assert.deepEqual(selectModel(input), { provider: "custom", model: "other" });
  assert.deepEqual(selectModel({ ...input, agentModel: undefined }), {
    provider: "custom",
    model: "preferred",
  });
  assert.deepEqual(
    selectModel({
      ...input,
      explicit: { provider: "openai", model: "preferred" },
    }),
    { provider: "openai", model: "preferred" },
  );
});
test("automatic selection skips disconnected and stale candidates and prefers tool support", () => {
  const input = {
    ...base,
    providers: [
      provider("offline", false),
      provider("openai", true, false),
      provider("custom"),
    ],
    defaultProvider: "offline",
    defaultModel: "preferred",
    agentModel: "custom/deleted",
    recent: [
      { provider: "offline", model: "preferred" },
      { provider: "custom", model: "deleted" },
    ],
  };
  assert.deepEqual(selectModel(input), {
    provider: "custom",
    model: "preferred",
  });
  assert.equal(
    selectModel({ ...base, providers: [provider("offline", false)] }),
    undefined,
  );
});
test("valid recent choices and conversation history are reused without child-agent pollution", () => {
  const threads = [
    {
      provider: "custom",
      model: "other",
      updatedAt: "2026-09-08",
      isSubagent: true,
    },
    {
      provider: "openai",
      model: "other",
      updatedAt: "2026-09-07",
      title: "Old title",
    },
  ] as Thread[];
  assert.deepEqual(selectModel({ ...base, threads }), {
    provider: "openai",
    model: "other",
  });
  assert.deepEqual(
    selectModel({ ...base, threads, defaultProvider: "openai" }),
    { provider: "openai", model: "other" },
  );
  assert.deepEqual(
    selectModel({
      ...base,
      threads,
      recent: [{ provider: "custom", model: "preferred" }],
    }),
    { provider: "custom", model: "preferred" },
  );
});
test("provider-only defaults use that provider's preferred model and partial config resolves safely", () => {
  assert.deepEqual(selectModel({ ...base, defaultProvider: "custom" }), {
    provider: "custom",
    model: "preferred",
  });
  assert.deepEqual(selectModel({ ...base, defaultModel: "custom/other" }), {
    provider: "custom",
    model: "other",
  });
  assert.deepEqual(
    selectModel({
      ...base,
      defaultProvider: "custom",
      defaultModel: "removed",
      recent: [{ provider: "openai", model: "other" }],
    }),
    { provider: "openai", model: "other" },
  );
});
test("remembered selections are validated and model IDs can contain slashes", () => {
  const custom = provider("custom");
  custom.models[0].id = "vendor/model";
  assert.deepEqual(
    selectModel({ ...base, providers: [custom], defaultModel: "vendor/model" }),
    { provider: "custom", model: "vendor/model" },
  );
  assert.deepEqual(parseModel("custom/vendor/model"), {
    provider: "custom",
    model: "vendor/model",
  });
  assert.equal(parseModel("/model"), undefined);
  assert.equal(parseModel("provider/"), undefined);
  assert.deepEqual(
    readModelRecents([
      null,
      {},
      { provider: "", model: "x" },
      { provider: "custom", model: "preferred", title: "never send this" },
    ]),
    [{ provider: "custom", model: "preferred" }],
  );
});

test("disconnected, deleted and stale explicit choices resolve to available models", () => {
  for (const explicit of [
    { provider: "offline", model: "preferred" },
    { provider: "openai", model: "deleted" },
  ]) {
    assert.deepEqual(selectModel({ ...base, explicit }), {
      provider: "openai",
      model: "preferred",
    });
    assert.equal(selectModel({ ...base, providers: [], explicit }), undefined);
  }
  assert.equal(
    selectModel({
      ...base,
      providers: [provider("offline", false)],
      explicit: { provider: "offline", model: "preferred" },
    }),
    undefined,
  );
});

test("a chat's model its provider no longer lists is kept and told apart", () => {
  const providers = [provider("openai"), provider("custom", false)];
  assert.deepEqual(unlistedModel(providers, "openai/retired"), {
    provider: "openai",
    model: "retired",
  });
  assert.equal(unlistedModel(providers, "openai/preferred"), undefined);
  // A list not in yet, or a provider not signed in, decides nothing.
  assert.equal(unlistedModel([{ ...provider("openai"), models: [] }], "openai/retired"), undefined);
  assert.equal(unlistedModel(providers, "custom/retired"), undefined);
  assert.equal(unlistedModel(providers, ""), undefined);
});

test("a configured default the provider retired falls back to a recent, then the first connected", () => {
  const providers = [provider("openai"), provider("custom")];
  assert.deepEqual(
    selectModel({
      ...base,
      providers,
      defaultProvider: "openai",
      defaultModel: "retired",
      recent: [{ provider: "custom", model: "other" }],
    }),
    { provider: "custom", model: "other" },
  );
  assert.deepEqual(
    selectModel({ ...base, providers, defaultProvider: "openai", defaultModel: "retired" }),
    { provider: "openai", model: "preferred" },
  );
});
