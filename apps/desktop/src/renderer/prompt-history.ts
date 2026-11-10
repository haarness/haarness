import type { Attachment } from "./media";
import { readContext, type PromptContext } from "./prompt-context";

export interface PromptHistoryEntry {
  text: string;
  thread?: string;
  attachments: Attachment[];
  context: PromptContext[];
}
export const historyKey = (workspace: string) =>
  `prompt-history:v1:chat:${workspace}`;
const MAX_ENTRIES = 200;
/** Only this many of the newest entries may bring back attachments or
 * context; an older one is kept only while it is plain text. */
const MAX_RICH_ENTRIES = 100;
const MAX_BYTES = 1536 * 1024;
/** Whether recalling the entry brings back more than its text. */
const rich = (entry: PromptHistoryEntry) =>
  entry.attachments.length > 0 || entry.context.length > 0;
/** History within its bounds, newest first. */
const bounded = (entries: PromptHistoryEntry[]) =>
  entries
    .filter((entry, index) => index < MAX_RICH_ENTRIES || !rich(entry))
    .slice(0, MAX_ENTRIES);
export const copyEntry = (entry: PromptHistoryEntry): PromptHistoryEntry =>
  structuredClone(entry);

export function readHistory(value: unknown): PromptHistoryEntry[] {
  if (!Array.isArray(value)) return [];
  const entries = value
    .filter(
      (entry): entry is PromptHistoryEntry =>
        !!entry &&
        typeof entry.text === "string" &&
        Array.isArray(entry.attachments) &&
        entry.attachments.every(
          (file: Attachment) =>
            !!file &&
            typeof file.name === "string" &&
            typeof file.data === "string",
        ) &&
        Array.isArray(entry.context) &&
        readContext(entry.context).length === entry.context.length,
    );
  return bounded(entries);
}
export function prependHistory(
  entries: PromptHistoryEntry[],
  entry: PromptHistoryEntry,
) {
  if (!entry.text.trim() && !entry.attachments.length && !entry.context.length)
    return entries;
  if (JSON.stringify(entries[0]) === JSON.stringify(entry)) return entries;
  if (entry.thread && entries[0] && !entries[0].thread && samePrompt(entries[0], entry))
    return [copyEntry(entry), ...entries.slice(1)];
  return bounded([copyEntry(entry), ...entries.filter(item => item.thread !== entry.thread || !samePrompt(item, entry))]);
}
function samePrompt(a: PromptHistoryEntry, b: PromptHistoryEntry) {
  return a.text === b.text && JSON.stringify(a.attachments) === JSON.stringify(b.attachments)
    && JSON.stringify(a.context) === JSON.stringify(b.context);
}
export function orderHistory(entries: PromptHistoryEntry[], thread?: string) {
  const result: PromptHistoryEntry[] = [];
  const ordered = thread ? [...entries.filter(entry => entry.thread === thread), ...entries.filter(entry => entry.thread !== thread)] : entries;
  for (const entry of ordered) if (!result.some(item => samePrompt(item, entry))) result.push(entry);
  return result;
}
/** Capture matches once: a recalled entry must not become the next search prefix. */
export function matchingHistory(entries: PromptHistoryEntry[], draft: string) {
  return entries.filter(entry => entry.text.startsWith(draft));
}
/** Large media remains recallable in memory, without exceeding desktop storage limits. */
export function persistedHistory(entries: PromptHistoryEntry[]) {
  let bytes = 2;
  return entries.filter((entry) => {
    const size = new TextEncoder().encode(JSON.stringify(entry)).length + 1;
    if (bytes + size > MAX_BYTES) return false;
    bytes += size;
    return true;
  });
}
export function canRecall(
  direction: "up" | "down",
  text: string,
  start: number,
  end: number,
  index: number,
) {
  if (start !== end) return false;
  return index >= 0
    ? start === 0 || start === text.length
    : direction === "up"
      ? start === 0
      : start === text.length;
}
export function navigateHistory(
  entries: PromptHistoryEntry[],
  index: number,
  direction: "up" | "down",
) {
  const next = direction === "up" ? index + 1 : index - 1;
  return next >= -1 &&
    next < entries.length &&
    next !== index &&
    !(index === -1 && direction === "down")
    ? next
    : undefined;
}

export function createHistoryStore(
  storage: (key: string, value?: unknown) => Promise<unknown>,
) {
  const cache = new Map<string, PromptHistoryEntry[]>();
  const hydrated = new Set<string>();
  const loading = new Map<string, Promise<void>>();
  const pending = new Map<string, PromptHistoryEntry[]>();
  let writes = Promise.resolve();
  const load = (key: string) => {
    if (hydrated.has(key)) return Promise.resolve();
    if (!loading.has(key))
      loading.set(key, storage(key).then(value => {
        let entries = readHistory(value);
        for (const entry of pending.get(key) ?? []) entries = prependHistory(entries, entry);
        cache.set(key, entries);
        pending.delete(key);
        hydrated.add(key);
      }).finally(() => loading.delete(key)));
    return loading.get(key)!;
  };
  return {
    load,
    entries: (key: string, thread?: string) => orderHistory(cache.get(key) ?? [], thread),
    async record(key: string, entry: PromptHistoryEntry) {
      // Recall remains usable even when disk hydration fails. Keep the pending
      // entries for a later merge so unread older history is never overwritten.
      cache.set(key, prependHistory(cache.get(key) ?? [], entry));
      if (!hydrated.has(key)) pending.set(key, [...(pending.get(key) ?? []), copyEntry(entry)]);
      await load(key);
      const value = persistedHistory(cache.get(key) ?? []);
      const write = writes.catch(() => {}).then(() => storage(key, value)).then(() => {});
      writes = write;
      await write;
    },
  };
}
