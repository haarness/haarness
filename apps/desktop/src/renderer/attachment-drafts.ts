import type { Attachment } from "./media";
import { checkAttachments } from "./media";

type Update = Attachment[] | ((items: Attachment[]) => Attachment[]);
/** Loading and mutations share a per-session queue, so late hydration cannot
 * replace a drop, removal, or transfer to a newly created conversation. */
export function createAttachmentDrafts(
  storage: (key: string, value?: Attachment[]) => Promise<Attachment[]>,
  changed: () => void,
) {
  const drafts = new Map<string, Attachment[]>();
  const operations = new Map<string, Promise<void>>();
  function enqueue(key: string, action: () => Promise<void>) {
    const next = (operations.get(key) ?? Promise.resolve()).catch(() => {}).then(action);
    operations.set(key, next);
    void next.finally(() => { if (operations.get(key) === next) operations.delete(key); }).catch(() => {});
    return next;
  }
  async function hydrate(key: string) {
    if (!drafts.has(key)) {
      drafts.set(key, await storage(key));
      changed();
    }
  }
  return {
    get: (key: string) => drafts.get(key) ?? [],
    load: (key: string) => enqueue(key, () => hydrate(key)),
    update: (key: string, value: Update) => enqueue(key, async () => {
      await hydrate(key);
      const items = checkAttachments(typeof value === "function" ? value(drafts.get(key)!) : value);
      drafts.set(key, items);
      changed();
      await storage(key, items);
    }),
  };
}
