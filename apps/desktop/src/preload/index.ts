import { contextBridge, ipcRenderer, webUtils } from "electron";
import type { Bridge, DesktopEvent } from "../shared";
const invoke = async (channel: string, ...args: unknown[]) => {
  const response = await ipcRenderer.invoke("haarness:" + channel, ...args);
  if (!response.ok) throw new Error(response.error);
  return response.value;
};
const bridge: Bridge = {
  // Never throws: a file the page made itself has no path.
  pathForFile: (file) => {
    try {
      return webUtils.getPathForFile(file);
    } catch {
      return "";
    }
  },
  account: (input) => invoke("account", input),
  globalShortcut: (action) => invoke("globalShortcut", action),
  openLink: (connection, thread, url) => invoke("openLink", connection, thread, url),
  browserData: (connection, input) => invoke("browserData", connection, input),
  browserAttach:(connection,thread)=>invoke("browserAttach",connection,thread),
  browserAdopt:(connection,thread)=>invoke("browserAdopt",connection,thread),
  browserCommand:(connection,thread,action)=>invoke("browserCommand",connection,thread,action),
  browserLayout:(connection,thread,layout)=>invoke("browserLayout",connection,thread,layout),
  browserSnapshot:(connection,thread)=>invoke("browserSnapshot",connection,thread),
  notificationReady: () => invoke("notificationReady"),
  notificationHandled: (target) => invoke("notificationHandled", target),
  windowAction: (action) => invoke("windowAction", action),
  boot: () => invoke("boot"),
  connect: (input) => invoke("connect", input),
  voice: (action, id, locale) => invoke("voice", action, id, locale),
  forget: (id) => invoke("forget", id),
  server: (input) => invoke("server", input),
  projects: () => invoke("projects"),
  remoteProjects: (server, path, options) =>
    invoke("remoteProjects", server, path, options),
  api: (connection, method, args = []) =>
    invoke("api", { connection, method, args }),
  files: (connection, action, input) =>
    invoke("files", connection, action, input),
  subscribe: (connection, cursor) => invoke("subscribe", connection, cursor),
  unsubscribe: (id) => invoke("unsubscribe", id),
  native: (connection, input) => invoke("native", connection, input),
  command: (connection, args, id) => invoke("command", connection, args, id),
  cancel: (id) => invoke("cancel", id),
  preferences: (input) => invoke("preferences", input),
  review: (input) => invoke("review", input),
  storage: (key, value) => invoke("storage", key, value),
  attachmentDraft: (key, value) => invoke("attachmentDraft", key, value),
  attachments: () => invoke("attachments"),
  screenshotMenu: () => invoke("screenshotMenu"),
  screenshots: (input) => invoke("screenshots", input),
  screenshotAttach: (listing, indexes) => invoke("screenshotAttach", listing, indexes),
  save: (name, content) => invoke("save", name, content),
  openArchive: () => invoke("openArchive"),
  pick: (kind) => invoke("pick", kind),
  openExternal: (url) => invoke("external", url),
  chromeSetup: (action) => invoke("chromeSetup", action),
  chromeReveal: () => invoke("chromeReveal"),
  reveal: (connection, path) => invoke("reveal", connection, path),
  terminal: (connection, action, id, input) =>
    invoke("terminal", connection, action, id, input),
  update: (action) => invoke("update", action),
  onEvent: (callback) => {
    const listener = (_: unknown, event: DesktopEvent) => callback(event);
    ipcRenderer.on("haarness:event", listener);
    return () => ipcRenderer.removeListener("haarness:event", listener);
  },
};
contextBridge.exposeInMainWorld("haarness", bridge);
