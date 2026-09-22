import { createHash, randomUUID } from "node:crypto";
import { mkdir, readFile, rename, rm, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { readAttachments, type Attachment } from "../attachments";

/** Media has its own bounded files, never the small preferences JSON. */
export function createAttachmentDraftStorage(directory: string) {
  const pending = new Map<string, Promise<Attachment[]>>();
  return (key: string, value?: unknown): Promise<Attachment[]> => {
    if (typeof key !== "string" || !key || key.length > 400)
      return Promise.reject(new Error("Invalid attachment draft key"));
    let input: Attachment[] | undefined;
    try { if (value !== undefined) input = readAttachments(value); }
    catch (error) { return Promise.reject(error); }
    const path = join(directory, createHash("sha256").update(key).digest("hex") + ".json");
    // Reads wait for preceding writes too, including across renderer reloads.
    const operation = (pending.get(key) ?? Promise.resolve()).catch(() => {}).then(async () => {
      if (input !== undefined) {
        if (!input.length) await rm(path, { force: true });
        else {
          await mkdir(directory, { recursive: true, mode: 0o700 });
          const temporary = path + "." + randomUUID();
          try {
            await writeFile(temporary, JSON.stringify(input), { mode: 0o600 });
            await rename(temporary, path);
          } finally { await rm(temporary, { force: true }); }
        }
        return input;
      }
      try { return readAttachments(JSON.parse(await readFile(path, "utf8"))); }
      catch (error) {
        if ((error as NodeJS.ErrnoException).code === "ENOENT") return [];
        throw error;
      }
    });
    pending.set(key, operation);
    void operation.finally(() => { if (pending.get(key) === operation) pending.delete(key); }).catch(() => {});
    return operation;
  };
}
