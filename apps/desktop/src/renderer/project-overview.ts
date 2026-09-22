import { createEffect, on, onCleanup, untrack } from "solid-js";
import { createStore, reconcile } from "solid-js/store";
import { readProjectCache, writeProjectCache } from "./project-cache";
import type {
  Thread,
  Turn,
  EventEnvelope,
  PendingPermissionInfo,
  PendingQuestionInfo,
} from "@haarness/sdk";
import type { Bridge, Connection, DesktopEvent } from "../shared";

interface Overview {
  threads: Thread[];
  turns: Turn[];
  permissions: PendingPermissionInfo[];
  questions: PendingQuestionInfo[];
  loaded: boolean;
  loading: boolean;
  error: string;
}

// Expanded inactive projects have a lightweight index stream. Transcripts and
// tools still belong to the active workspace; collapsing releases this stream.
export function createProjectOverview(input: {
  id: () => string;
  expanded: () => boolean;
  active: () => boolean;
  snapshot: () =>
    | Pick<Overview, "threads" | "turns" | "permissions" | "questions">
    | undefined;
  bridge: Bridge;
  connect: (id: string) => Promise<Connection | null>;
}) {
  const [state, set] = createStore<Overview>({
    threads: [],
    turns: [],
    permissions: [],
    questions: [],
    loaded: false,
    loading: false,
    error: "",
  });
  // The sidebar is a persistent project index, not a connection screen.
  // Render the last known titles while the background runtime is starting.
  createEffect(
    on(input.id, (id) => {
      let disposed = false;
      void readProjectCache(input.bridge.storage, id)
        .then((cached) => {
          if (!disposed && !state.loaded && cached)
            set({ threads: cached, loaded: true });
        })
        .catch(() => {});
      onCleanup(() => {
        disposed = true;
      });
    }),
  );
  createEffect(() => {
    if (!state.loaded) return;
    const id = input.id();
    const threads = JSON.parse(JSON.stringify(state.threads)) as Thread[];
    const timer = setTimeout(() => {
      void writeProjectCache(input.bridge.storage, id, threads).catch(() => {});
    }, 200);
    onCleanup(() => clearTimeout(timer));
  });
  createEffect(() => {
    if (!input.active()) return;
    const value = input.snapshot();
    if (value) set({ ...value, loaded: true, loading: false, error: "" });
  });
  createEffect(
    on(
      () => [input.id(), input.expanded(), input.active()] as const,
      ([id, expanded, active]) => {
        if (!expanded || active) return;
        let disposed = false,
          attempt = 0,
          subscription = "",
          instance = "",
          cursor = 0;
        let timer: ReturnType<typeof setTimeout> | undefined;
        const delegated = new Set<string>();
        let operation = 0;
        let early: DesktopEvent[] | undefined;
        const api = <T>(method: string, args: unknown[] = []) =>
          input.bridge.api<T>(id, method, args);
        const stop = async () => {
          const previous = subscription;
          subscription = "";
          if (previous) await input.bridge.unsubscribe(previous);
        };
        const retry = () => {
          if (disposed) return;
          clearTimeout(timer);
          timer = setTimeout(
            () => void load(),
            Math.min(30000, 500 * 2 ** Math.min(attempt++, 6)),
          );
        };
        const receive = (event: DesktopEvent) => {
          if (
            disposed ||
            event.connection !== id ||
            !["server", "connection"].includes(event.kind)
          )
            return;
          if (early) {
            early.push(event);
            return;
          }
          if (!subscription || event.id !== subscription) return;
          if (event.kind === "connection") {
            retry();
            return;
          }
          const value = event.data as
            | EventEnvelope
            | { type: "resync_required" };
          if (
            value.type === "resync_required" ||
            !("serverInstanceId" in value) ||
            value.serverInstanceId !== instance
          ) {
            retry();
            return;
          }
          if (value.eventId <= cursor) return;
          cursor = value.eventId;
          if (
            value.type === "thread.created" ||
            value.type === "thread.updated"
          ) {
            const incoming = value.payload as Thread;
            const thread = {
              ...incoming,
              isSubagent:
                incoming.isSubagent ??
                state.threads.find((thread) => thread.id === incoming.id)
                  ?.isSubagent ??
                delegated.has(incoming.id),
            };
            set("threads", (threads) => [
              ...threads.filter((item) => item.id !== thread.id),
              thread,
            ]);
          } else if (value.type === "task.updated") {
            const payload = value.payload as { childThreadId?: string };
            if (payload.childThreadId) {
              delegated.add(payload.childThreadId);
              set("threads", (threads) =>
                threads.map((thread) =>
                  thread.id === payload.childThreadId
                    ? { ...thread, isSubagent: true }
                    : thread,
                ),
              );
            }
          } else if (value.type === "permission.requested") {
            const permission = value.payload as PendingPermissionInfo;
            set("permissions", (items) => [
              ...items.filter(
                (item) => item.permissionId !== permission.permissionId,
              ),
              permission,
            ]);
          } else if (value.type === "permission.resolved") {
            const payload = value.payload as { permissionId: string };
            set("permissions", (items) =>
              items.filter(
                (item) => item.permissionId !== payload.permissionId,
              ),
            );
          } else if (value.type === "question.requested") {
            const question = value.payload as PendingQuestionInfo;
            set("questions", (items) => [
              ...items.filter(
                (item) => item.questionId !== question.questionId,
              ),
              question,
            ]);
          } else if (value.type === "question.resolved") {
            const payload = value.payload as { questionId: string };
            set("questions", (items) =>
              items.filter((item) => item.questionId !== payload.questionId),
            );
          } else if (value.type === "turn.started") {
            const turn = value.payload as Turn;
            set("turns", (turns) => [
              ...turns.filter((item) => item.id !== turn.id),
              turn,
            ]);
          } else if (value.type === "turn.finished") {
            set("turns", (turns) =>
              turns.filter((turn) => turn.id !== value.turnId),
            );
          }
        };
        async function load() {
          set("loading", true);
          const current = ++operation;
          const valid = () => !disposed && operation === current;
          try {
            await stop();
            if (!valid()) return;
            const connection = await input.connect(id);
            if (!connection || !valid()) return;
            const info = connection.info;
            const [threads, turns, permissions, questions] = await Promise.all([
              api<Thread[]>("listThreads", [{ archived: true }]),
              api<Turn[]>("activeTurns"),
              api<PendingPermissionInfo[]>("listPermissions"),
              api<PendingQuestionInfo[]>("listQuestions"),
            ]);
            if (!valid()) return;
            set("threads", reconcile(threads, { key: "id" }));
            set("turns", turns);
            set("permissions", permissions);
            set("questions", questions);
            instance = info.serverInstanceId;
            cursor = info.eventCursor ?? 0;
            // Replay starts before the REST snapshot, closing the snapshot/SSE gap.
            early = [];
            const key = await input.bridge.subscribe(id, cursor);
            if (!valid()) {
              await input.bridge.unsubscribe(key);
              return;
            }
            subscription = key;
            const pending = early;
            early = undefined;
            set({ loaded: true, loading: false, error: "" });
            attempt = 0;
            for (const event of pending) receive(event);
          } catch (error) {
            early = undefined;
            if (!valid()) return;
            set("loading", false);
            set(
              "error",
              error instanceof Error ? error.message : String(error),
            );
            retry();
          }
        }
        const off = input.bridge.onEvent(receive);
        set("loading", true);
        void untrack(load);
        onCleanup(() => {
          disposed = true;
          operation++;
          clearTimeout(timer);
          off();
          void stop().catch(() => {});
        });
      },
    ),
  );
  return state;
}
