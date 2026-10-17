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
