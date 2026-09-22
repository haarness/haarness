import type { Item, Thread, TodoEntry } from "@haarness/sdk";

/** Todo snapshots have no plan ID. Keep an identity across status updates and
 * reordering; a changed checklist, a clear, or a restart after completion begins
 * a new plan. The first snapshot's durable ID also survives desktop restarts. */
export function currentPlan(items: Item[], thread?: Thread) {
  if (!thread) return;
  let plan: { id: string; items: TodoEntry[] } | undefined;
  let signature = "";
  let settled = false;
  for (const item of items) {
    if (
      item.threadId !== thread.id ||
      (thread.revert != null && item.sequence > thread.revert.sequence) ||
      item.data.type !== "todo"
    )
      continue;
    const todos = item.data.items;
    if (!todos.length) {
      plan = undefined;
      signature = "";
      settled = false;
      continue;
    }
    const next = JSON.stringify(
      todos.map((todo) => todo.content.trim()).sort(),
    );
    const done = todos.every(
      (todo) => todo.status === "completed" || todo.status === "cancelled",
    );
    const id =
      !plan || signature !== next || (settled && !done) ? item.id : plan.id;
    plan = { id, items: todos };
    signature = next;
    settled = done;
  }
  return plan;
}
