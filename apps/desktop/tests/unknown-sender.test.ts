import { test } from "node:test";
import assert from "node:assert/strict";
import { ApiError } from "@haarness/sdk";
import { onceMoreForUnknownSender } from "../src/main/unknown-sender";

const unknown = () =>
  new ApiError(409, "unknown_sender", "Haarness could not tell which program on this computer sent this request, so nothing changed. Try again.", true, {});

test("a request whose sender the server could not find is sent once more", async () => {
  let calls = 0;
  const answer = await onceMoreForUnknownSender(async () => {
    calls += 1;
    if (calls === 1) throw unknown();
    return "answered";
  });
  assert.equal(answer, "answered");
  assert.equal(calls, 2);
});

test("a second refusal stands in the server's words, and other failures are not retried", async () => {
  let calls = 0;
  await assert.rejects(
    onceMoreForUnknownSender(async () => {
      calls += 1;
      throw unknown();
    }),
    /could not tell which program/,
  );
  assert.equal(calls, 2);
  calls = 0;
  await assert.rejects(
    onceMoreForUnknownSender(async () => {
      calls += 1;
      throw new ApiError(409, "conflict", "Approvals changed in another window. Refresh and try again.", true, {});
    }),
    /another window/,
  );
  assert.equal(calls, 1);
});
