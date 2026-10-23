import type { EventEnvelope, Thread } from "@haarness/sdk";
import type { NotificationTarget } from "../shared";
import { untitledKind } from "../chat-titles";

export interface NativeNotification {
  on(event: "click" | "close" | "failed", callback: () => void): unknown;
  show(): void;
  close(): void;
}

/**
 * How long a "Task finished" banner waits before it goes out. The daemon
 * starts the next queued prompt (or the reply to steering a stop left unread)
 * as a new root turn moments after the previous one finishes, so a banner sent
 * at once would announce the end of work that is still going. Upstream measured
 * those resumptions at 31–192 ms and holds its completion banner for 500 ms;
 * a real completion costs half a second nobody can see.
 */
export const COMPLETION_GRACE_MS = 500;

// One process-wide owner: sidebar streams, reloads and multiple windows cannot
// duplicate a system notification. Keep native objects alive until dismissed.
export function createNotifications(input: {
  enabled: () => boolean;
  focused: () => boolean;
  policy?: () => { completionNotifications: "background" | "always" | "never"; permissionNotifications: boolean; questionNotifications: boolean };
  create: (options: { title: string; body: string }) => NativeNotification;
  open: (target: NotificationTarget, owner: number) => void;
  /** The interface language's text for an English catalog key. */
  translate?: (key: string) => string;
}) {
  const say = (key: string, values: Record<string, string> = {}) =>
    (input.translate?.(key) ?? key).replace(/\{(\w+)\}/g, (match, name: string) =>
      Object.hasOwn(values, name) ? values[name] : match,
    );
  const claimed = new Set<string>();
  const visible = new Map<NativeNotification, string>();
  const revisions = new Map<string, number>();
  // A request is announced again when Auto turns on with its card shown (the
  // reviewer is deciding it) and when the reviewer leaves it to the person.
  // Only its first announce, or one that needs the person again after a
  // review, asks for attention; a review in progress never does.
  const requests = new Map<string, "reviewing" | "person">();
  const announce = (connection: string, event: EventEnvelope, payload: Record<string, unknown>) => {
    const review = payload.review as { status?: unknown } | null | undefined;
    const reviewing = review?.status === "reviewing";
    if (typeof payload.permissionId !== "string") return !reviewing;
    const key = JSON.stringify([connection, event.serverInstanceId, payload.permissionId]);
    const before = requests.get(key);
    requests.delete(key);
    requests.set(key, reviewing ? "reviewing" : "person");
    if (requests.size > 500) requests.delete(requests.keys().next().value!);
    return !reviewing && before !== "person";
  };
  // Completions waiting out COMPLETION_GRACE_MS, one per root conversation
  // (keyed by connection and root). Ending a wait with `false` drops its
  // banner: the conversation picked up again, stopped or failed, a newer
  // completion took its place, or the project went away.
  const holds = new Map<string, { connection: string; end: (kept: boolean) => void }>();
  const hold = (key: string, connection: string) => {
    holds.get(key)?.end(false);
    return new Promise<boolean>((resolve) => {
      const end = (kept: boolean) => {
        clearTimeout(timer);
        if (holds.get(key)?.end === end) holds.delete(key);
        resolve(kept);
      };
      const timer = setTimeout(() => end(true), COMPLETION_GRACE_MS);
      holds.set(key, { connection, end });
    });
  };
  const drop = (key: string) => holds.get(key)?.end(false);
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
    let title: string | undefined;
    if (event.type === "turn.started" || event.type === "turn.finished") {
      // A delegated agent's turn neither ends nor resumes the conversation.
      if (root !== event.threadId) return;
      // Work the turn started in the background (a child agent, a workflow
      // run) still owes the conversation a result; the turn that receives
      // the last one is where it ends, so this one is neither finished nor
      // failed yet, and a completion it holds is not true either.
      const owed = typeof payload.background === "number" && payload.background > 0;
      if (event.type === "turn.finished" && !owed && payload.status === "completed")
        title = "Task finished";
      else if (event.type === "turn.finished" && !owed && payload.status === "failed")
        title = "Task failed";
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
    if (event.type === "permission.requested" && !announce(connection, event, payload)) return;
    const completed = title === "Task finished";
    const conversationKey = JSON.stringify([connection, root]);
    // The conversation picked up again (the next queued prompt, a stop's
    // follow-up), stopped, failed or is waiting on background work: a
    // completion it is holding is not true.
    if (event.type.startsWith("turn.") && !completed) drop(conversationKey);
    if (!title) return; // A start, a stop or a turn background work outlives.
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
    // Failures and requests for attention are never taken back, so only a
    // completion waits; the checks below run again after the wait.
    if (completed && !(await hold(conversationKey, connection))) return;
    const conversation = await thread(root).catch(() => undefined);
    if (
      disposed ||
      revisions.get(connection) !== revision ||
      !permitted()
    )
      return;
    // The server's placeholder until a title is generated ("New chat", or
    // "New thread" from an older server) is not a name: Desktop calls that
    // chat untitled.
    const kind = untitledKind(conversation?.title);
    const name = !conversation
      ? say("Chat {id}", { id: root.slice(0, 8) })
      : kind === "chat"
        ? say("Untitled chat")
        : kind === "fork"
          ? say("Fork of untitled chat")
          : conversation.title;
    const notification = input.create({
      title: `Haarness — ${say(title)}`,
      body: `${name} · ${label}`,
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
      for (const held of [...holds.values()])
        if (held.connection === connection) held.end(false);
      for (const [notification, id] of visible) {
        if (id !== connection) continue;
        visible.delete(notification);
        notification.close();
      }
    },
    dispose() {
      disposed = true;
      for (const held of [...holds.values()]) held.end(false);
      for (const notification of visible.keys()) notification.close();
      visible.clear();
    },
  };
}
