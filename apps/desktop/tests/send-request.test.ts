import test from "node:test";
import assert from "node:assert/strict";
import { commandBody, messageBody } from "../src/renderer/send-request";

test("a message says a person can stop the turn it starts, with what it always sent", () => {
  assert.deepEqual(
    messageBody({
      delivery: "steer",
      text: "Run the tests",
      attachments: ["/tmp/shot.png"],
      mentioned: { resources: [{ server: "docs", uri: "docs://setup" }] },
      requestId: "0190-request",
      effort: "high",
    }),
    {
      delivery: "steer",
      preview: true,
      tools: { preview: true },
      text: "Run the tests",
      attachments: ["/tmp/shot.png"],
      resources: [{ server: "docs", uri: "docs://setup" }],
      requestId: "0190-request",
      reasoningEffort: "high",
      attended: true,
    },
  );
  const queued = messageBody({
    delivery: "queue",
    text: "Then deploy",
    attachments: [],
    mentioned: {},
    requestId: "0190-queued",
  });
  assert.equal(queued.attended, true);
  assert.equal("reasoningEffort" in queued, false);
  assert.equal("resources" in queued, false);
});

test("a slash command says a person can stop the turn it starts", () => {
  assert.deepEqual(
    commandBody({
      name: "test",
      arguments: "--watch",
      mentioned: {},
      requestId: "0190-command",
    }),
    {
      name: "test",
      tools: { preview: true },
      arguments: "--watch",
      requestId: "0190-command",
      attended: true,
    },
  );
});
