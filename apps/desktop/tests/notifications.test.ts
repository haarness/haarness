import { test, beforeEach, afterEach, mock } from "node:test";
import assert from "node:assert/strict";
import { EventEmitter } from "node:events";
import {
  COMPLETION_GRACE_MS,
  createNotifications,
} from "../src/main/notifications";
import type { EventEnvelope, Thread } from "@haarness/sdk";

// Completion banners wait out a grace on a real timer; every test drives it.
beforeEach(() => mock.timers.enable({ apis: ["setTimeout"] }));
afterEach(() => mock.timers.reset());
/** Advance the fake clock, then let the woken receive() calls run on. */
async function tick(ms = COMPLETION_GRACE_MS) {
  mock.timers.tick(ms);
  await new Promise((resolve) => setImmediate(resolve));
}

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
  /** Deliver one event without waiting out a completion's grace. */
  const deliver = (value = event(), connection = "project-a") =>
    center.receive(
      connection,
      "My project",
      7,
      value,
      async () => ({ title: "Fix 日本", id: "conversation" }) as Thread,
    );
  /** Deliver one event and let any completion's grace run out. */
  const receive = async (value = event(), connection = "project-a") => {
    const pending = deliver(value, connection);
    await tick();
    await pending;
  };
  return {
    center,
    shown,
    opened,
    deliver,
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
  await Promise.all([f.deliver(), f.deliver(), tick()]);
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
  await f.receive(event({ eventId: 2, threadId: "child" }));
  await f.receive(event({ eventId: 3, payload: { status: "failed" } }));
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
  await tick();
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
  await tick();
  f.center.forget("a");
  resolve({ title: "late" } as Thread);
  await pending;
  assert.equal(f.shown.length, 0);
});
test("a failed title lookup still offers a scoped notification", async () => {
  const f = fixture();
  const pending = f.center.receive("a", "a", 1, event(), async () => {
    throw new Error("offline");
  });
  await tick();
  await pending;
  assert.equal(f.shown.length, 1);
  assert.match(f.shown[0].body, /Chat conversa/);
});
test("a chat without a generated title is named untitled, not by the server's placeholder", async () => {
  const f = fixture();
  const pending = f.center.receive("a", "My project", 1, event(), async () =>
    ({ title: "New thread", id: "conversation" }) as Thread,
  );
  await tick();
  await pending;
  assert.equal(f.shown[0].body, "Untitled chat · My project");
  const fork = f.center.receive("a", "My project", 1, event({ eventId: 2 }), async () =>
    ({ title: "Fork of New thread", id: "conversation" }) as Thread,
  );
  await tick();
  await fork;
  assert.equal(f.shown[1].body, "Fork of untitled chat · My project");
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
test("a request announced again asks for attention only when it newly needs the person", async () => {
  const f = fixture();
  const request = (eventId: number, review?: Record<string, unknown>, permissionId = "p1") =>
    f.receive(event({ eventId, type: "permission.requested", payload: { permissionId, review } }));
  // Shown in Review: one banner.
  await request(1);
  assert.equal(f.shown.length, 1);
  // Auto turns on elsewhere with the card shown: the reviewer is deciding it.
  await request(2, { status: "reviewing" });
  assert.equal(f.shown.length, 1);
  // The reviewer leaves it to the person: it needs them again.
  await request(3, { status: "asked", rationale: "Test verdict: ask." });
  assert.equal(f.shown.length, 2);
  // The same need announced again (a reconnect, a replay) stays quiet.
  await request(4, { status: "asked", rationale: "Test verdict: ask." });
  assert.equal(f.shown.length, 2);
  // A request first seen under review never asks; its ask does, once.
  await request(5, { status: "reviewing" }, "p2");
  assert.equal(f.shown.length, 2);
  await request(6, { status: "failed", reason: { code: "timeout", seconds: 60 } }, "p2");
  assert.equal(f.shown.length, 3);
  // Permission IDs are scoped by project.
  await f.receive(event({ eventId: 7, type: "permission.requested", payload: { permissionId: "p1" } }), "project-b");
  assert.equal(f.shown.length, 4);
  assert.ok(f.shown.every((item) => /Approval needed/.test(item.title)));
});
test("changing notification policy while resolving a conversation suppresses the stale alert", async () => {
  const policy = { completionNotifications: "always" as "always" | "never", permissionNotifications: true, questionNotifications: true };
  const f = fixture(policy);
  let resolve!: (thread: Thread) => void;
  const pending = f.center.receive("a", "a", 1, event(), () => new Promise(r => resolve = r));
  await tick();
  policy.completionNotifications = "never";
  resolve({title:"Late result"} as Thread);
  await pending;
  assert.equal(f.shown.length,0);
});

test("a completion waits out its grace before the banner goes out", async () => {
  const f = fixture();
  const pending = f.deliver();
  await tick(COMPLETION_GRACE_MS - 1);
  assert.equal(f.shown.length, 0, "nothing goes out inside the grace");
  await tick(1);
  await pending;
  assert.equal(f.shown.length, 1);
  assert.match(f.shown[0].title, /Task finished/);
});
test("a queue of prompts announces finished once, after its last turn", async () => {
  const f = fixture();
  const started = (eventId: number) =>
    event({ eventId, type: "turn.started", payload: { status: "running" } });
  // Each queued prompt starts as a new root turn moments after the last one.
  const pending = [f.deliver(event({ eventId: 1 }))];
  await tick(100);
  pending.push(f.deliver(started(2)));
  await tick(COMPLETION_GRACE_MS);
  assert.equal(f.shown.length, 0, "the next prompt started: not finished");
  pending.push(f.deliver(event({ eventId: 3 })));
  await tick(30);
  pending.push(f.deliver(started(4)));
  pending.push(f.deliver(event({ eventId: 5 })));
  await tick(COMPLETION_GRACE_MS - 1);
  assert.equal(f.shown.length, 0);
  await tick(1);
  await Promise.all(pending);
  assert.equal(f.shown.length, 1, "the final turn of the queue gets one banner");
  assert.match(f.shown[0].title, /Task finished/);
  // Two completions inside one grace (a turn too short to see) send one banner.
  pending.push(f.deliver(event({ eventId: 6 })), f.deliver(event({ eventId: 7 })));
  await tick();
  await Promise.all(pending);
  assert.equal(f.shown.length, 2);
});
test("a stop or failure inside the grace drops the held completion; failure shows once, stop nothing", async () => {
  const f = fixture();
  const held = f.deliver(event({ eventId: 1 }));
  await f.receive(event({ eventId: 2, payload: { status: "failed" } }));
  await held;
  assert.deepEqual(
    f.shown.map((notification) => notification.title),
    ["Haarness — Task failed"],
  );
  const stopped = f.deliver(event({ eventId: 3 }));
  await f.receive(event({ eventId: 4, payload: { status: "interrupted" } }));
  await stopped;
  await tick();
  assert.equal(f.shown.length, 1, "a stop is never announced as finished");
});
test("a turn that background work outlives is neither finished nor failed; the turn its last result starts is", async () => {
  const f = fixture();
  // A completion still in its grace is not true once a later turn says
  // background work goes on.
  const held = f.deliver(event({ eventId: 1 }));
  await f.receive(event({ eventId: 2, payload: { status: "completed", background: 1 } }));
  await f.receive(event({ eventId: 3, payload: { status: "failed", background: 2 } }));
  await held;
  await tick();
  assert.equal(f.shown.length, 0, "the conversation is still at work");
  // The child's result starts a turn; when it ends, the conversation is done.
  await f.deliver(event({ eventId: 4, type: "turn.started", payload: { status: "running" } }));
  await f.receive(event({ eventId: 5, payload: { status: "completed" } }));
  assert.deepEqual(
    f.shown.map((notification) => notification.title),
    ["Haarness — Task finished"],
  );
  // A count of zero, as an older server never sends, is no different.
  await f.receive(event({ eventId: 6, payload: { status: "failed", background: 0 } }));
  assert.equal(f.shown.length, 2);
  assert.match(f.shown[1].title, /Task failed/);
});
test("other conversations and delegated agents starting do not drop a held completion", async () => {
  const f = fixture();
  const pending = f.deliver();
  for (const [eventId, threadId, rootThreadId] of [
    [2, "other", "other"],
    [3, "child", "conversation"],
  ] as const)
    await f.deliver(
      event({ eventId, type: "turn.started", threadId, rootThreadId }),
    );
  // The same conversation in another project is a different conversation.
  await f.deliver(event({ eventId: 4, type: "turn.started" }), "project-b");
  await tick();
  await pending;
  assert.equal(f.shown.length, 1);
});
test("forgetting a project or disposing drops completions still in their grace", async () => {
  const f = fixture();
  const forgotten = f.deliver();
  f.center.forget("project-a");
  await forgotten;
  const disposed = f.deliver(event({ eventId: 2 }), "project-b");
  f.center.dispose();
  await disposed;
  await tick();
  assert.equal(f.shown.length, 0);
});

test("titles and stand-in chat names speak the interface language", async () => {
  const shown: { title: string; body: string }[] = [];
  const center = createNotifications({
    enabled: () => true,
    focused: () => false,
    translate: (key) => `«${key}»`,
    create(options) {
      return Object.assign(new EventEmitter(), options, {
        show() {
          shown.push(options);
        },
        close() {},
      });
    },
    open: () => {},
  });
  const untitled = center.receive("a", "Renamed project", 1, event(), async () =>
    ({ title: "New thread", id: "conversation" }) as Thread,
  );
  await tick();
  await untitled;
  assert.deepEqual(shown[0], {
    title: "Haarness — «Task finished»",
    body: "«Untitled chat» · Renamed project",
  });
  const missing = center.receive("a", "Renamed project", 1, event({ eventId: 2 }), async () => {
    throw new Error("offline");
  });
  await tick();
  await missing;
  assert.equal(shown[1].body, "«Chat conversa» · Renamed project");
});
