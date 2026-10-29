/*
 * The open chat's work in the background, live: its agent's shell commands
 * and its subagents. The rows in the transcript, the title bar's count and the
 * Still running panel all read it from here, so they always agree.
 */
import { createEffect, createMemo, createSignal, on, onCleanup, untrack } from "solid-js";
import { reconcile } from "solid-js/store";
import type { ShellCommandInfo } from "@haarness/sdk";
import { agentTasks, agentTaskStatus } from "./agent-tasks";
import { conversationRoot } from "../thread-kinds";
import { conflictAnswer, notFoundAnswer } from "./error-text";
import { t } from "./i18n";
import { useApp } from "./state";
import {
  BACKGROUND_COMMANDS,
  createOutputFollow,
  goneKey,
  laterCommand,
  liveCommand,
  moveRefusal,
  type FollowView,
  type OutputPage,
} from "./shell-commands";

/** How long an ended command or subagent stays in the list, greyed. The
 * server lists ended commands for as long. */
export const RECENTLY_ENDED_MS = 10 * 60_000;

const [clock, setClock] = createSignal(Date.now());
let ticking = 0;
let ticker: ReturnType<typeof setInterval> | undefined;
/** The time, ticking every second while some caller has `live` work on screen. */
export function liveClock(live: () => boolean) {
  createEffect(() => {
    if (!live()) return;
    if (!ticking++) {
      setClock(Date.now());
      ticker = setInterval(() => setClock(Date.now()), 1000);
    }
    onCleanup(() => {
      if (!--ticking) clearInterval(ticker);
    });
  });
  return clock;
}

/** The server lists, follows, stops and moves this project's commands. */
export function commandsOffered() {
  const app = useApp();
  return () => !!app.state.connection?.info.capabilities.includes(BACKGROUND_COMMANDS);
}

/**
 * Whether the server no longer has command `id` of chat `thread`, after a
 * request about it failed: it answers the chat's list without it. A failure
 * that is not the server's answer (the computer offline through the relay,
 * a Cloud workspace asleep, the network) leaves that list unread too, and
 * reads as passing: the row keeps what it showed.
 */
export async function commandGone(connection: string, thread: string, id: string) {
  try {
    const listed = await window.haarness.api<ShellCommandInfo[]>(connection, "listShellCommands", [thread]);
    return !listed.some((command) => command.id === id);
  } catch {
    return false;
  }
}

/**
 * The output of command `id` of chat `thread`, followed while `active` (an
 * open row): the end of what it printed first, then what it prints, read when
 * the server says it printed more or changed state. Undefined until the first
 * read lands.
 */
export function followCommand(input: {
  id: () => string | undefined;
  thread: () => string | undefined;
  active: () => boolean;
}) {
  const app = useApp();
  const [view, setView] = createSignal<FollowView>();
  createEffect(
    on(
      () => [input.active(), input.id(), input.thread(), app.state.connection?.id] as const,
      ([active, id, thread, connection]) => {
        setView(undefined);
        if (!active || !id || !thread || !connection) return;
        const follow = createOutputFollow({
          read: (query) =>
            window.haarness.api<OutputPage>(connection, "readShellCommandOutput", [thread, id, query]),
          update: (next) => {
            setView(next);
            // Every row of it settles, not only this one.
            if (next.gone && app.state.connection?.id === connection) app.set("commandsGone", goneKey(thread, id), true);
          },
          // Only the server's not-found answer is worth asking about.
          gone: (error) => (notFoundAnswer(error) ? commandGone(connection, thread, id) : false),
          // A window nobody sees reads nothing until it shows again.
          paused: () => document.visibilityState === "hidden",
        });
        const shown = () => follow.resume();
        document.addEventListener("visibilitychange", shown);
        onCleanup(() => document.removeEventListener("visibilitychange", shown));
        // Back online, or the project's connection back: a quiet row that
        // was waiting out failures reads on at once.
        const back = () => follow.nudge();
        window.addEventListener("online", back);
        onCleanup(() => window.removeEventListener("online", back));
        createEffect(
          on(
            () => app.state.connectionStatus === "Connected",
            (connected, before) => {
              if (connected && before === false) back();
            },
            { defer: true },
          ),
        );
        follow.wake();
        createEffect(
          on(
            () => [app.state.commandOutput[id], app.state.shellCommands[id]?.state],
            () => follow.wake(),
            { defer: true },
          ),
        );
        onCleanup(follow.dispose);
      },
    ),
  );
  return view;
}

/** Keeps a command's answer from a stop or a move, unless an event already
 * brought newer news of it. */
function remember(app: ReturnType<typeof useApp>, connection: string, commands: ShellCommandInfo[]) {
  if (app.state.connection?.id !== connection) return;
  for (const command of commands) {
    const known = app.state.shellCommands[command.id];
    const next = laterCommand(known, command);
    if (next !== known) app.set("shellCommands", command.id, reconcile(next));
  }
}

/** Stops a command with the processes it started; it reads Stopping… until
 * it has exited. */
export async function stopCommand(app: ReturnType<typeof useApp>, command: ShellCommandInfo) {
  const connection = app.state.connection!.id;
  let answer: ShellCommandInfo;
  try {
    answer = await app.api<ShellCommandInfo>("stopShellCommand", command.threadId, command.id);
  } catch (error) {
    // Already gone from the server: there is nothing left to stop.
    if (notFoundAnswer(error) && (await commandGone(connection, command.threadId, command.id))) {
      if (app.state.connection?.id === connection) app.set("commandsGone", goneKey(command.threadId, command.id), true);
      return;
    }
    throw error;
  }
  remember(app, connection, [answer]);
}

/** Sends one running foreground command, or every one of the chat's, to the
 * background; its reply goes on without waiting for it. */
export async function backgroundCommands(app: ReturnType<typeof useApp>, thread: string, command?: string) {
  const connection = app.state.connection!.id;
  let moved: ShellCommandInfo[];
  try {
    moved = await app.api<ShellCommandInfo[]>("backgroundShellCommands", thread, command ? { commandId: command } : {});
  } catch (error) {
    // Refused (the chat runs as many commands there as it may): the
    // server's own reason when it is plain, else ours.
    if (!conflictAnswer(error)) throw error;
    throw new Error(
      moveRefusal(error instanceof Error ? error.message : String(error)) ??
        t("This chat can't send more commands to the background right now."),
    );
  }
  remember(app, connection, moved);
}

/**
 * The open chat's background work: its commands in the background (running
 * ones, then those that ended lately), its subagents working in the
 * background, and how many of each still run. A subagent seen in the
 * background stays listed for a while once it ends, as a command does.
 */
export function backgroundWork() {
  const app = useApp();
  const offered = commandsOffered();
  const root = createMemo(() =>
    app.state.selected ? conversationRoot(app.state.threads, app.state.selected) : "",
  );
  /** A coarse tick, so what ended lately leaves the list on time; it ticks
   * only while the list holds something that ended. It only says when to
   * look again: ages are measured against the time now, so a chat opened
   * between two ticks lists exactly what ended in the last ten minutes. */
  const [tick, setTick] = createSignal(0);
  const commands = createMemo(() => {
    if (!offered() || !root()) return [];
    const mine = Object.values(app.state.shellCommands).filter(
      (command) =>
        command.background &&
        command.rootThreadId === root() &&
        !app.state.commandsGone[goneKey(command.threadId, command.id)],
    );
    const running = mine
      .filter((command) => liveCommand(command.state))
      .sort((a, b) => a.startedAt.localeCompare(b.startedAt));
    tick();
    const now = Date.now();
    const ended = mine
      .filter(
        (command) =>
          !liveCommand(command.state) &&
          (!command.endedAt || now - Date.parse(command.endedAt) < RECENTLY_ENDED_MS),
      )
      .sort((a, b) => (b.endedAt ?? "").localeCompare(a.endedAt ?? ""));
    return [...running, ...ended];
  });
  const tasks = createMemo(() => [...agentTasks(app.state.items).values()]);
  /** Subagents seen working in the background, and when they were last seen so. */
  const [seen, setSeen] = createSignal(new Map<string, number>());
  const status = (key: string) => {
    const task = tasks().find((item) => item.key === key);
    return task ? agentTaskStatus(task, app.state.tasks?.[task.key]) : undefined;
  };
  createEffect(() => {
    const working = tasks().filter((task) => agentTaskStatus(task, app.state.tasks?.[task.key]) === "background");
    if (!working.length) return;
    const next = new Map(untrack(seen));
    const now = Date.now();
    for (const task of working) next.set(`${app.state.selected}:${task.key}`, now);
    setSeen(next);
  });
  /** The subagents listed, by their task's key: a row keeps its place (its
   * focus, its sweep, a stop under way) as the chat's items change. */
  const subagents = createMemo(
    () => {
      if (!offered()) return [];
      tick();
      const now = Date.now();
      return tasks()
        .filter((task) => {
          const state = status(task.key);
          if (state === "background") return true;
          const at = seen().get(`${app.state.selected}:${task.key}`);
          return (
            at !== undefined &&
            now - at < RECENTLY_ENDED_MS &&
            ["completed", "failed", "stopped"].includes(state ?? "")
          );
        })
        .map((task) => task.key);
    },
    [],
    { equals: (a, b) => a.length === b.length && a.every((key, index) => key === b[index]) },
  );
  const task = (key: string) => tasks().find((item) => item.key === key);
  const running = createMemo(
    () =>
      commands().filter((command) => liveCommand(command.state)).length +
      subagents().filter((key) => status(key) === "background").length,
  );
  const ended = createMemo(
    () =>
      commands().some((command) => !liveCommand(command.state)) ||
      subagents().some((key) => status(key) !== "background"),
  );
  createEffect(() => {
    if (!ended()) return;
    const timer = setInterval(() => setTick((value) => value + 1), 30_000);
    onCleanup(() => clearInterval(timer));
  });
  return { commands, subagents, task, running, status };
}
