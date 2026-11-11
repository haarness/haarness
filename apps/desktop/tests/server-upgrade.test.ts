import { test } from "node:test";
import assert from "node:assert/strict";
import { ApiError } from "@haarness/sdk";
import { serverIdle, upgradeCheckMs, upgradeWatcher, UPGRADE_CHECK_MS, type ActivityClient } from "../src/main/server-upgrade";

function lists(overrides: Partial<Record<keyof ActivityClient, () => Promise<unknown[]>>> = {}): ActivityClient {
  const empty = () => Promise.resolve([]);
  return {
    activeTurns: overrides.activeTurns ?? empty,
    listPtys: overrides.listPtys ?? empty,
    listPermissions: overrides.listPermissions ?? empty,
    listQuestions: overrides.listQuestions ?? empty,
    listWorkflowRuns: overrides.listWorkflowRuns ?? empty,
  } as ActivityClient;
}
const refused = (status: number) => () => Promise.reject(new ApiError(status, "refused", "refused", false, null));

test("a server with nothing under way is idle", async () => {
  assert.equal(await serverIdle(lists()), true);
  // Ended terminals and finished workflows hold nothing.
  assert.equal(
    await serverIdle(
      lists({
        listPtys: () => Promise.resolve([{ status: "exited" }]),
        listWorkflowRuns: () => Promise.resolve([{ status: "completed" }, { status: "failed" }, { status: "cancelled" }]),
      }),
    ),
    true,
  );
});

test("a turn, an open terminal, a waiting approval or question, or a running or paused workflow is work", async () => {
  for (const busy of [
    lists({ activeTurns: () => Promise.resolve([{ id: "t" }]) }),
    lists({ listPtys: () => Promise.resolve([{ status: "exited" }, { status: "running" }]) }),
    lists({ listPermissions: () => Promise.resolve([{ id: "p" }]) }),
    lists({ listQuestions: () => Promise.resolve([{ id: "q" }]) }),
    lists({ listWorkflowRuns: () => Promise.resolve([{ status: "running" }]) }),
    lists({ listWorkflowRuns: () => Promise.resolve([{ status: "paused" }]) }),
  ])
    assert.equal(await serverIdle(busy), false);
});

test("a list too new for an older server holds nothing; any other failure counts as work", async () => {
  assert.equal(await serverIdle(lists({ listPtys: refused(404), listWorkflowRuns: refused(400) })), true);
  assert.equal(await serverIdle(lists({ listQuestions: refused(500) })), false);
  assert.equal(await serverIdle(lists({ listPtys: () => Promise.reject(new Error("socket hang up")) })), false);
  // Every server lists its turns: not being able to is not knowing.
  assert.equal(await serverIdle(lists({ activeTurns: refused(404) })), false);
});

test("the check interval is fixed outside end-to-end tests", () => {
  assert.equal(upgradeCheckMs({}), UPGRADE_CHECK_MS);
  assert.equal(upgradeCheckMs({ HAARNESS_DESKTOP_UPGRADE_CHECK_MS: "10" }), UPGRADE_CHECK_MS);
  assert.equal(upgradeCheckMs({ HAARNESS_DESKTOP_TEST: "1", HAARNESS_DESKTOP_UPGRADE_CHECK_MS: "250" }), 250);
  assert.equal(upgradeCheckMs({ HAARNESS_DESKTOP_TEST: "1", HAARNESS_DESKTOP_UPGRADE_CHECK_MS: "-1" }), UPGRADE_CHECK_MS);
});

/** A watcher on a hand-driven clock. */
function harness(state: { pending: Map<string, string>; idle: boolean[]; fail?: number }) {
  const due: Array<{ run: () => void; ms: number; live: boolean }> = [];
  const replaced: string[] = [];
  const logged: string[] = [];
  const watcher = upgradeWatcher<string>({
    pending: (id) => state.pending.get(id),
    idle: async () => state.idle.shift() ?? false,
    replace: async (id) => {
      replaced.push(id);
      if (state.fail && state.fail-- > 0) throw new Error("server would not stop");
      state.pending.delete(id);
    },
    every: 1000,
    log: (message) => logged.push(message),
    schedule: (run, ms) => {
      const entry = { run, ms, live: true };
      due.push(entry);
      return entry as unknown as ReturnType<typeof setTimeout>;
    },
    cancel: (timer) => {
      (timer as unknown as { live: boolean }).live = false;
    },
  });
  /** Runs what is due, then lets the checks it started finish. */
  async function tick() {
    const now = due.splice(0).filter((entry) => entry.live);
    for (const entry of now) entry.run();
    for (let index = 0; index < 10; index++) await Promise.resolve();
  }
  return { watcher, due, replaced, logged, tick };
}

test("a busy server of an earlier build is replaced at the first check that finds it idle, and then left alone", async () => {
  const state = { pending: new Map([["local:/p", "client"]]), idle: [false, false, true] };
  const { watcher, due, replaced, tick } = harness(state);
  watcher.watch("local:/p");
  watcher.watch("local:/p");
  assert.equal(due.length, 1, "one watch per connection");
  assert.equal(due[0].ms, 1000);
  await tick();
  await tick();
  assert.deepEqual(replaced, []);
  await tick();
  assert.deepEqual(replaced, ["local:/p"]);
  assert.equal(due.length, 0, "an upgraded connection is no longer checked");
  assert.deepEqual(watcher.watching, []);
});

test("a replacement that fails, or finds the work back, is tried again at the next check", async () => {
  const state = { pending: new Map([["local:/p", "client"]]), idle: [true, true], fail: 1 };
  const { watcher, replaced, logged, tick, due } = harness(state);
  watcher.watch("local:/p");
  await tick();
  assert.deepEqual(replaced, ["local:/p"]);
  assert.match(logged[0], /server would not stop/);
  assert.equal(due.length, 1);
  await tick();
  assert.deepEqual(replaced, ["local:/p", "local:/p"]);
  assert.equal(due.length, 0);
});

test("a connection that closed, or was upgraded by opening it again, stops being checked", async () => {
  const state = { pending: new Map([["local:/a", "a"], ["local:/b", "b"]]), idle: [] as boolean[] };
  const { watcher, replaced, tick, due } = harness(state);
  watcher.watch("local:/a");
  watcher.watch("local:/b");
  state.pending.delete("local:/a");
  watcher.stop("local:/b");
  await tick();
  assert.deepEqual(replaced, []);
  assert.equal(due.length, 0);
});
