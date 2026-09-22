import test from "node:test";
import assert from "node:assert/strict";
import type { Item, ItemData, Turn } from "@haarness/sdk";
import {
  agentTasks,
  agentTaskStatus,
  agentTaskActiveTurn,
} from "../src/renderer/agent-tasks";
const item = (sequence: number, data: ItemData, turnId = "turn"): Item => ({
  id: `item-${sequence}`,
  threadId: "parent",
  turnId,
  sequence,
  createdAt: "2026-09-07T00:00:00Z",
  data,
});
const call = {
  type: "tool_call",
  call_id: "task",
  name: "task",
  arguments: { description: "Inspect routing" },
} as const;
const link = {
  type: "task_link",
  call_id: "task",
  child_thread_id: "child",
} as const;
const state = (state: string) =>
  ({
    type: "task_state",
    call_id: "task",
    child_thread_id: "child",
    child_turn_id: "child-turn",
    state,
  }) as const;

test("agent lifecycle updates keep one anchor; repeated call IDs in different turns stay separate", () => {
  const items = [
    item(1, call),
    item(2, link),
    item(3, state("background")),
    item(4, state("done")),
    item(5, call, "next-turn"),
    item(6, link, "next-turn"),
  ];
  const tasks = [...agentTasks(items).values()];
  assert.equal(tasks.length, 2);
  assert.equal(tasks[0].item.id, "item-1");
  assert.equal(agentTaskStatus(tasks[0]), "completed");
  assert.equal(agentTaskStatus(tasks[1]), "running");
  // A fork may contain the link without its call; it still provides access.
  assert.equal(
    [...agentTasks([items[1], items[3]]).values()][0].item.id,
    "item-2",
  );
});

test("background launch results are not completion, terminal checkpoints win over stale live data", () => {
  const result = item(3, {
    type: "tool_result",
    call_id: "task",
    name: "task",
    output: '<task id="child" state="running">started</task>',
    is_error: false,
  });
  const task = [
    ...agentTasks([item(1, call), item(2, link), result]).values(),
  ][0];
  assert.equal(agentTaskStatus(task), "background");
  assert.equal(
    agentTaskStatus({ ...task, checkpoint: state("cancelled") }),
    "stopped",
  );
  assert.equal(
    agentTaskStatus({ ...task, checkpoint: state("failed") }),
    "failed",
  );
  assert.equal(
    agentTaskStatus(
      { ...task, checkpoint: state("done") },
      {
        callId: "task",
        childThreadId: "child",
        childTurnId: "child-turn",
        state: "running",
      },
    ),
    "completed",
  );
});

test("continued background references settle with their original child turn", () => {
  const items = [
    item(1, call),
    item(2, link),
    item(3, state("background")),
    item(4, call, "guidance"),
    item(5, state("background"), "guidance"),
    item(6, state("done")),
    item(7, call, "new-run"),
    item(
      8,
      { ...state("background"), child_turn_id: "another-turn" },
      "new-run",
    ),
  ];
  const tasks = [...agentTasks(items).values()];
  assert.deepEqual(
    tasks.map((task) => agentTaskStatus(task)),
    ["completed", "completed", "background"],
  );
});

test("a fast child's completion wins over a late background-launch checkpoint", () => {
  const tasks = agentTasks([
    item(1, call),
    item(2, link),
    item(3, state("done")),
    item(4, state("background")),
  ]);
  assert.equal(agentTaskStatus([...tasks.values()][0]), "completed");
});

test("stop controls belong to the exact live child run, never a newer run in its conversation", () => {
  const task = [
    ...agentTasks([
      item(1, call),
      item(2, link),
      item(3, state("background")),
    ]).values(),
  ][0];
  const turn: Turn = {
    id: "child-turn",
    threadId: "child",
    status: "running",
    agent: "general",
    provider: "echo",
    model: "echo-1",
    requestId: "request",
    createdAt: "",
    updatedAt: "",
  };
  assert.equal(agentTaskActiveTurn(task, undefined, [turn]), turn);
  assert.equal(
    agentTaskActiveTurn(task, undefined, [{ ...turn, id: "new-run" }]),
    undefined,
  );
  assert.equal(
    agentTaskActiveTurn(task, undefined, [{ ...turn, threadId: "sibling" }]),
    undefined,
  );
  assert.equal(
    agentTaskActiveTurn(task, undefined, [{ ...turn, status: "completed" }]),
    undefined,
  );
  assert.equal(
    agentTaskActiveTurn({ ...task, checkpoint: state("done") }, undefined, [
      turn,
    ]),
    undefined,
  );
  // Live progress can supply identity before the durable checkpoint arrives.
  const progress = {
    callId: "task",
    childThreadId: "child",
    childTurnId: "child-turn",
    state: "running",
  };
  assert.equal(
    agentTaskActiveTurn({ ...task, checkpoint: undefined }, progress, [turn]),
    turn,
  );
  assert.equal(
    agentTaskActiveTurn({ ...task, checkpoint: undefined }, undefined, [turn]),
    undefined,
  );
  assert.equal(
    agentTaskActiveTurn(task, { ...progress, state: "cancelled" }, [turn]),
    undefined,
  );
});
