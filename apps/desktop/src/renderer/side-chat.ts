/**
 * Side chats: `/btw` threads beside a chat. Each asks side questions about
 * the chat's conversation without adding to it; a follow-up sends the thread
 * so far. Nothing here changes the chat itself. Pure rules only, so they can
 * be tested; the store and the panel live in SideChat.tsx.
 */

export type SideTurnState = "pending" | "done" | "error" | "stopped";

export interface SideTurn {
  question: string;
  answer: string;
  state: SideTurnState;
  error?: string;
}

export interface SideThread {
  id: string;
  turns: SideTurn[];
  /** What its box holds, kept with the thread. */
  draft: string;
  time: number;
}

/** One chat's side chats. `front` is the thread whose tab is in front. */
export interface SideChats {
  threads: SideThread[];
  front?: string;
}

/** Threads a chat keeps; the oldest goes first, never one still answering. */
export const MAX_THREADS = 12;
/** Chats whose side chats are kept, the most recently used. */
export const MAX_CHATS = 20;
/** Finished questions a follow-up sends at most: the thread has no compaction. */
export const MAX_HISTORY = 40;
/** Stored size of one chat's side chats; the oldest questions give way. */
export const MAX_STORED = 1024 * 1024;

/** The finished questions, which are what a follow-up sends as its thread. */
export function sideHistory(turns: readonly SideTurn[]) {
  return turns
    .filter((turn) => turn.state === "done" && turn.answer.trim() && turn.question.trim())
    .map((turn) => ({ question: turn.question, answer: turn.answer }))
    .slice(-MAX_HISTORY);
}

/** A tab's name: its first question, or nothing before one is asked. */
export function sideTitle(thread: Pick<SideThread, "turns">) {
  const question = thread.turns[0]?.question.replace(/\s+/g, " ").trim();
  if (!question) return undefined;
  return question.length > 32 ? question.slice(0, 31).trimEnd() + "…" : question;
}

export function sideBusy(thread: Pick<SideThread, "turns"> | undefined) {
  return thread?.turns.at(-1)?.state === "pending";
}

/** A new thread, added after the others; the oldest give way to keep at most
 * {@link MAX_THREADS}, never one still answering. */
export function sideAdd(chat: SideChats, thread: SideThread): SideChats {
  const threads = [...chat.threads, thread];
  while (threads.length > MAX_THREADS) {
    const old = threads.findIndex((item) => !sideBusy(item));
    if (old < 0) break;
    threads.splice(old, 1);
  }
  return { threads, front: thread.id };
}

/** Without thread `id`; the neighbour comes forward if it was in front. */
export function sideRemove(chat: SideChats, id: string): SideChats {
  const at = chat.threads.findIndex((item) => item.id === id);
  if (at < 0) return chat;
  const next = chat.threads[at + 1] ?? chat.threads[at - 1];
  return {
    threads: chat.threads.filter((item) => item.id !== id),
    front: chat.front === id ? next?.id : chat.front,
  };
}

const isTurn = (value: unknown): value is SideTurn => {
  const turn = value as Partial<SideTurn> | null;
  return (
    !!turn &&
    typeof turn.question === "string" &&
    typeof turn.answer === "string" &&
    ["pending", "done", "error", "stopped"].includes(String(turn.state)) &&
    (turn.error === undefined || typeof turn.error === "string")
  );
};

/**
 * Stored side chats as this version reads them. Anything malformed is left
 * out; an answer that was still being written belonged to a request that is
 * gone now, so it comes back stopped.
 */
export function sideRestore(value: unknown): SideChats {
  const stored = value as Partial<SideChats> | null;
  if (!stored || !Array.isArray(stored.threads)) return { threads: [] };
  const threads = stored.threads
    .filter(
      (thread): thread is SideThread =>
        !!thread && typeof thread.id === "string" && Array.isArray(thread.turns),
    )
    .map((thread) => ({
      id: thread.id,
      draft: typeof thread.draft === "string" ? thread.draft : "",
      time: typeof thread.time === "number" ? thread.time : 0,
      turns: thread.turns
        .filter(isTurn)
        .map((turn) => (turn.state === "pending" ? { ...turn, state: "stopped" as const } : turn)),
    }))
    .slice(-MAX_THREADS);
  const front = threads.some((thread) => thread.id === stored.front) ? stored.front : undefined;
  return { threads, front };
}

/** What is stored: within {@link MAX_STORED}, the oldest questions of the
 * oldest threads giving way first. */
export function sideStored(chat: SideChats): SideChats {
  const value: SideChats = {
    front: chat.front,
    threads: chat.threads.map((thread) => ({ ...thread, turns: [...thread.turns] })),
  };
  while (JSON.stringify(value).length > MAX_STORED) {
    const thread = value.threads.find((item) => item.turns.length > 1) ?? value.threads[0];
    if (!thread) break;
    if (thread.turns.length > 1) thread.turns.shift();
    else value.threads.shift();
  }
  return value;
}

/** The most recently used chats first, at most {@link MAX_CHATS}, and the
 * ones that fell off the end. */
export function sideRecent(order: readonly string[], used: string) {
  const next = [used, ...order.filter((key) => key !== used)];
  return { order: next.slice(0, MAX_CHATS), dropped: next.slice(MAX_CHATS) };
}

/** A /btw typed in the composer: its question ("" for a bare /btw), or
 * undefined for anything else. */
export function btwQuestion(text: string) {
  const match = /^\/btw(?:\s+([\s\S]*))?$/.exec(text.trim());
  return match ? (match[1] ?? "").trim() : undefined;
}

/** The conversation's narrowest width beside the side chat (the side panel keeps the same). */
export const CHAT_MIN = 480;
/** The side chat's narrowest and widest, and where it starts. */
export const SIDE_MIN = 300;
export const SIDE_MAX = 640;
export const SIDE_WIDTH = 380;
/** The side panel's narrowest. */
export const DOCK_MIN = 360;
/** A divider between two columns. */
const RULE = 1;

/**
 * Where the side chat stands in a workbench `width` wide, and how wide it is:
 * a column of its own between the conversation and the side panel when they
 * all fit (`column`); in the side panel's place when only the conversation and
 * the side chat do (`instead`); over the whole workbench when not even those
 * do, or when the side panel itself is laid over everything in a narrow
 * window (`over`). `dock` is how the side panel stands: not shown, a column,
 * expanded over the conversation, or laid over everything.
 */
export function sidePlace(input: {
  width: number;
  wanted: number;
  dock: "none" | "column" | "expanded" | "overlay";
}): { place: "column" | "instead" | "over"; width: number } {
  const fit = (room: number) => Math.max(SIDE_MIN, Math.min(input.wanted, SIDE_MAX, room));
  if (input.dock === "overlay") return { place: "over", width: input.width };
  if (input.dock === "expanded")
    return input.width >= SIDE_MIN + RULE + DOCK_MIN
      ? { place: "column", width: fit(input.width - RULE - DOCK_MIN) }
      : { place: "over", width: input.width };
  const three = CHAT_MIN + RULE + SIDE_MIN + RULE + DOCK_MIN;
  if (input.dock === "column" && input.width >= three)
    return { place: "column", width: fit(input.width - three + SIDE_MIN) };
  if (input.width >= CHAT_MIN + RULE + SIDE_MIN)
    return { place: input.dock === "column" ? "instead" : "column", width: fit(input.width - CHAT_MIN - RULE) };
  return { place: "over", width: input.width };
}
