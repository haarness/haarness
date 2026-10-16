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
/** When this window created a chat, and on which event stream connection. */
export interface CreatedChat {
  at: number;
  stream: number;
}
/** A chat created moments ago has no history yet, so opening it needs no
 * request and nothing to wait for. A late open, or one after the event stream
 * reconnected (which may have missed its first events), loads as usual. */
export function freshChat(created: CreatedChat, now: number, stream: number) {
  return now - created.at < 3000 && created.stream === stream;
}
function merge<V extends { id: string }>(items: V[], value: V) {
  return items.some((item) => item.id === value.id)
    ? items.map((item) => (item.id === value.id ? value : item))
    : [...items, value];
}
/**
 * A chat's history as a read returned it, plus the items `events` (what
 * arrived while the read was in flight) created in `thread` after it, and
 * their rewrites. The read is the server's copy of everything it holds; an
 * item created after the server answered is not in it, and setting the read
 * alone would drop that item, and with it a reply whose streamed text it had
 * replaced. A rewrite is the item's final form, so it wins either way.
 */
export function landItems(read: T.Item[], events: T.EventEnvelope[], thread: string) {
  const items = new Map(read.map((item) => [item.id, item]));
  for (const event of events) {
    const item = event.payload as T.Item;
    if (!item?.id || item.threadId !== thread) continue;
    if (event.type === "item.updated" || (event.type === "item.created" && !items.has(item.id)))
      items.set(item.id, item);
  }
  return [...items.values()].sort((a, b) => a.sequence - b.sequence);
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
  // A slash command's message is rewritten to what the command expanded to
  // once it runs; the open chat shows that, as it does after a reopen.
  if (event.type === "item.updated" && payload.id) {
    const item = payload as T.Item;
    if (item.threadId === state.selected)
      update("items", (items) =>
        merge(items, item).sort((a, b) => a.sequence - b.sequence),
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
