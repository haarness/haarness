/*
 * The agent's shell commands as Desktop shows them: one that runs in the
 * background, one sent there while it ran, and a foreground one the server
 * reports while it runs (servers with `background-commands.v1`).
 *
 * A command is known three ways, best first: what the server reports of it
 * now (`ShellCommandInfo`, from the `/commands` routes and `command.updated`),
 * the chat's own `command_state` checkpoints (which outlive the server's list
 * and a restart), and the `metadata.command` of its `bash` result. Nothing here
 * touches the DOM or the store, so the rules can be tested on their own.
 */
import type {
  Item,
  ItemData,
  ShellCommandInfo,
  ShellCommandMovedBy,
  ShellCommandState,
} from "@haarness/sdk";
import { withoutEscapes } from "./display-text";
import { thoughtDuration } from "./thinking";

/** The server lists, follows, stops and moves its agents' commands. */
export const BACKGROUND_COMMANDS = "background-commands.v1";

/** The command may still print and change state. */
export const liveCommand = (state?: string) => state === "running" || state === "stopping";

/** How a command the server no longer has is remembered: by the chat that
 * asked, since a fork names the commands of the chat it came from, which
 * still has them. */
export const goneKey = (thread: string, id: string) => `${thread}:${id}`;

export type CommandCheckpoint = Extract<ItemData, { type: "command_state" }>;

/** The last checkpoint of each command, by its call (`turn:call`, as
 * `taskKey` names a delegation). Items arrive in order, so the latest wins. */
export function commandCheckpoints(items: Item[]) {
  const checkpoints = new Map<string, CommandCheckpoint>();
  for (const item of items)
    if (item.data.type === "command_state")
      checkpoints.set(`${item.turnId ?? ""}:${item.data.call_id}`, item.data);
  return checkpoints;
}

/** `metadata.command` of a `bash` result: the command behind it. */
export function commandRef(metadata: unknown) {
  const command = record(record(metadata)?.command);
  return typeof command?.id === "string"
    ? {
        id: command.id,
        background: command.background === true,
        startedAt: typeof command.startedAt === "string" ? command.startedAt : undefined,
      }
    : undefined;
}

/** What a row knows of its command, from the best source that has each part. */
export interface CommandView {
  id?: string;
  /** It runs, or ran, in the background: its call's result is only the
   * runtime's note to the model, never the command's output. */
  background: boolean;
  state?: ShellCommandState;
  exitCode?: number;
  signal?: string;
  startedAt?: string;
  backgroundAt?: string;
  endedAt?: string;
  movedBy?: ShellCommandMovedBy;
  /** The end of its output, as its last checkpoint kept it. */
  tail?: string;
}

export function commandView(input: {
  result?: { metadata?: unknown };
  info?: ShellCommandInfo;
  checkpoint?: CommandCheckpoint;
  /** The server has listed the chat's commands, or answered that it has no
   * such command, and this one was not there: a checkpoint that says it runs
   * is from before (a restart the chat did not record, or a fork, which
   * copies the checkpoints but runs nothing). */
  forgotten?: boolean;
}): CommandView | undefined {
  const { info, checkpoint } = input;
  const ref = commandRef(input.result?.metadata);
  if (!info && !checkpoint && !ref) return undefined;
  const recorded = input.forgotten && liveCommand(checkpoint?.state) ? undefined : checkpoint?.state;
  // A checkpoint is written only for a command in the background.
  return {
    id: info?.id ?? checkpoint?.command_id ?? ref?.id,
    background: !!(info?.background || checkpoint || ref?.background),
    state: info?.state ?? recorded,
    exitCode: info ? (info.exitCode ?? undefined) : (checkpoint?.exit_code ?? undefined),
    signal: info ? (info.signal ?? undefined) : (checkpoint?.signal ?? undefined),
    startedAt: info?.startedAt ?? checkpoint?.started_at ?? ref?.startedAt,
    backgroundAt: info?.backgroundAt ?? undefined,
    endedAt: info ? (info.endedAt ?? undefined) : (checkpoint?.ended_at ?? undefined),
    movedBy: info?.movedBy ?? checkpoint?.moved_by ?? undefined,
    tail: checkpoint?.tail,
  };
}

/** How a command stands, as an English catalog key and the tone it is drawn
 * in; undefined while that is not known (a state a newer server reports). */
export interface CommandStatus {
  key: string;
  values?: Record<string, string>;
  tone: "live" | "done" | "failed" | "stopped";
}
export function commandStatus(
  view: Pick<CommandView, "state" | "exitCode" | "signal"> & { endedAt?: string },
): CommandStatus | undefined {
  switch (view.state) {
    case "running":
      return { key: "Running", tone: "live" };
    case "stopping":
      return { key: "Stopping…", tone: "live" };
    case "exited":
      if (view.exitCode === 0) return { key: "Completed", tone: "done" };
      // Ended by a signal it was not sent by Haarness, there is no code to give.
      return view.exitCode === undefined || view.exitCode === null
        ? { key: "Failed", tone: "failed" }
        : { key: "exit {code}", values: { code: String(view.exitCode) }, tone: "failed" };
    case "stopped":
      return { key: "Stopped", tone: "stopped" };
    case "timed_out":
      return { key: "Ran out of time", tone: "stopped" };
    case "interrupted":
      // A restart records when it found the command cut off; a fork's copy
      // of a command that runs on in the chat it came from has no end.
      return view.endedAt ? { key: "Cut off by a restart", tone: "stopped" } : undefined;
    default:
      return undefined;
  }
}

/** How long a command has run, as narrow as a step row reads it in the
 * interface language: "12s", "1m 12s", and past an hour "2h 5m". */
export function ranFor(ms: number, locale: string) {
  const minutes = Math.floor(Math.max(0, ms) / 60_000);
  if (minutes < 60) return thoughtDuration(Math.max(1000, ms), locale);
  const unit = (value: number, name: "hour" | "minute") =>
    new Intl.NumberFormat(locale, { style: "unit", unit: name, unitDisplay: "narrow" }).format(value);
  const rest = minutes % 60;
  const hours = unit(Math.floor(minutes / 60), "hour");
  return rest ? `${hours} ${unit(rest, "minute")}` : hours;
}

/** Elapsed between two server times (now when `to` is missing), never below zero. */
export function elapsed(from: string | undefined, to: string | number | undefined) {
  const start = from ? Date.parse(from) : NaN;
  const end = typeof to === "number" ? to : to ? Date.parse(to) : NaN;
  return Number.isFinite(start) && Number.isFinite(end) ? Math.max(0, end - start) : undefined;
}

const rank = (state: ShellCommandState) =>
  state === "running" ? 0 : state === "stopping" || state === "unknown" ? 1 : 2;

/**
 * What to keep of a command after a report of it arrives: the report, unless
 * an older one landed late (a list read before an event that came first). A
 * command never runs again once it has ended or been asked to stop, and never
 * leaves the background it went to.
 */
export function laterCommand(current: ShellCommandInfo | undefined, incoming: ShellCommandInfo) {
  if (!current || current.id !== incoming.id) return incoming;
  if (rank(incoming.state) < rank(current.state)) return current;
  if (current.background && !incoming.background) return current;
  return incoming;
}

/**
 * Whether the command a row's checkpoint or result names does not run for
 * the row's chat, when the server reports none for it there: the server
 * answered that chat no longer has it (`gone`), listed that chat's commands
 * without it (`listed`), or reports it as another chat's (`owner`, a fork
 * naming the commands of the chat it came from). A row with no chat of its
 * own (Still running) trusts what it is given.
 */
export function commandForgotten(input: {
  id?: string;
  thread?: string;
  reported: boolean;
  owner?: string;
  gone: (key: string) => boolean;
  listed: boolean;
}) {
  const { id, thread } = input;
  if (input.reported || !id || !thread) return false;
  return (!!input.owner && input.owner !== thread) || input.gone(goneKey(thread, id)) || input.listed;
}

/**
 * The server's reason for refusing to move a command (409), as a person
 * reads it: without the "conflict:" every refusal starts with, when it is
 * one short plain sentence. Undefined for one that names ids or runs long;
 * Desktop then says it in the interface language.
 */
export function moveRefusal(message: string) {
  const text = message.trim().replace(/^conflict:\s*/i, "").trim();
  if (!text || text.length > 120 || /[\n<>{}]/.test(text)) return undefined;
  if (/\bcmd-\w+|[0-9a-f]{8}-[0-9a-f]{4}-|\b[0-9a-f]{12,}\b/i.test(text)) return undefined;
  const sentence = text.charAt(0).toUpperCase() + text.slice(1);
  return /[.!?]$/.test(sentence) ? sentence : sentence + ".";
}

/** A `batch` step's own call id: the steps' calls are numbered from 1 after
 * the batch's (`call:1`, `call:2`). */
export const stepCallId = (batch: string, index: number) => `${batch}:${index + 1}`;

/**
 * Each command's line by its id, from the chat's own `bash` calls (a
 * `batch`'s steps too) and the `metadata.command` of their results: what a
 * `bash_output` or `bash_stop` step names once the server no longer lists
 * the command it read or stopped.
 */
export function commandLines(items: Item[]) {
  const calls = new Map<string, unknown>();
  const lines = new Map<string, string>();
  const line = (args: unknown) => {
    const command = record(args)?.command;
    return typeof command === "string" ? command : undefined;
  };
  for (const item of items) {
    const data = item.data;
    const key = `${item.turnId ?? ""}:${"call_id" in data ? data.call_id : ""}`;
    if (data.type === "tool_call") calls.set(key, data.arguments);
    if (data.type !== "tool_result") continue;
    const args = calls.get(key);
    const own = commandRef(data.metadata);
    const text = line(args);
    if (own && text) lines.set(own.id, text);
    // A batch's steps, each with its own result.
    const steps = record(args)?.tool_calls;
    const results = record(data.metadata)?.results;
    if (!Array.isArray(steps) || !Array.isArray(results)) continue;
    steps.forEach((step, index) => {
      const ref = commandRef(record(results[index])?.metadata);
      const text = line(record(step)?.parameters);
      if (ref && text) lines.set(ref.id, text);
    });
  }
  return lines;
}

/** The command a row shows: the one its result names, or the one the server
 * reports for its call. */
export function commandForCall(
  commands: Iterable<ShellCommandInfo>,
  call: { id?: string; callId?: string; turnId?: string | null },
) {
  for (const command of commands)
    if (call.id ? command.id === call.id : command.callId === call.callId && command.turnId === call.turnId)
      return command;
  return undefined;
}

/**
 * Output as a terminal leaves it on screen, without its escape sequences: a
 * line a program rewrote with carriage returns (a progress bar) reads as its
 * last version. A carriage return at the very end has not rewritten anything
 * yet, since a line feed may still follow it.
 */
export function screenText(raw: string) {
  return withoutEscapes(raw)
    .replace(/\r+\n/g, "\n")
    .split("\n")
    .map((line) => {
      const parts = line.replace(/\r+$/, "").split("\r");
      return parts[parts.length - 1];
    })
    .join("\n");
}

/** A page of `GET …/commands/{id}/output`. */
export interface OutputPage {
  text: string;
  start: number;
  cursor: number;
  size: number;
  more: boolean;
  settled: boolean;
  state: ShellCommandState;
  exitCode?: number | null;
}
export type ReadOutput = (query: { cursor?: number; limit?: number; tail?: boolean }) => Promise<OutputPage>;

/** What a followed command's view shows. */
export interface FollowView {
  /** The output kept so far, as a terminal would show it. */
  text: string;
  /** A page has landed: an empty `text` now means it printed nothing. */
  read: boolean;
  /** Earlier output is left out, because the view keeps only the end of a
   * long output or the server no longer kept it. */
  cut: boolean;
  /** The command has ended and every byte it printed has been read. */
  settled: boolean;
  state?: ShellCommandState;
  exitCode?: number;
  /** The server no longer has the command (it ended long ago, or Haarness
   * restarted): what the chat's checkpoint kept is all there is. */
  gone: boolean;
}

/** The most of a command's output a view keeps and reads at once. */
export const OUTPUT_KEEP = 64 * 1024;

/**
 * Follows one command's output from where its view last read. The first read
 * takes the end of what it printed, then each read continues from the
 * cursor: when `wake` is called (Desktop calls it on `command.output` and
 * `command.updated`), at once again while the server says more is waiting,
 * and every `poll` ms while the command is live in case an event was missed.
 * After a failed read it waits twice as long each time, up to `maxWait`. It
 * stops once the server says the output has settled. Reads never overlap:
 * a wake during one reads again after it. While `paused` (a hidden window)
 * it reads nothing, and `resume` catches up.
 */
export function createOutputFollow(options: {
  read: ReadOutput;
  update: (view: FollowView) => void;
  poll?: number;
  keep?: number;
  /** Whether a failed read means the server no longer has the command;
   * anything else (a computer offline, a workspace asleep) is passing, and
   * the output read so far stays. */
  gone?: (error: unknown) => boolean | Promise<boolean>;
  maxWait?: number;
  paused?: () => boolean;
  timer?: { set: (run: () => void, ms: number) => unknown; clear: (handle: unknown) => void };
}) {
  const keep = options.keep ?? OUTPUT_KEEP;
  const poll = options.poll ?? 4000;
  const gone = options.gone ?? (() => false);
  const maxWait = options.maxWait ?? 60_000;
  const timer = options.timer ?? {
    set: (run: () => void, ms: number) => setTimeout(run, ms),
    clear: (handle: unknown) => clearTimeout(handle as ReturnType<typeof setTimeout>),
  };
  let raw = "";
  let cursor: number | undefined;
  let reading = false;
  let again = false;
  let disposed = false;
  let waiting: unknown;
  /** Failed reads in a row, which space the next ones out. */
  let failures = 0;
  /** A read was due while paused. */
  let owed = false;
  const view: FollowView = { text: "", read: false, cut: false, settled: false, gone: false };
  const publish = () => options.update({ ...view, text: screenText(raw) });
  /** Keeps the end of the output, from the start of a line, so a cut never
   * leaves half a line or half an escape sequence at the top. */
  function trim() {
    if (raw.length <= keep) return;
    raw = raw.slice(-keep);
    const line = raw.indexOf("\n");
    if (line >= 0 && line < raw.length - 1) raw = raw.slice(line + 1);
    view.cut = true;
  }
  function schedule() {
    timer.clear(waiting);
    waiting = undefined;
    if (disposed || view.settled || view.gone || !poll) return;
    waiting = timer.set(wake, failures ? Math.min(poll * 2 ** failures, maxWait) : poll);
  }
  /** One page, added to what is kept; false when the read failed. */
  async function readPage() {
    const first = cursor === undefined;
    let page: OutputPage;
    try {
      page = await options.read(first ? { tail: true, limit: keep } : { cursor, limit: keep });
    } catch (error) {
      const lost = !disposed && (await Promise.resolve(gone(error)).catch(() => false));
      if (disposed) return undefined;
      if (lost) {
        view.gone = true;
        publish();
      } else failures++;
      return undefined;
    }
    if (disposed) return undefined;
    failures = 0;
    let text = page.text;
    if (first && page.start > 0) {
      // The end of a long output starts inside a line.
      const line = text.indexOf("\n");
      text = line >= 0 ? text.slice(line + 1) : text;
      view.cut = true;
    } else if (!first && page.start > cursor!) {
      // What came between was no longer kept.
      raw += raw && !raw.endsWith("\n") ? "\n…\n" : "…\n";
      view.cut = true;
    }
    raw += text;
    trim();
    cursor = page.cursor;
    view.read = true;
    view.state = page.state;
    view.exitCode = page.exitCode ?? undefined;
    view.settled = page.settled;
    publish();
    return page;
  }
  async function run() {
    reading = true;
    try {
      for (;;) {
        again = false;
        const page = await readPage();
        if (!page || view.settled || !(page.more || again)) break;
      }
    } finally {
      reading = false;
    }
    schedule();
  }
  function wake() {
    if (disposed || view.settled || view.gone) return;
    if (options.paused?.()) {
      owed = true;
      timer.clear(waiting);
      waiting = undefined;
      return;
    }
    owed = false;
    if (reading) {
      again = true;
      return;
    }
    timer.clear(waiting);
    waiting = undefined;
    void run();
  }
  return {
    wake,
    /** The connection is back: read at once, as if nothing had failed. */
    nudge() {
      failures = 0;
      wake();
    },
    /** Reads what was owed while paused. */
    resume() {
      if (owed) wake();
    },
    dispose() {
      disposed = true;
      timer.clear(waiting);
    },
  };
}

function record(value: unknown): Record<string, unknown> | undefined {
  return value !== null && typeof value === "object" && !Array.isArray(value)
    ? (value as Record<string, unknown>)
    : undefined;
}
