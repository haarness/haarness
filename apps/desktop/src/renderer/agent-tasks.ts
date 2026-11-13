import type { Item, ItemData, Turn } from "@haarness/sdk";
import { withoutEscapes } from "./display-text";

type Data<K extends ItemData["type"]> = Extract<ItemData, { type: K }>;
export interface AgentTask {
  key: string;
  item: Item;
  call?: Data<"tool_call">;
  link?: Data<"task_link" | "task_state">;
  checkpoint?: Data<"task_state">;
  result?: Data<"tool_result">;
}
export interface TaskProgress {
  callId: string;
  childThreadId: string;
  childTurnId: string;
  state: string;
  toolCalls?: number;
  lastTool?: string | null;
}
export const taskKey = (turn: string | null | undefined, call: string) =>
  `${turn ?? ""}:${call}`;

/** One stable transcript row per delegation, including every lifecycle update.
 * Call IDs are only unique within a turn; continuing a child is a separate row.
 */
export function agentTasks(items: Item[]) {
  const tasks = new Map<string, AgentTask>();
  const checkpoints = new Map<string, Data<"task_state">>();
  for (const item of items) {
    const data = item.data;
    if (
      !(
        (data.type === "tool_call" || data.type === "tool_result") &&
        data.name === "task"
      ) &&
      data.type !== "task_link" &&
      data.type !== "task_state"
    )
      continue;
    if (!("call_id" in data)) continue;
    const key = taskKey(item.turnId, data.call_id);
    const task = tasks.get(key) ?? { key, item };
    // A call answered by its first result: a later one for the same call
    // (a stopped turn's own, after the one the shutdown stored) is not what
    // the model was told. A call made again under the key (a goal's next
    // round) starts over.
    if (data.type === "tool_call") {
      if (task.result) task.result = undefined;
      task.call = data;
    }
    if (data.type === "tool_result" && !task.result) task.result = data;
    if (data.type === "task_link" || data.type === "task_state")
      task.link = data;
    if (data.type === "task_state") {
      task.checkpoint = data;
      const previous = checkpoints.get(data.child_turn_id);
      if (
        !previous ||
        !["done", "completed", "failed", "cancelled", "interrupted"].includes(
          previous.state,
        )
      )
        checkpoints.set(data.child_turn_id, data);
    }
    tasks.set(key, task);
  }
  // Guidance may reference the same running child from a later parent turn.
  // Completion is persisted under the original call; every reference to that
  // exact child turn must still settle, without settling a later child run.
  for (const task of tasks.values()) {
    const turn = task.checkpoint?.child_turn_id;
    if (turn) task.checkpoint = checkpoints.get(turn);
  }
  return tasks;
}

/** A failed task's error as its row shows it, without the escape sequences a
 * tool may have left in it; empty when the task did not fail. */
export function agentTaskError(task: AgentTask) {
  return task.result?.is_error && !stopped(task.result)
    ? withoutEscapes(task.result.output)
    : "";
}

/** The delegation's call ended in a stop, not a failure: Haarness stopped
 * before it finished (a restart, a shutdown, a turn that broke off), with
 * words written for the parent model, or a person's Stop reached it before
 * it began. */
const stopped = (result: Data<"tool_result">) => {
  const metadata = result.metadata;
  return (
    result.output.trim() === "operation cancelled" ||
    (typeof metadata === "object" &&
      metadata !== null &&
      !Array.isArray(metadata) &&
      metadata.unfinished === true)
  );
};

export function agentTaskStatus(task: AgentTask, progress?: TaskProgress) {
  const checkpoint = task.checkpoint?.state;
  const result = task.result;
  const terminal = (state?: string) => {
    if (state === "done" || state === "completed") return "completed";
    if (state === "cancelled" || state === "interrupted") return "stopped";
    if (state === "failed" || state === "error") return "failed";
  };
  // The durable checkpoint wins over a live update that raced its arrival.
  const finished = terminal(checkpoint);
  if (finished) return finished;
  const live = terminal(progress?.state);
  if (live) return live;
  if (result?.is_error) return stopped(result) ? "stopped" : "failed";
  const envelope = result?.output.match(/^<task\b[^>]*\bstate="([^"]+)"/)?.[1];
  if (terminal(envelope)) return terminal(envelope)!;
  if (progress?.state === "waiting_permission") return "approval";
  if (checkpoint === "background" || envelope === "running")
    return "background";
  if (result && !envelope) return "completed";
  return task.link ? "running" : "starting";
}

/** Only the run represented by this row may offer a stop control. A child
 * conversation can be reused later; its older rows must not stop the new run.
 */
export function agentTaskActiveTurn(
  task: AgentTask,
  progress: TaskProgress | undefined,
  turns: Turn[],
) {
  if (
    ["completed", "failed", "stopped"].includes(agentTaskStatus(task, progress))
  )
    return undefined;
  const id = task.checkpoint?.child_turn_id || progress?.childTurnId;
  return turns.find(
    (turn) =>
      turn.id === id &&
      turn.threadId === task.link?.child_thread_id &&
      (turn.status === "running" || turn.status === "queued"),
  );
}
