import type * as T from "@haarness/sdk";

type Chat = Pick<T.Thread, "id" | "archived" | "isSubagent">;

/** Where a project's open chat tabs are remembered, beside its selected chat
 * (`session:<id>`). */
export const tabsKey = (connection: string) => "chat-tabs:v1:" + connection;

/**
 * Whether the reader archived a chat. An agent's own chat is archived by the
 * server when its work ends; it can still be open, and that is no reason to
 * close or drop its tab.
 */
export function archivedByReader(thread: Chat) {
  return thread.archived && !thread.isSubagent;
}

/**
 * The chat tabs to reopen from a saved list: each chat the project still has
 * and the reader has not archived, once, in the saved order. Anything else a
 * damaged or older record holds is dropped.
 */
export function savedTabs(saved: unknown, threads: readonly Chat[]): string[] {
  if (!Array.isArray(saved)) return [];
  const open = unarchivedChats(threads);
  return [
    ...new Set(
      saved.filter((id): id is string => typeof id === "string" && open.has(id)),
    ),
  ];
}

/** Whether a saved record already lists exactly these tabs, in this order. */
export function sameTabs(saved: unknown, tabs: readonly string[]) {
  const list = Array.isArray(saved) ? saved : [];
  return list.length === tabs.length && list.every((id, i) => id === tabs[i]);
}

/** The chats a project has that the reader has not archived. */
export function unarchivedChats(threads: readonly Chat[]): Set<string> {
  return new Set(
    threads
      .filter((thread) => !archivedByReader(thread))
      .map((thread) => thread.id),
  );
}

/**
 * The chats the reader archived since `before` (the chats not archived then):
 * each one is in `threads` now and archived. A chat seen for the first time
 * was not archived just now, so a project's first listing archives nothing.
 */
export function newlyArchived(
  before: ReadonlySet<string>,
  threads: readonly Chat[],
): string[] {
  return threads
    .filter((thread) => archivedByReader(thread) && before.has(thread.id))
    .map((thread) => thread.id);
}
