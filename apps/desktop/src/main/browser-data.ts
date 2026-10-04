import { app, dialog, type BrowserWindow } from "electron";
import { copyFile, mkdir, rm } from "node:fs/promises";
import { join } from "node:path";
import { randomUUID } from "node:crypto";
import { data, persist } from "./store";
import { translate } from "../localization";
import type { BrowserData, BrowserDownload } from "../browser-settings";
const key = (connection: string) => "browser-data:" + connection;
export function browserData(connection: string): BrowserData {
  return (data().values[key(connection)] ??= {
    history: [],
    downloads: [],
  }) as BrowserData;
}
const downloadPath = (id: string) => {
  if (!/^file_[a-f0-9-]{36}$/.test(id))
    throw new Error("Invalid download identifier");
  return join(app.getPath("userData"), "browser-downloads", id);
};
export function recordBrowserVisit(
  connection: string,
  thread: string,
  url: string,
  title: string,
) {
  if (!/^https?:\/\//i.test(url)) return;
  const record = {
    id: randomUUID(),
    thread,
    url,
    title,
    time: new Date().toISOString(),
  };
  const value = browserData(connection);
  value.history = [record, ...value.history].slice(0, 1000);
  void persist();
}
export async function recordBrowserDownload(
  win: BrowserWindow,
  connection: string,
  file: BrowserDownload,
  source: string,
  valid: () => boolean,
) {
  if (!valid()) return;
  if (file.state === "completed") {
    await mkdir(join(app.getPath("userData"), "browser-downloads"), {
      recursive: true,
      mode: 0o700,
    });
    await copyFile(source, downloadPath(file.id));
  }
  if (!valid()) {
    await rm(downloadPath(file.id), { force: true });
    return;
  }
  const value = browserData(connection);
  value.downloads.unshift(file);
  for (const old of value.downloads.splice(200))
    await rm(downloadPath(old.id), { force: true });
  await persist();
  if (
    file.state === "completed" &&
    data().preferences.browserAskDownloadLocation &&
    valid()
  )
    await saveBrowserDownload(win, connection, file.id);
}
export async function saveBrowserDownload(
  win: BrowserWindow,
  connection: string,
  id: string,
) {
  const file = browserData(connection).downloads.find(
    (entry) => entry.id === id,
  );
  if (!file || file.state !== "completed")
    throw new Error("This download is no longer available");
  const selected = await dialog.showSaveDialog(win, {
    title: translate(data().preferences.language, "Save browser download"),
    defaultPath: join(
      app.getPath("downloads"),
      file.name.replace(/[\\/]/g, "_"),
    ),
  });
  if (!selected.canceled && selected.filePath)
    await copyFile(downloadPath(id), selected.filePath);
}
export async function removeBrowserData(
  connection: string,
  kind: "history" | "downloads" | "all",
  id?: string,
) {
  const value = browserData(connection);
  if (kind !== "downloads")
    value.history = id ? value.history.filter((entry) => entry.id !== id) : [];
  if (kind !== "history") {
    const removed = value.downloads.filter((entry) => !id || entry.id === id);
    value.downloads = id
      ? value.downloads.filter((entry) => entry.id !== id)
      : [];
    for (const entry of removed)
      await rm(downloadPath(entry.id), { force: true });
  }
  await persist();
}
