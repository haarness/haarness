/**
 * Files an agent tab produced: captures (kept in memory until the server
 * copies them) and downloads (kept by Chrome in its own folder; the bridge
 * reads one only after the extension announces it). Uploads arrive as paths
 * the bridge staged under its uploads root, and only those paths are used.
 */
import { fileID, type FileID, type File as WireFile, type FileRef, type HostFile } from "../../core/browser";
import type { FileInput, FileListing, FileStore, StoredFile } from "../../core/driver-contract";

const MAX_CAPTURES = 20;
/** `Browser.MAX_FILE_BYTES`: what one tool result may carry. */
const MAX_FILE_BYTES = 5 * 1024 * 1024;

interface Entry {
  id: FileID;
  name: string;
  mime: string;
  bytes: number;
  state: "pending" | "completed" | "failed";
  /** The origins its bytes came from (the tab's top document, a download's URLs). */
  resources: readonly string[];
  data?: Uint8Array;
  path?: string;
}

export interface ChromeFileStore extends FileStore {
  /** A download the extension attributed to this tab, from the documents and URLs in `resources`. */
  download(id: FileID, name: string, resources: readonly string[]): void;
  finish(id: FileID, result: { path: string; bytes: number; mime: string } | { failed: true }): void;
}

export function createFileStore(uuid: () => string): ChromeFileStore {
  const files = new Map<FileID, Entry>();
  const captures: FileID[] = [];
  const unique = (resources: readonly string[]) => [...new Set(resources)].sort();
  const stored = ({ id, name, mime, bytes, state, resources }: Entry): StoredFile => ({ id, name, mime, bytes, state, resources });
  const listing = ({ id, name, mime, bytes, state }: Entry): FileListing => ({ id, name, mime, bytes, state });
  const entry = (id: FileID) => {
    const file = files.get(id);
    if (!file)
      throw new Error(
        "File ID is not retained in this tab. Call browser_files_list for this tab and use an exact returned fileID from the same tab, not a path or request ID.",
      );
    if (file.state === "pending")
      throw new Error(
        "File is still being downloaded. Check browser_files_list again and wait for state completed; do not start a duplicate download.",
      );
    if (file.state === "failed")
      throw new Error(
        "The download failed, so this file cannot be read. Inspect browser_console and browser_network_list for the cause before deciding to start it again.",
      );
    return file;
  };
  return {
    ready: Promise.resolve(),
    async save(name, mime, data, resources) {
      if (data.byteLength > MAX_FILE_BYTES)
        throw new Error(
          "Capture exceeds the 5 MiB transfer limit. Reduce screenshot maxWidth/quality or capture an element. Do not retry an identical capture.",
        );
      const id = fileID(uuid());
      files.set(id, { id, name, mime, bytes: data.byteLength, state: "completed", resources: unique(resources), data });
      captures.push(id);
      // Keep memory bounded: the oldest captures go first.
      while (captures.length > MAX_CAPTURES) files.delete(captures.shift()!);
      return id;
    },
    get: (id) => stored(entry(id)),
    list: () => [...files.values()].map(listing),
    async transfer(id, authorized): Promise<HostFile> {
      const file = entry(id);
      if (authorized && file.resources.some((resource) => !authorized.includes(resource)))
        throw new Error(
          "File source changed before export. Inspect the tab and request the file again to check its source permissions; no bytes were exported.",
        );
      if (file.data) return { id, name: file.name, mime: file.mime, data: file.data } satisfies WireFile;
      if (file.bytes > MAX_FILE_BYTES)
        throw new Error(
          "File exceeds the 5 MiB transfer limit. Choose a smaller completed file; repeating browser_files_get for this file will not help.",
        );
      return { id, name: file.name, mime: file.mime, bytes: file.bytes, path: file.path! } satisfies FileRef;
    },
    download(id, name, resources) {
      files.set(id, { id, name: name.slice(0, 2_048), mime: "application/octet-stream", bytes: 0, state: "pending", resources: unique(resources) });
    },
    finish(id, result) {
      const file = files.get(id);
      if (!file) return;
      if ("failed" in result) file.state = "failed";
      else Object.assign(file, { state: "completed", path: result.path, bytes: result.bytes, mime: result.mime || file.mime });
    },
    async dispose() {
      files.clear();
    },
  };
}

/** Whether `path` is inside `root` with no way out (`..`) — uploads may only come from there. */
export function underRoot(path: string, root: string | null): boolean {
  if (!root) return false;
  const normal = (value: string) => value.replaceAll("\\", "/").replace(/\/+$/, "");
  const base = normal(root),
    candidate = normal(path);
  if (!candidate.startsWith(base + "/")) return false;
  return !candidate
    .slice(base.length + 1)
    .split("/")
    .some((part) => part === ".." || part === "." || part === "");
}

/**
 * Uploads and drops use only files the bridge staged for this command under
 * its uploads root; the driver hands their paths to Chromium itself.
 */
export function createFileInput(uploadsRoot: () => string | null): FileInput {
  return {
    async stage(files) {
      return files.map((file) => {
        if (!("path" in file) || !underRoot(file.path, uploadsRoot()))
          throw new Error("Chrome can upload only files Haarness staged for this command. Try the upload again.");
        return file.path;
      });
    },
  };
}
