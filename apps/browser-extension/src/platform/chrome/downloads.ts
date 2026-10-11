/**
 * Chrome's side of download attribution. A page under an agent command
 * reports `Page.downloadWillBegin` (see session.ts); when Chrome's download
 * item for that URL appears within 5 s it becomes a file of that tab, and when
 * it completes the bridge is told the path it may read. Every other download is
 * the user's and is ignored.
 */
import type { FileID } from "../../core/browser";
import type { createAttribution } from "../../core/downloads";
import type { ChromeFileStore } from "./files";

export interface Finished {
  server: string;
  thread: string;
  command: string;
  path: string;
  url: string;
}

export function createDownloads(deps: {
  attribution: ReturnType<typeof createAttribution>;
  store(tabId: number): ChromeFileStore;
  existing(tabId: number): ChromeFileStore | undefined;
  uuid(): string;
}) {
  const owned = new Map<number, { tabId: number; file: FileID; server: string; thread: string; command: string; url: string }>();
  return {
    created(item: chrome.downloads.DownloadItem) {
      const match = deps.attribution.claim({ id: item.id, url: item.url, finalUrl: item.finalUrl, startedAt: Date.parse(item.startTime) });
      if (!match) return;
      const file = `file_${deps.uuid()}` as FileID;
      deps.store(match.tabId).download(file, item.filename.split(/[\\/]/).pop() || match.name || "download");
      owned.set(item.id, { tabId: match.tabId, file, server: match.server, thread: match.thread, command: match.command, url: match.url });
    },
    async changed(delta: chrome.downloads.DownloadDelta, finished: (download: Finished) => void) {
      const entry = owned.get(delta.id);
      const state = delta.state?.current;
      if (!entry || (state !== "complete" && state !== "interrupted")) return;
      owned.delete(delta.id);
      const [item] = await chrome.downloads.search({ id: delta.id });
      const store = deps.existing(entry.tabId);
      if (state !== "complete" || !item?.filename) return store?.finish(entry.file, { failed: true });
      store?.finish(entry.file, { path: item.filename, bytes: item.fileSize, mime: item.mime });
      finished({ server: entry.server, thread: entry.thread, command: entry.command, path: item.filename, url: entry.url });
    },
  };
}
