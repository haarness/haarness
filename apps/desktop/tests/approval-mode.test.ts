import { test } from "node:test";
import assert from "node:assert/strict";
import type { PendingPermissionInfo, Thread } from "@haarness/sdk";
import {
  autoApproval,
  approveAndEnable,
  canAutoApprove,
  createAutoApprover,
  readApprovalModes,
} from "../src/renderer/approval-mode";
const request = (id: string, threadId = "root"): PendingPermissionInfo => ({
  permissionId: id,
  permission: "bash",
  operation: "bash:printf test",
  patterns: ["printf test"],
  alwaysPatterns: ["bash:printf *"],
  detail: {},
  threadId,
  rootThreadId: "root",
  createdAt: new Date().toISOString(),
});
test("Auto scopes by checkout and conversation; child agents inherit but forks do not", () => {
  const modes = {
    a: { "*": false, root: true },
    b: { "*": true, manual: false },
  };
  const threads = [
    { id: "child", parentId: "root", isSubagent: true },
    { id: "fork", parentId: "root", isSubagent: false },
  ] as Thread[];
  assert.equal(autoApproval(modes, "a", "child", threads), true);
  assert.equal(autoApproval(modes, "a", "fork", threads), false);
  assert.equal(autoApproval(modes, "a", "unknown", [], "root"), true);
  assert.equal(autoApproval(modes, "b", "manual"), false);
  assert.equal(autoApproval(modes, "b", "new"), true);
  assert.equal(autoApproval(modes, "other", "root"), false);
  assert.equal(
    autoApproval(
      { a: { root: true, child: false } },
      "a",
      "child",
      threads,
      "root",
    ),
    false,
  );
  assert.deepEqual(
    readApprovalModes({ a: { "*": true, bad: "true" }, bad: [], null: null }),
    { a: { "*": true } },
  );
});
test("Auto leaves restricted requests manual", () => {
  for (const permission of ["instruction_import"])
    assert.equal(canAutoApprove({ ...request("1"), permission }), false);
  assert.equal(canAutoApprove({ ...request("1"), allowOnce: false }), false);
  assert.equal(canAutoApprove({ ...request("1"), allowAlways: false }), false);
  assert.equal(canAutoApprove(request("1")), true);
});
test("Auto attempts each permission once across replay, keeps failures manual, and stops in Review", async () => {
  let enabled = true;
  const replies: string[] = [],
    failed: string[] = [];
  const consume = createAutoApprover({
    enabled: () => enabled,
    reply: async (connection, id) => {
      replies.push(connection + ":" + id);
      if (id === "fail") throw Error("offline");
    },
    failed: (_, r) => failed.push(r.permissionId),
  });
  consume("a", [request("ok"), request("fail")], []);
  consume("a", [request("ok"), request("fail")], []);
  await new Promise((resolve) => setImmediate(resolve));
  assert.deepEqual(replies, ["a:ok", "a:fail"]);
  assert.deepEqual(failed, ["fail"]);
  enabled = false;
  consume("a", [request("new")], []);
  assert.equal(replies.length, 2);
  enabled = true;
  consume("b", [request("ok")], []);
  assert.equal(replies.at(-1), "b:ok");
});

test("Allow once + Auto acknowledges the current prompt before enabling the consumer", () => {
  const replies: string[] = [];
  const consume = createAutoApprover({
    enabled: () => true,
    reply: async (_, id) => {
      replies.push(id);
    },
    failed: () => assert.fail("unexpected retry"),
  });
  consume.acknowledge("a", "current");
  consume("a", [request("current"), request("next")], []);
  assert.deepEqual(replies, ["next"]);
});

test("Allow once + Auto never enables after a failed reply or after navigation/review changes", async () => {
  let enabled = 0,
    acknowledged = 0,
    current = true;
  const callbacks = {
    acknowledge: () => {
      acknowledged++;
    },
    current: () => current,
    enable: async () => {
      enabled++;
    },
  };
  await assert.rejects(
    approveAndEnable({
      ...callbacks,
      reply: async () => {
        throw Error("offline");
      },
    }),
  );
  assert.equal(enabled, 0);
  assert.equal(acknowledged, 0);
  let resolve!: () => void;
  const reply = new Promise<void>((done) => {
    resolve = done;
  });
  const pending = approveAndEnable({ ...callbacks, reply: () => reply });
  current = false;
  resolve();
  await pending;
  assert.equal(enabled, 0);
  assert.equal(acknowledged, 1);
  current = true;
  await approveAndEnable({ ...callbacks, reply: async () => {} });
  assert.equal(enabled, 1);
});
