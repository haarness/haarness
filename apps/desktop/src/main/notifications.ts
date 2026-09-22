import type { EventEnvelope, Thread } from "@haarness/sdk";
import type { NotificationTarget } from "../shared";

export interface NativeNotification {
  on(event: "click" | "close" | "failed", callback: () => void): unknown;
  show(): void;
  close(): void;
}

// One process-wide owner: sidebar streams, reloads and multiple windows cannot
// duplicate a system notification. Keep native objects alive until dismissed.
export function createNotifications(input: {
  enabled: () => boolean;
  focused: () => boolean;
  policy?: () => { completionNotifications: "background" | "always" | "never"; permissionNotifications: boolean; questionNotifications: boolean };
  create: (options: { title: string; body: string }) => NativeNotification;
  open: (target: NotificationTarget, owner: number) => void;
}) {
  const claimed = new Set<string>();
  const visible = new Map<NativeNotification, string>();
  const revisions = new Map<string, number>();
  let disposed = false;
  async function receive(
    connection: string,
    label: string,
    owner: number,
    event: EventEnvelope,
    thread: (id: string) => Promise<Thread>,
  ) {
    const payload = event.payload as Record<string, unknown> | null;
    if (!payload || !event.threadId) return;
    const root =
      event.rootThreadId ||
      (typeof payload.rootThreadId === "string"
        ? payload.rootThreadId
        : event.threadId);
    let title: string;
    if (event.type === "turn.finished") {
      if (root !== event.threadId) return;
      if (payload.status === "completed") title = "Task finished";
      else if (payload.status === "failed") title = "Task failed";
      else return; // User cancellation is not a successful completion.
    } else if (event.type === "permission.requested") title = "Approval needed";
    else if (event.type === "question.requested") title = "Answer needed";
    else return;
    const key = JSON.stringify([
      connection,
      event.serverInstanceId,
      event.eventId,
    ]);
    if (disposed || claimed.has(key)) return;
    claimed.add(key);
    if (claimed.size > 500) claimed.delete(claimed.values().next().value!);
    const permitted = () => {
      if (!input.enabled()) return false;
      const policy = input.policy?.();
      if (!policy) return !input.focused();
      if (event.type === "turn.finished") return policy.completionNotifications === "always" || (policy.completionNotifications === "background" && !input.focused());
      if (event.type === "permission.requested") return policy.permissionNotifications && !input.focused();
      return policy.questionNotifications && !input.focused();
    };
    if (!permitted()) return;
    const revision = revisions.get(connection);
    const conversation = await thread(root).catch(() => undefined);
    if (
      disposed ||
      revisions.get(connection) !== revision ||
      !permitted()
    )
      return;
    const notification = input.create({
      title: `Haarness — ${title}`,
      body: `${conversation?.title || "Conversation " + root.slice(0, 8)} · ${label}`,
    });
    const target = { connection, thread: root };
    visible.set(notification, connection);
    const remove = () => visible.delete(notification);
    notification.on("click", () => {
      if (!visible.has(notification)) return;
      remove();
      input.open(target, owner);
      notification.close();
    });
    notification.on("close", remove);
    notification.on("failed", remove);
    while (visible.size > 100) {
      const oldest = visible.keys().next().value!;
      visible.delete(oldest);
      oldest.close();
    }
    notification.show();
  }
  return {
    receive,
    forget(connection: string) {
      revisions.set(connection, (revisions.get(connection) ?? 0) + 1);
      for (const [notification, id] of visible) {
        if (id !== connection) continue;
        visible.delete(notification);
        notification.close();
      }
    },
    dispose() {
      disposed = true;
      for (const notification of visible.keys()) notification.close();
      visible.clear();
    },
  };
}
