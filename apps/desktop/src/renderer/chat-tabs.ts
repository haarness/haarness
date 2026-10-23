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

/** The ordering scope (`ordering:v1`) of a location's chat tabs. */
export const tabOrderScope = (connection: string) =>
  "list:session-tabs:" + connection;

/**
 * How a chat comes into the tab strip. A plain click in the sidebar or a
 * Search pick opens a `preview`: it takes the preview tab's place, so looking
 * through chats leaves one tab behind rather than one each. `keep` gives the
 * chat a tab that stays (a middle-click, Command/Control-click or
 * double-click), turning its preview into one. Anything else keeps an open
 * tab as it is and opens a new chat in a tab of its own.
 */
export type TabOpen = "preview" | "keep" | undefined;

/** The tabs, and which of them is the preview, once `id` is opened. */
export function openTab(
  tabs: readonly string[],
  preview: string,
  id: string,
  how: TabOpen,
): { tabs: string[]; preview: string } {
  if (tabs.includes(id))
    return { tabs: [...tabs], preview: how === "keep" && preview === id ? "" : preview };
  if (how !== "preview") return { tabs: [...tabs, id], preview };
  const at = preview ? tabs.indexOf(preview) : -1;
  if (at < 0) return { tabs: [...tabs, id], preview: id };
  return { tabs: tabs.map((tab, index) => (index === at ? id : tab)), preview: id };
}

/** The tabs a location remembers: every tab but its preview. A preview that
 * was the open chat still comes back, as the location's selected chat. */
export function keptTabs(tabs: readonly string[], preview: string) {
  return tabs.filter((tab) => tab !== preview);
}

/** A saved tab order with the replaced preview's slot handed to the chat that
 * took its place, so the new preview shows where the old one was. */
export function takeSlot(saved: readonly string[] | undefined, from: string, to: string) {
  if (!saved?.includes(from) || saved.includes(to)) return undefined;
  return saved.map((id) => (id === from ? to : id));
}
