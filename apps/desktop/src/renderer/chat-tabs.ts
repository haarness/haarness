import type * as T from "@haarness/sdk";

/** Where a project's open chat tabs are remembered, beside its selected chat
 * (`session:<id>`). */
export const tabsKey = (connection: string) => "chat-tabs:v1:" + connection;

/**
 * The chat tabs to reopen from a saved list: each chat the project still has
 * and has not archived, once, in the saved order. Anything else a damaged or
 * older record holds is dropped.
 */
export function savedTabs(
  saved: unknown,
  threads: readonly Pick<T.Thread, "id" | "archived">[],
): string[] {
  if (!Array.isArray(saved)) return [];
  const open = new Set(
    threads.filter((thread) => !thread.archived).map((thread) => thread.id),
  );
  return [
    ...new Set(
      saved.filter((id): id is string => typeof id === "string" && open.has(id)),
    ),
  ];
}

/** The chats a project has that are not archived. */
export function unarchivedChats(
  threads: readonly Pick<T.Thread, "id" | "archived">[],
): Set<string> {
  return new Set(
    threads.filter((thread) => !thread.archived).map((thread) => thread.id),
  );
}

/**
 * The chats archived since `before` (the chats that were not archived then):
 * each one is in `threads` now and archived. A chat seen for the first time
 * was not archived just now, so a project's first listing archives nothing.
 */
export function newlyArchived(
  before: ReadonlySet<string>,
  threads: readonly Pick<T.Thread, "id" | "archived">[],
): string[] {
  return threads
    .filter((thread) => thread.archived && before.has(thread.id))
    .map((thread) => thread.id);
}
