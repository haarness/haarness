import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { randomUUID } from "node:crypto";
import { HaarnessCloud, ApiError } from "../typescript/dist/index.js";
const auth = JSON.parse(await readFile(process.argv[2], "utf8"));
const cloud = new HaarnessCloud({
  baseUrl: auth.apiUrl,
  token: auth.access_token,
});
const delay = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
const poll = async (fn, test) => {
  const end = Date.now() + 120000;
  let result;
  while (Date.now() < end) {
    result = await fn();
    if (test(result)) return result;
    await delay(500);
  }
  throw new Error("Timeout " + JSON.stringify(result));
};
const key = () => ({ headers: { "Idempotency-Key": randomUUID() } });
const empty =
  "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
let workspace;
let eventTask;
const changes = [];
try {
  eventTask = (async () => {
    for await (const event of cloud.events({ reconnect: true })) {
      if (event.type === "entity.changed") changes.push(event.entity);
    }
  })().catch((error) => {
    if (!String(error).includes("closed") && error.name !== "AbortError")
      throw error;
  });
  workspace = await cloud.newWorkspace({
    title: "SDK TypeScript shared workspace",
    configuration: { model: "router/fixture/echo", diskGb: 1 },
    start: true,
  });
  await poll(
    () => workspace.get(),
    (s) => s.observedState === "running",
  );
  const a = await workspace.chats.create({ title: "first" }),
    b = await workspace.chats.create({ title: "second" });
  assert.equal((await workspace.chats.list()).length, 2);
  await b.rename("renamed");
  assert.equal((await b.get()).title, "renamed");
  const runtime = await workspace.connect();
  const written = await runtime.writeFile({
    path: "typescript-shared.txt",
    content: "shared 雪",
    revision: empty,
  });
  assert.equal(
    (await runtime.readFile({ path: written.path })).content,
    "shared 雪",
  );
  const result = await a.ask("hello fixture");
  assert.equal(result.turn.status, "completed");
  await assert.rejects(b.wait(result.turn.id), /another chat/);
  assert.equal((await b.messages()).length, 0);
  const schedule = await cloud.createCloudSchedule(
    {
      workspaceId: workspace.id,
      chatId: a.id,
      name: "TS schedule",
      prompt: "timed hello",
      at: new Date(Date.now() + 4000).toISOString(),
      everySeconds: 60,
    },
    key(),
  );
  const id = schedule.id;
  assert.equal(
    (await cloud.listCloudSchedules({ workspaceId: workspace.id })).items[0].id,
    id,
  );
  await poll(
    () => cloud.cloudScheduleRuns(id),
    (p) => p.items[0]?.status === "completed",
  );
  const fresh = await cloud.getCloudSchedule(id);
  const paused = await cloud.updateCloudSchedule(id, {
    version: fresh.version,
    enabled: false,
  });
  assert.equal(paused.enabled, false);
  await assert.rejects(
    cloud.updateCloudSchedule(id, { version: fresh.version, enabled: true }),
    (e) => e instanceof ApiError && e.status === 409,
  );
  const requestKey = key(),
    run = await cloud.runCloudSchedule(id, requestKey);
  assert.equal((await cloud.runCloudSchedule(id, requestKey)).id, run.id);
  await poll(
    () => cloud.cloudScheduleRuns(id),
    (p) => p.items.find((r) => r.id === run.id)?.status === "completed",
  );
  await workspace.stop();
  await poll(
    () => workspace.get(),
    (s) => s.observedState === "stopped",
  );
  await workspace.start();
  await poll(
    () => workspace.get(),
    (s) => s.observedState === "running",
  );
  // Reuse the same runtime handle across a generation change; its stale ticket must reattach.
  assert.equal(
    (await runtime.readFile({ path: written.path })).content,
    "shared 雪",
  );
  await cloud.deleteCloudSchedule(id);
  await assert.rejects(
    cloud.getCloudSchedule(id),
    (e) => e instanceof ApiError && e.status === 404,
  );
  await runtime.mutateFile({
    action: "delete",
    path: written.path,
    confirm: true,
  });
  await b.archive();
  assert.equal((await workspace.chats.list()).length, 1);
  assert.ok(
    changes.includes("schedule") && changes.includes("schedule_run"),
    JSON.stringify(changes),
  );
  console.log(
    "typescript: workspace/chats, shared files, all schedule endpoints, live schedule WebSocket events and runtime generation reattachment PASS",
  );
} finally {
  if (workspace) {
    try {
      const deletion = await workspace.delete();
      assert.equal(
        (
          await poll(
            () => cloud.getOperation(deletion.operation.id),
            (r) => ["succeeded", "failed", "cancelled"].includes(r.status),
          )
        ).status,
        "succeeded",
      );
    } finally {
      await workspace.close();
    }
  }
  cloud.close();
  await eventTask;
}
