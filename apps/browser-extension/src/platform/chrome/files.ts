/**
 * Files an agent tab produced: captures (kept in memory until the server
 * copies them) and downloads (kept by Chrome in its own folder; the bridge
 * reads one only after the extension announces it). Uploads arrive as paths
 * the bridge staged under its uploads root, and only those paths are used.
 */
import type { FileID, File as WireFile, FileRef } from "../../core/browser";
import type { FileInput, FileStore, StoredFile } from "../../core/driver-contract";
import type { ChromeTransport } from "./cdp";

const MAX_CAPTURES = 20;

interface Entry extends StoredFile {
  bytes: number;
  data?: Uint8Array;
  path?: string;
}

export interface ChromeFileStore extends FileStore {
  /** A download the extension attributed to this tab. */
  download(id: FileID, name: string): void;
  finish(id: FileID, result: { path: string; bytes: number; mime: string } | { failed: true }): void;
}

export function createFileStore(uuid: () => string): ChromeFileStore {
  const files = new Map<FileID, Entry>();
  const captures: FileID[] = [];
  const meta = ({ id, name, mime, bytes, state }: Entry): StoredFile => ({ id, name, mime, bytes, state });
  return {
    ready: Promise.resolve(),
    async save(name, mime, data) {
      const id = `file_${uuid()}` as FileID;
      files.set(id, { id, name, mime, bytes: data.length, state: "completed", data });
      captures.push(id);
      // Keep memory bounded: the oldest captures go first.
      while (captures.length > MAX_CAPTURES) files.delete(captures.shift()!);
      return id;
    },
    get: (id) => {
      const entry = files.get(id);
      return entry && meta(entry);
    },
    list: () => [...files.values()].map(meta),
    async transfer(id): Promise<WireFile | FileRef> {
      const entry = files.get(id);
      if (!entry || entry.state !== "completed")
        throw new Error("File is not available. Call browser_files_list for this tab and use a completed file ID.");
      if (entry.data) return { id, name: entry.name, mime: entry.mime, data: base64(entry.data) };
      return { id, name: entry.name, mime: entry.mime, bytes: entry.bytes, path: entry.path! };
    },
    download(id, name) {
      files.set(id, { id, name, mime: "application/octet-stream", bytes: 0, state: "pending" });
    },
    finish(id, result) {
      const entry = files.get(id);
      if (!entry) return;
      if ("failed" in result) entry.state = "failed";
      else Object.assign(entry, { state: "completed", path: result.path, bytes: result.bytes, mime: result.mime || entry.mime });
    },
    async dispose() {
      files.clear();
    },
  };
}

function base64(bytes: Uint8Array) {
  let binary = "";
  for (let index = 0; index < bytes.length; index += 0x8000)
    binary += String.fromCharCode(...bytes.subarray(index, index + 0x8000));
  return btoa(binary);
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

export function createFileInput(transport: ChromeTransport, uploadsRoot: () => string | null): FileInput {
  const paths = (files: (WireFile | FileRef)[]) =>
    files.map((file) => {
      if (!("path" in file) || !underRoot(file.path, uploadsRoot()))
        throw new Error("Chrome can upload only files Haarness staged for this command. Try the upload again.");
      return file.path;
    });
  return {
    async upload(target, files) {
      await transport.send("DOM.setFileInputFiles", { files: paths(files), backendNodeId: target.backendNodeId }, target.sessionID);
    },
    async drop(target, files) {
      const data = { items: [], files: paths(files), dragOperationsMask: 1 };
      for (const type of ["dragEnter", "dragOver", "drop"] as const)
        await transport.send("Input.dispatchDragEvent", { type, x: target.x, y: target.y, data }, target.sessionID);
    },
  };
}
