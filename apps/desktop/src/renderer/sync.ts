import { taskKey, type TaskProgress } from "./agent-tasks";
import type * as T from "@haarness/sdk";
import type { SetStoreFunction } from "solid-js/store";
export interface Live {
  tasks?: Record<string, TaskProgress>;
  threads: T.Thread[];
  turns: T.Turn[];
  permissions: T.PendingPermissionInfo[];
  questions: T.PendingQuestionInfo[];
  streams: Record<string, { text: string; reasoning: string }>;
  items: T.Item[];
  selected: string;
}
function merge<V extends { id: string }>(items: V[], value: V) {
  return items.some((item) => item.id === value.id)
    ? items.map((item) => (item.id === value.id ? value : item))
    : [...items, value];
}
export function reduceEvent<S extends Live>(
  state: S,
  set: SetStoreFunction<S>,
  event: T.EventEnvelope,
) {
  // The store accepts only domain snapshots and explicitly understood deltas.
  const update = set as unknown as SetStoreFunction<Live>;
  const payload = event.payload as Record<string, unknown>;
  if (
    event.type === "task.updated" &&
    typeof payload.callId === "string" &&
    typeof payload.childThreadId === "string" &&
    typeof payload.state === "string"
  ) {
    update("threads", (threads) =>
      threads.map((thread) =>
        thread.id === payload.childThreadId
          ? { ...thread, isSubagent: true }
          : thread,
      ),
    );
    const key = taskKey(event.turnId, payload.callId);
    update("tasks", (tasks) => ({
      ...tasks,
      [key]: payload as unknown as TaskProgress,
    }));
  }
  if (["thread.created", "thread.updated"].includes(event.type) && payload.id) {
    const thread = payload as T.Thread;
    const kind =
      thread.isSubagent ??
      state.threads.find((item) => item.id === thread.id)?.isSubagent;
    update("threads", (items) =>
      merge(items, {
        ...thread,
        isSubagent:
          kind ??
          Object.values(state.tasks ?? {}).some(
            (task) => task.childThreadId === thread.id,
          ),
      }),
    );
  }
  if (event.type === "item.created" && payload.id) {
    const item = payload as T.Item;
    if (item.threadId === state.selected)
      update("items", (items) =>
        merge(items, item).sort((a, b) => a.sequence - b.sequence),
      );
    if (
      item.turnId &&
      ["assistant_message", "reasoning"].includes(item.data.type)
    ) {
      const key = item.data.type === "reasoning" ? "reasoning" : "text";
      if (state.streams[item.turnId]) update("streams", item.turnId, key, "");
    }
  }
  if (
    ["turn.text_delta", "turn.reasoning_delta"].includes(event.type) &&
    event.turnId
  ) {
    const id = event.turnId;
    const stream = state.streams[id] ?? { text: "", reasoning: "" };
    const key = event.type === "turn.text_delta" ? "text" : "reasoning";
    update("streams", id, {
      ...stream,
      [key]: stream[key] + String(payload.delta ?? ""),
    });
  }
  if (event.type === "turn.started" && payload.id)
    update("turns", (items) => merge(items, payload as T.Turn));
  if (event.type === "turn.finished" && event.turnId) {
    update("turns", (items) =>
      items.filter((item) => item.id !== event.turnId),
    );
    update("streams", event.turnId, { text: "", reasoning: "" });
  }
  if (event.type === "permission.requested" && payload.permissionId)
    update("permissions", (items) => [
      ...items.filter((item) => item.permissionId !== payload.permissionId),
      payload as T.PendingPermissionInfo,
    ]);
  if (event.type === "permission.resolved")
    update("permissions", (items) =>
      items.filter((item) => item.permissionId !== payload.permissionId),
    );
  if (event.type === "question.requested" && payload.questionId)
    update("questions", (items) => [
      ...items.filter((item) => item.questionId !== payload.questionId),
      payload as T.PendingQuestionInfo,
    ]);
  if (event.type === "question.resolved")
    update("questions", (items) =>
      items.filter((item) => item.questionId !== payload.questionId),
    );
}
