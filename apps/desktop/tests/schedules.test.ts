import { test } from "node:test";
import assert from "node:assert/strict";
import type { Thread } from "@haarness/sdk";
import {
  describeSchedule,
  localDate,
  scheduled,
  splitSchedules,
  triggerArgs,
  triggerForm,
} from "../src/renderer/schedule-data";

test("schedule pickers round-trip interval, calendar and custom triggers", () => {
  for (const seconds of [60, 90, 3600, 172800])
    assert.deepEqual(triggerArgs(triggerForm({ type: "interval", seconds })), [
      "--every",
      `${seconds}s`,
    ]);
  for (const expression of [
    "30 9 * * *",
    "0 18 * * 1-5",
    "15 8 * * 2",
    "*/15 9-17 * * 1-5",
  ])
    assert.deepEqual(triggerArgs(triggerForm({ type: "cron", expression })), [
      "--cron",
      expression,
    ]);
  assert.equal(triggerForm({ type: "cron", expression: "0 9 * * 7" }).day, 0);
  assert.equal(
    describeSchedule({ type: "cron", expression: "30 9 * * 1-5" }),
    "Weekdays at 09:30",
  );
  assert.equal(
    describeSchedule({ type: "interval", seconds: 3600 }),
    "Every 1 hour",
  );
});
test("schedule pickers validate intervals, times and one-shot dates", () => {
  const daily = triggerForm({ type: "cron", expression: "30 9 * * *" });
  assert.deepEqual(triggerArgs({ ...daily, frequency: "weekly" }), [
    "--cron",
    "30 9 * * 1",
  ]);
  const form = triggerForm();
  assert.throws(
    () => triggerArgs({ ...form, frequency: "interval", amount: 0 }),
    /at least one minute/,
  );
  assert.throws(() => triggerArgs({ ...form, time: "25:00" }), /valid time/);
  assert.throws(
    () => triggerArgs({ ...form, frequency: "once", at: "2000-01-01T09:00" }),
    /one minute/,
  );
  const at = localDate(new Date(Date.now() + 86400000));
  assert.deepEqual(triggerArgs({ ...form, frequency: "once", at }), [
    "--at",
    new Date(at).toISOString(),
  ]);
});
const thread = (
  id: string,
  title: string,
  schedule?: Thread["schedule"],
  parentId?: string,
): Thread => ({
  id,
  title,
  schedule,
  parentId: parentId ?? null,
  isSubagent: false,
  archived: false,
  agent: "build",
  provider: "echo",
  model: "echo-1",
  ultracode: false,
  createdAt: `2026-09-08T0${id.slice(-1)}:00:00Z`,
  updatedAt: "2026-09-08T20:00:00Z",
});
test("scheduled groups retain renamed runs and isolate same-name schedules and checkouts", () => {
  const rows = [
    thread("1", "A chat"),
    thread("2", "Renamed run", { id: "job-a", name: "Daily" }),
    thread("3", "Another run", { id: "job-a", name: "Daily" }),
    thread("4", "Same name", { id: "job-b", name: "Daily" }),
    thread("5", "Fork of run", undefined, "2"),
  ].map((thread) => ({
    key: thread.id,
    thread,
    location: { id: "project-a" },
  }));
  const split = splitSchedules([
    ...rows,
    { ...rows[1], key: "other", location: { id: "project-b" } },
  ]);
  assert.deepEqual(
    split.chats.map((row) => row.key),
    ["1", "5"],
  );
  assert.equal(split.groups.length, 3);
  assert.deepEqual(
    split.groups
      .find((group) => group.runs.some((run) => run.key === "3"))!
      .runs.map((row) => row.key),
    ["3", "2"],
  );
  assert.equal(
    scheduled(thread("1", "Schedule: write documentation")),
    undefined,
  );
  assert.equal(
    scheduled(thread("1", "Schedule: Old @ 2026-09-08T10:00:00+00:00"))?.name,
    "Old",
  );
  assert.equal(
    scheduled(
      thread(
        "1",
        "Schedule: Old @ 2026-09-08T10:00:00+00:00",
        undefined,
        "parent",
      ),
    ),
    undefined,
  );
});

test("legacy runs join a uniquely identified schedule in the same checkout", () => {
  const values = [
    thread("2", "Current", { id: "job", name: "Daily" }),
    thread("1", "Schedule: Daily @ 2026-09-08T01:00:00Z"),
  ].map((thread) => ({ thread, key: thread.id, location: { id: "local" } }));
  assert.equal(splitSchedules(values).groups.length, 1);
});


test("schedule descriptions use translated labels and locale dates", () => {
  const labels: Record<string, string> = { "Every day": "毎日", "Weekly": "毎週", "Monday": "月曜日", "At an interval": "一定間隔", "Hours": "時間", "Custom cron": "カスタムcron" };
  const translate = (key: string) => labels[key] ?? key;
  assert.equal(describeSchedule({ type: "cron", expression: "30 9 * * *" }, translate, "ja"), "毎日 · 09:30");
  assert.equal(describeSchedule({ type: "cron", expression: "0 9 * * 1" }, translate, "ja"), "毎週 · 月曜日 · 09:00");
  assert.equal(describeSchedule({ type: "interval", seconds: 7200 }, translate, "ja"), "一定間隔 · 2 時間");
});
