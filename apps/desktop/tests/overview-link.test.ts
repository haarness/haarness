import { test } from "node:test";
import assert from "node:assert/strict";
import type { EventEnvelope } from "@haarness/sdk";
import type { Connection, DesktopEvent } from "../src/shared";
import { createOverviewLink } from "../src/renderer/overview-link";
import { createWakeSlots } from "../src/renderer/reconnect-wait";

const location = "remote:http://host";

/** One sidebar project against a fake main process, with time in hand. */
function harness(
  connect?: (calls: number) => Promise<Connection | null>,
) {
  let now = 0;
  let instance = "first";
  const timers: { at: number; run: () => void; cancelled: boolean }[] = [];
  const delays: number[] = [];
  const subscribed: { key: string; cursor: number }[] = [];
  const unsubscribed: string[] = [];
  const reads: string[] = [];
  const states: string[] = [];
  const applied: number[] = [];
  let snapshots = 0,
    connects = 0;
  const info = () => ({
    serverInstanceId: instance,
    eventCursor: 5,
    capabilities: [],
  });
  const link = createOverviewLink({
    id: location,
    bridge: {
      api: async <T,>(_id: string, method: string) => {
        reads.push(method);
        return info() as T;
      },
      subscribe: async (_id: string, cursor: number) => {
        const key = "s" + (subscribed.length + 1);
        subscribed.push({ key, cursor });
        return key;
      },
      unsubscribe: async (key: string) => {
        unsubscribed.push(key);
      },
    },
    connect: async () => {
      connects++;
      if (connect) return connect(connects);
      return { info: info() } as unknown as Connection;
    },
    snapshot: async () => {
      snapshots++;
      return () => {};
    },
    apply: (event: EventEnvelope) => applied.push(event.eventId),
    loading: () => states.push("loading"),
    loaded: () => states.push("loaded"),
    failed: () => states.push("failed"),
    now: () => now,
    schedule: (run, ms) => {
      delays.push(ms);
      const timer = { at: now + ms, run, cancelled: false };
      timers.push(timer);
      return () => {
        timer.cancelled = true;
      };
    },
    slots: createWakeSlots(),
  });
  const settle = async () => {
    for (let turn = 0; turn < 20; turn++)
      await new Promise((resolve) => setImmediate(resolve));
  };
  const emit = (event: DesktopEvent) => link.receive(event);
  return {
    link,
    delays,
    subscribed,
    unsubscribed,
    reads,
    states,
    applied,
    get snapshots() {
      return snapshots;
    },
    get connects() {
      return connects;
    },
    set instance(value: string) {
      instance = value;
    },
    settle,
    /** Lets `ms` pass, running the timers that come due. */
    async advance(ms: number) {
      const end = now + ms;
      for (;;) {
        const next = timers
          .filter((timer) => !timer.cancelled && timer.at <= end)
          .sort((a, b) => a.at - b.at)[0];
        if (!next) break;
        now = next.at;
        next.cancelled = true;
        next.run();
        await settle();
      }
      now = end;
      await settle();
    },
    /** Main says the stream's response arrived. */
    opened: (key: string) =>
      emit({ kind: "connection", connection: location, id: key, data: "opened" }),
    /** The stream ended. */
    drop: (key: string) =>
      emit({ kind: "connection", connection: location, id: key, data: "resync" }),
    event: (key: string, eventId: number, type = "thread.updated") =>
      emit({
        kind: "server",
        connection: location,
        id: key,
        data: { eventId, type, serverInstanceId: instance, payload: {} },
      }),
    wake: () => emit({ kind: "connection", data: "resume" }),
    last: () => subscribed.at(-1)!.key,
  };
}

test("a sidebar project whose stream ends as soon as it opens waits longer each time", async () => {
  const project = harness();
  project.link.start();
  await project.settle();
  for (let attempt = 0; attempt < 4; attempt++) {
    project.opened(project.last());
    project.drop(project.last());
    await project.advance(project.delays.at(-1)!);
  }
  assert.deepEqual(project.delays, [500, 1000, 2000, 4000]);
  assert.equal(project.snapshots, 5);
});

test("a stream that stayed open 10 s from its response starts the wait over", async () => {
  const project = harness();
  project.link.start();
  await project.settle();
  project.drop(project.last());
  await project.advance(500);
  project.opened(project.last());
  await project.advance(10_000);
  project.drop(project.last());
  assert.deepEqual(project.delays, [500, 500]);
});

test("a stream held without a response does not count as working, however long it was held", async () => {
  const project = harness();
  project.link.start();
  await project.settle();
  await project.advance(30_000);
  project.drop(project.last());
  await project.advance(500);
  await project.advance(30_000);
  project.drop(project.last());
  assert.deepEqual(project.delays, [500, 1000]);
});

test("the server's resync and the end of its stream ask for one retry", async () => {
  const project = harness();
  project.link.start();
  await project.settle();
  const key = project.last();
  project.link.receive({
    kind: "server",
    connection: location,
    id: key,
    data: { type: "resync_required" },
  });
  project.drop(key);
  assert.deepEqual(project.delays, [500]);
});

test("a wake brings a waiting project's retry forward once, to its slot, however many signals come with it", async () => {
  const project = harness(async () => {
    throw new Error("unreachable");
  });
  project.link.start();
  await project.settle();
  for (const ms of [500, 1000, 2000]) await project.advance(ms);
  assert.deepEqual(project.delays, [500, 1000, 2000, 4000]);
  const connects = project.connects;
  project.wake();
  project.link.hint(false);
  project.link.hint(false);
  assert.deepEqual(project.delays, [500, 1000, 2000, 4000, 750]);
  await project.advance(750);
  assert.equal(project.connects, connects + 1);
});

test("a signal never pushes a retry due sooner later", async () => {
  const project = harness(async () => {
    throw new Error("unreachable");
  });
  project.link.start();
  await project.settle();
  assert.deepEqual(project.delays, [500]);
  project.link.hint(false);
  assert.deepEqual(project.delays, [500]);
});

test("a signal supersedes an attempt still in flight, whose late answer is dropped", async () => {
  let release!: (value: Connection) => void;
  const project = harness((call) =>
    call === 1
      ? new Promise<Connection>((resolve) => (release = resolve))
      : Promise.resolve({
          info: { serverInstanceId: "first", eventCursor: 5, capabilities: [] },
        } as unknown as Connection),
  );
  project.link.start();
  await project.settle();
  project.link.hint(false);
  assert.deepEqual(project.delays, [750]);
  await project.advance(750);
  assert.equal(project.connects, 2);
  assert.deepEqual(project.subscribed, [{ key: "s1", cursor: 5 }]);
  release({
    info: { serverInstanceId: "first", eventCursor: 5, capabilities: [] },
  } as unknown as Connection);
  await project.settle();
  assert.equal(project.subscribed.length, 1);
  assert.equal(project.snapshots, 1);
});

test("a connected sidebar project is left alone by the network and the window, and a wake subscribes it again quietly", async () => {
  const project = harness();
  project.link.start();
  await project.settle();
  project.opened("s1");
  project.event("s1", 6);
  project.link.hint(false);
  await project.settle();
  assert.deepEqual(project.subscribed, [{ key: "s1", cursor: 5 }]);
  assert.deepEqual(project.delays, []);
  project.wake();
  await project.settle();
  // From its last event, with no snapshot and no loading state.
  assert.deepEqual(project.subscribed.at(-1), { key: "s2", cursor: 6 });
  assert.deepEqual(project.unsubscribed, ["s1"]);
  assert.deepEqual(project.reads, ["info"]);
  assert.equal(project.snapshots, 1);
  assert.deepEqual(project.states, ["loading", "loaded"]);
  project.event("s2", 7);
  project.event("s1", 8);
  assert.deepEqual(project.applied, [6, 7]);
  assert.deepEqual(project.delays, []);
});

test("a wake that finds another server instance loads the project again", async () => {
  const project = harness();
  project.link.start();
  await project.settle();
  project.instance = "second";
  project.wake();
  await project.settle();
  assert.deepEqual(project.delays, [500]);
  await project.advance(500);
  assert.equal(project.snapshots, 2);
});
