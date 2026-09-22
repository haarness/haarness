import type { Item, Thread } from "@haarness/sdk";

/** Compatibility with an older daemon that is finishing existing work.
 * Current servers include the durable flag. Only old parent groups need a
 * transcript read, and one read handles every child of that parent.
 */
export async function withThreadKinds(
  threads: Thread[],
  read: (id: string) => Promise<Item[]>,
): Promise<Thread[]> {
  const parents = [
    ...new Set(
      threads
        .filter((thread) => thread.isSubagent === undefined && thread.parentId)
        .map((thread) => thread.parentId!),
    ),
  ];
  if (!parents.length) return threads;
  const delegated = new Set<string>();
  const ownership = new Map(
    threads.map((thread) => [thread.id, thread.parentId]),
  );
  // Bound concurrency, not the number of projects or conversations.
  for (let index = 0; index < parents.length; index += 4) {
    await Promise.all(
      parents.slice(index, index + 4).map(async (id) => {
        const items = await read(id).catch(() => []);
        for (const item of items) {
          const data = item.data;
          if (data.type === "task_link" || data.type === "task_state")
            if (ownership.get(data.child_thread_id) === id)
              delegated.add(data.child_thread_id);
        }
      }),
    );
  }
  return threads.map((thread) =>
    thread.isSubagent === undefined
      ? { ...thread, isSubagent: delegated.has(thread.id) }
      : thread,
  );
}

/** A delegated conversation belongs under its nearest independent ancestor. */
export function conversationRoot(threads: Thread[], id: string) {
  const byId = new Map(threads.map((thread) => [thread.id, thread]));
  const seen = new Set<string>();
  let thread = byId.get(id);
  while (thread?.isSubagent && thread.parentId && !seen.has(thread.id)) {
    seen.add(thread.id);
    id = thread.parentId;
    thread = byId.get(id);
  }
  return id;
}
