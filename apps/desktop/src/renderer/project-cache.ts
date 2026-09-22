import type { Thread } from "@haarness/sdk";
import type { Bridge } from "../shared";

type Storage = Bridge["storage"];
type Manifest = { version: 2; generation: string; pages: number };
const prefix = (id: string) => "project-index:v2:" + id;
const valid = (value: unknown): value is Manifest => {
  const item = value as Manifest | null;
  return (
    item?.version === 2 &&
    typeof item.generation === "string" &&
    Number.isSafeInteger(item.pages) &&
    item.pages >= 0
  );
};
const threads = (value: unknown): Thread[] | undefined =>
  Array.isArray(value) &&
  value.every(
    (item) =>
      item && typeof item.id === "string" && typeof item.title === "string",
  )
    ? value
    : undefined;

export async function readProjectCache(
  storage: Storage,
  id: string,
): Promise<Thread[] | undefined> {
  // Immutable pages are published only when their manifest is committed.
  // Retry if another window replaced that generation during this read.
  for (let attempt = 0; attempt < 2; attempt++) {
    const manifest = await storage(prefix(id));
    if (!valid(manifest)) {
      const legacy = (await storage("project-index:v1:" + id)) as {
        version?: number;
        threads?: unknown;
      } | null;
      return legacy?.version === 1 ? threads(legacy.threads) : undefined;
    }
    const result: Thread[] = [];
    let complete = true;
    for (let page = 0; page < manifest.pages; page++) {
      const chunk = threads(
        await storage(`${prefix(id)}:${manifest.generation}:${page}`),
      );
      if (!chunk) {
        complete = false;
        break;
      }
      result.push(...chunk);
    }
    if (complete) return result;
  }
}

// Serialize writes so a slow old snapshot cannot replace a newer one. Paging
// bounds IPC payloads without dropping titles from large conversation lists.
const writes = new Map<string, Promise<void>>();
export function writeProjectCache(
  storage: Storage,
  id: string,
  values: Thread[],
) {
  const previous = writes.get(id) ?? Promise.resolve();
  const next = previous
    .catch(() => {})
    .then(async () => {
      const old = await storage(prefix(id));
      const generation = crypto.randomUUID();
      let page: Thread[] = [],
        bytes = 2,
        count = 0;
      const flush = async () => {
        if (!page.length) return;
        await storage(`${prefix(id)}:${generation}:${count++}`, page);
        page = [];
        bytes = 2;
      };
      for (const thread of values) {
        const size =
          new TextEncoder().encode(JSON.stringify(thread)).length + 1;
        if (bytes + size > 512 * 1024) await flush();
        page.push(thread);
        bytes += size;
      }
      await flush();
      await storage(prefix(id), { version: 2, generation, pages: count });
      if (valid(old))
        for (let index = 0; index < old.pages; index++)
          await storage(`${prefix(id)}:${old.generation}:${index}`, null);
      await storage("project-index:v1:" + id, null);
    });
  writes.set(id, next);
  void next
    .finally(() => {
      if (writes.get(id) === next) writes.delete(id);
    })
    .catch(() => {});
  return next;
}
