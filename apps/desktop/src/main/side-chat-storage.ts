import { createHash, randomUUID } from "node:crypto";
import { mkdir, readFile, rename, rm, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { MAX_STORED, sideRecent } from "../renderer/side-chat";

/**
 * Side chats have their own files beside the preferences, one per chat, so a
 * draft saved after each pause in typing writes that chat's file and never
 * the whole preferences JSON. `index.json` lists the chats most recently
 * used first; every save counts as a use, and the chats beyond the most
 * recent 20 are forgotten. Operations run one at a time.
 */
export function createSideChatStorage(directory: string) {
  let chain: Promise<unknown> = Promise.resolve();
  const file = (key: string) => join(directory, createHash("sha256").update(key).digest("hex") + ".json");
  const index = join(directory, "index.json");
  async function atomically(path: string, text: string) {
    await mkdir(directory, { recursive: true, mode: 0o700 });
    const temporary = path + "." + randomUUID();
    try {
      await writeFile(temporary, text, { mode: 0o600 });
      await rename(temporary, path);
    } finally {
      await rm(temporary, { force: true });
    }
  }
  async function order(): Promise<string[]> {
    try {
      const value = JSON.parse(await readFile(index, "utf8"));
      return Array.isArray(value) ? value.filter((key): key is string => typeof key === "string") : [];
    } catch {
      return [];
    }
  }
  const queue = <T>(operation: () => Promise<T>): Promise<T> => {
    const next = chain.catch(() => {}).then(operation);
    chain = next;
    return next;
  };
  const valid = (key: unknown): key is string => typeof key === "string" && !!key && key.length <= 400;
  return {
    /** Chat `key`'s side chats, or null. */
    get: (key: unknown) => {
      if (!valid(key)) return Promise.reject(new Error("Invalid side chat key"));
      return queue(async () => {
        try {
          return JSON.parse(await readFile(file(key), "utf8")) as unknown;
        } catch {
          return null;
        }
      });
    },
    /** Saves chat `key`'s side chats (null removes them), which counts as using it. */
    set: (key: unknown, value: unknown) => {
      if (!valid(key)) return Promise.reject(new Error("Invalid side chat key"));
      const text = value === null ? null : JSON.stringify(value);
      // The renderer trims to MAX_STORED; a little room for the JSON around it.
      if (text !== null && text.length > MAX_STORED + 64 * 1024)
        return Promise.reject(new Error("These side chats are too large to keep"));
      return queue(async () => {
        const current = await order();
        if (text === null) {
          await rm(file(key), { force: true });
          if (current.includes(key)) await atomically(index, JSON.stringify(current.filter((item) => item !== key)));
          return;
        }
        await atomically(file(key), text);
        const next = sideRecent(current, key);
        await atomically(index, JSON.stringify(next.order));
        for (const dropped of next.dropped) await rm(file(dropped), { force: true });
      });
    },
    /** Forgets the side chats of every chat whose key starts with `prefix` (a removed project's). */
    drop: (prefix: string) =>
      queue(async () => {
        const current = await order();
        const gone = current.filter((key) => key.startsWith(prefix));
        if (!gone.length) return;
        for (const key of gone) await rm(file(key), { force: true });
        await atomically(index, JSON.stringify(current.filter((key) => !key.startsWith(prefix))));
      }),
  };
}
