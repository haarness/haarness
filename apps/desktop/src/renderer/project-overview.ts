import { createEffect, on, onCleanup, untrack } from "solid-js";
import { createStore, reconcile } from "solid-js/store";
import { readProjectCache, writeProjectCache } from "./project-cache";
import { createOverviewLink } from "./overview-link";
import { onReconnectHint } from "./reconnect-wait";
import type {
  Thread,
  Turn,
  EventEnvelope,
  PendingPermissionInfo,
  PendingQuestionInfo,
  ShellCommandInfo,
} from "@haarness/sdk";
import type { Bridge, Connection } from "../shared";
import { BACKGROUND_COMMANDS, liveCommand } from "./shell-commands";

interface Overview {
  threads: Thread[];
  turns: Turn[];
  permissions: PendingPermissionInfo[];
  questions: PendingQuestionInfo[];
  /** The agents' shell commands running in its chats. */
  commands: ShellCommandInfo[];
  /** The server answers for chats in Auto itself. */
  shared: boolean;
  loaded: boolean;
  /** Turns, approvals and questions come from the server now, not only
   * the cached chats. */
  live: boolean;
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
    | Pick<Overview, "threads" | "turns" | "permissions" | "questions" | "commands">
    | undefined;
  bridge: Bridge;
  connect: (id: string) => Promise<Connection | null>;
}) {
  const [state, set] = createStore<Overview>({
    threads: [],
    turns: [],
    permissions: [],
    questions: [],
    commands: [],
    shared: false,
    loaded: false,
    live: false,
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
    if (!value) return;
    set({ ...value, loaded: true, live: true, loading: false, error: "" });
    onCleanup(() => set("live", false));
  });
  createEffect(
    on(
      () => [input.id(), input.expanded(), input.active()] as const,
      ([id, expanded, active]) => {
        if (!expanded || active) return;
        const delegated = new Set<string>();
        const api = <T>(method: string, args: unknown[] = []) =>
          input.bridge.api<T>(id, method, args);
        const apply = (value: EventEnvelope) => {
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
          } else if (value.type === "command.updated") {
            const command = value.payload as ShellCommandInfo;
            set("commands", (commands) => [
              ...commands.filter((item) => item.id !== command.id),
              ...(liveCommand(command.state) ? [command] : []),
            ]);
          }
        };
        const link = createOverviewLink({
          id,
          bridge: input.bridge,
          connect: input.connect,
          async snapshot(info) {
            const [threads, turns, permissions, questions, commands] = await Promise.all([
              api<Thread[]>("listThreads", [{ archived: true }]),
              api<Turn[]>("activeTurns"),
              api<PendingPermissionInfo[]>("listPermissions"),
              api<PendingQuestionInfo[]>("listQuestions"),
              info.capabilities.includes(BACKGROUND_COMMANDS)
                ? api<ShellCommandInfo[]>("activeShellCommands").catch(() => [])
                : [],
            ]);
            return () => {
              set("threads", reconcile(threads, { key: "id" }));
              set("turns", turns);
              set("permissions", permissions);
              set("questions", questions);
              set("commands", commands);
              set("shared", info.capabilities.includes("approval-mode.v1"));
              set("live", true);
            };
          },
          apply,
          loading: () => set("loading", true),
          loaded: () => set({ loaded: true, loading: false, error: "" }),
          failed: (message) => set({ loading: false, error: message }),
        });
        const off = input.bridge.onEvent(link.receive);
        const offHint = onReconnectHint(() => link.hint(false));
        set("loading", true);
        untrack(link.start);
        onCleanup(() => {
          link.dispose();
          off();
          offHint();
          set("live", false);
        });
      },
    ),
  );
  return state;
}
