import { test } from "node:test";
import assert from "node:assert/strict";
import { EventEmitter } from "node:events";
import { createNotifications } from "../src/main/notifications";
import type { EventEnvelope, Thread } from "@haarness/sdk";

const event = (extra: Partial<EventEnvelope> = {}): EventEnvelope => ({
  type: "turn.finished",
  threadId: "conversation",
  rootThreadId: "conversation",
  payload: { status: "completed" },
  eventId: 1,
  serverInstanceId: "server",
  schemaVersion: "1",
  createdAt: new Date().toISOString(),
  ...extra,
});
function fixture(policy?: { completionNotifications: "background" | "always" | "never"; permissionNotifications: boolean; questionNotifications: boolean }) {
  let enabled = true,
    focused = false;
  const shown: (EventEmitter & {
    title: string;
    body: string;
    close(): void;
  })[] = [];
  const opened: unknown[] = [];
  const center = createNotifications({
    enabled: () => enabled,
    policy: policy ? () => policy : undefined,
    focused: () => focused,
    create(options) {
      const notification = Object.assign(new EventEmitter(), options, {
        show() {
          shown.push(notification);
        },
        close() {
          notification.emit("close");
        },
      });
      return notification;
    },
    open: (target, owner) => opened.push({ target, owner }),
  });
  const receive = (value = event(), connection = "project-a") =>
    center.receive(
      connection,
      "My project",
      7,
      value,
      async () => ({ title: "Fix 日本", id: "conversation" }) as Thread,
    );
  return {
    center,
    shown,
    opened,
    receive,
    enable: (value: boolean) => (enabled = value),
    focus: (value: boolean) => (focused = value),
  };
}
test("click retains originating project and conversation and is one shot", async () => {
  const f = fixture();
  await f.receive();
  assert.equal(f.shown[0].body, "Fix 日本 · My project");
  f.shown[0].emit("click");
  f.shown[0].emit("click");
  assert.deepEqual(f.opened, [
    { target: { connection: "project-a", thread: "conversation" }, owner: 7 },
  ]);
});
test("deduplicates replay and overlapping subscriptions but scopes event IDs by project and instance", async () => {
  const f = fixture();
  await Promise.all([f.receive(), f.receive()]);
  await f.receive(event(), "project-b");
  await f.receive(event({ serverInstanceId: "restarted" }));
  assert.equal(f.shown.length, 3);
});
test("foreground and muted events stay suppressed after replay", async () => {
  const f = fixture();
  f.focus(true);
  await f.receive();
  f.focus(false);
  await f.receive();
  f.enable(false);
  await f.receive(event({ eventId: 2 }));
  f.enable(true);
  await f.receive(event({ eventId: 2 }));
  assert.equal(f.shown.length, 0);
});
test("failure is distinct; cancellation and delegated completion stay quiet", async () => {
  const f = fixture();
  await f.receive(event({ payload: { status: "interrupted" } }));
  await f.receive(event({ threadId: "child" }));
  await f.receive(event({ payload: { status: "failed" } }));
  assert.equal(f.shown.length, 1);
  assert.match(f.shown[0].title, /Task failed/);
});
test("delegated approval and question route to the root conversation", async () => {
  const f = fixture();
  for (const [index, type] of [
    "permission.requested",
    "question.requested",
  ].entries()) {
    await f.receive(event({ eventId: index + 1, type, threadId: "child" }));
    f.shown[index].emit("click");
  }
  assert.match(f.shown[0].title, /Approval needed/);
  assert.match(f.shown[1].title, /Answer needed/);
  assert.deepEqual(
    f.opened,
    Array(2).fill({
      target: { connection: "project-a", thread: "conversation" },
      owner: 7,
    }),
  );
});
test("rechecks foreground and preference after asynchronous title lookup", async () => {
  const f = fixture();
  let resolve!: (value: Thread) => void;
  const pending = f.center.receive(
    "a",
    "a",
    1,
    event(),
    () => new Promise((r) => (resolve = r)),
  );
  f.focus(true);
  resolve({ title: "late" } as Thread);
  await pending;
  assert.equal(f.shown.length, 0);
});
test("forgotten, closed and failed notifications cannot navigate; retention is bounded", async () => {
  const f = fixture();
  await f.receive();
  f.center.forget("project-a");
  f.shown[0].emit("click");
  for (let i = 2; i <= 103; i++) await f.receive(event({ eventId: i }));
  f.shown[1].emit("click");
  assert.equal(f.opened.length, 0);
  f.shown.at(-1)!.emit("failed");
  f.shown.at(-1)!.emit("click");
  assert.equal(f.opened.length, 0);
  f.center.dispose();
  f.shown.at(-2)!.emit("click");
  assert.equal(f.opened.length, 0);
});

test("forget invalidates an in-flight title lookup", async () => {
  const f = fixture();
  let resolve!: (value: Thread) => void;
  const pending = f.center.receive(
    "a",
    "a",
    1,
    event(),
    () => new Promise((r) => (resolve = r)),
  );
  f.center.forget("a");
  resolve({ title: "late" } as Thread);
  await pending;
  assert.equal(f.shown.length, 0);
});
test("a failed title lookup still offers a scoped notification", async () => {
  const f = fixture();
  await f.center.receive("a", "a", 1, event(), async () => {
    throw new Error("offline");
  });
  assert.equal(f.shown.length, 1);
  assert.match(f.shown[0].body, /Conversation conversa/);
});

test("notification controls independently govern completion, approvals and questions", async () => {
  const policy = { completionNotifications: "always" as "always" | "background" | "never", permissionNotifications: false, questionNotifications: true };
  const f = fixture(policy);
  f.focus(true);
  await f.receive();
  assert.equal(f.shown.length, 1);
  policy.completionNotifications = "never";
  f.focus(false);
  await f.receive(event({eventId:2}));
  await f.receive(event({eventId:3,type:"permission.requested"}));
  assert.equal(f.shown.length, 1);
  await f.receive(event({eventId:4,type:"question.requested"}));
  assert.equal(f.shown.length, 2);
  policy.questionNotifications = false;
  policy.permissionNotifications = true;
  await f.receive(event({eventId:5,type:"question.requested"}));
  await f.receive(event({eventId:6,type:"permission.requested"}));
  assert.equal(f.shown.length, 3);
  f.enable(false);
  await f.receive(event({eventId:7,type:"permission.requested"}));
  assert.equal(f.shown.length, 3);
});
test("changing notification policy while resolving a conversation suppresses the stale alert", async () => {
  const policy = { completionNotifications: "always" as "always" | "never", permissionNotifications: true, questionNotifications: true };
  const f = fixture(policy);
  let resolve!: (thread: Thread) => void;
  const pending = f.center.receive("a", "a", 1, event(), () => new Promise(r => resolve = r));
  policy.completionNotifications = "never";
  resolve({title:"Late result"} as Thread);
  await pending;
  assert.equal(f.shown.length,0);
});
