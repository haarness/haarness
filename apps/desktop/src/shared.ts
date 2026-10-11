import type * as Types from "@haarness/sdk";
import type { ThemeId } from "./themes";
import type { Attachment } from "./attachments";
export interface ProjectLocation {
  cloudWorkspace?: string;
  cloudSession?: string;
  cloudAccount?: string;
  cloudApiUrl?: string;
  remoteWorkspace?: string;
  project?: { id: string; name: string; root: string; git: boolean };
  checkout?: Pick<
    Types.WorkspaceInfo,
    "id" | "name" | "kind" | "branch" | "managed" | "exists"
  >;
}
export interface Connection extends ProjectLocation {
  id: string;
  /** Remote host origin, distinct from this project's routed endpoint. */
  url?: string;
  label: string;
  workspace: string;
  remote: boolean;
  trust: boolean;
  info: Types.ServerInfo;
  /** Existing work uses a compatible earlier runtime until an idle reconnect. */
  upgradePending?: boolean;
}
export interface Preferences {
  theme: "dark" | "light" | "system";
  palette: ThemeId;
  fontSize: number;
  notifications: boolean;
  completionNotifications: "background" | "always" | "never";
  permissionNotifications: boolean;
  questionNotifications: boolean;
  browserWebLinks: "external" | "browser";
  browserLocalLinks: "external" | "browser";
  browserShowFullURL: boolean;
  browserAskDownloadLocation: boolean;
  /** Draw the agent's cursor over the page while it controls the browser. */
  browserAgentCursor: boolean;
  sound: boolean;
  compactTools: boolean;
  showReasoning: boolean;
  sendKey: "enter" | "mod-enter";
  editorWrap: boolean;
  diffLayout: "split" | "unified";
  terminalFontSize: number;
  shortcuts: Record<string, string>;
  language: string;
}
export const defaults: Preferences = {
  // A new install follows the system until someone picks Light or Dark.
  theme: "system",
  palette: "haarness",
  fontSize: 14,
  notifications: true,
  completionNotifications: "background",
  permissionNotifications: true,
  questionNotifications: true,
  browserWebLinks: "external",
  browserLocalLinks: "browser",
  browserShowFullURL: true,
  browserAskDownloadLocation: false,
  browserAgentCursor: true,
  sound: false,
  compactTools: true,
  showReasoning: false,
  sendKey: "enter",
  editorWrap: false,
  diffLayout: "split",
  terminalFontSize: 13,
  shortcuts: {
    new: "Mod+N",
    settings: "Mod+,",
    palette: "Mod+K",
    files: "Mod+Shift+E",
    diff: "Mod+Shift+D",
    terminal: "Mod+J",
    browser: "Mod+Shift+B",
    // Composer keys, matched while the message box has focus rather than by
    // the native menu. The TUI switches agents with Tab, but here Tab has to
    // keep moving keyboard focus, so the defaults are chords.
    agentNext: "Mod+.",
    agentPrev: "Mod+Shift+.",
  },
  language: "en",
};
/**
 * Tab and Shift+Tab used to switch agents from anywhere in the window, so
 * keyboard users could not move focus. Saved preferences still hold those old
 * defaults; move them to the new chords, unless another shortcut took one.
 */
export function migrateAgentKeys(shortcuts: Preferences["shortcuts"]) {
  const next = { ...shortcuts };
  const taken = (value: string) =>
    Object.values(next).some((other) => other.toLowerCase() === value.toLowerCase());
  for (const [name, old] of [["agentNext", "Tab"], ["agentPrev", "Shift+Tab"]] as const)
    if (next[name] === old && !taken(defaults.shortcuts[name]))
      next[name] = defaults.shortcuts[name];
  return next.agentNext === shortcuts.agentNext && next.agentPrev === shortcuts.agentPrev
    ? shortcuts
    : next;
}
export interface SavedServer {
  id: string;
  label: string;
  url?: string;
  /** Haarness Cloud sessions of one account, derived from saved locations. */
  cloud?: boolean;
}
export interface Recent extends ProjectLocation {
  id: string;
  label: string;
  workspace: string;
  remote: boolean;
  url?: string;
  trust: boolean;
}
export interface FileEntry {
  name: string;
  path: string;
  directory: boolean;
  symlink: boolean;
  size: number;
}
export interface FileContent {
  path: string;
  content: string;
  encoding: string;
  mime: string;
  revision: string;
  size: number;
}
export type VoiceEvent =
  | { type: "preparing"; message: string }
  | { type: "started" }
  | { type: "transcript"; text: string; isFinal: boolean }
  | { type: "ended" }
  | { type: "error"; message: string };
export interface NotificationTarget {
  connection: string;
  thread: string;
}
export interface DesktopEvent {
  kind:
    | "browser"
    | "notification-open"
    | "voice"
    | "server"
    | "connection"
    | "pty"
    | "command"
    | "menu"
    | "update"
    | "projects";
  connection?: string;
  id?: string;
  data: unknown;
}
export interface DirectoryListing {
  path: string;
  parent: string | null;
  directories: { name: string; path: string }[];
  nextCursor?: string | null;
}
export interface BrowserLayout { visible:boolean; bounds:{x:number;y:number;width:number;height:number} }
export interface BrowserTab { id:string; url:string; title:string; loading:boolean; canGoBack:boolean; canGoForward:boolean; generation:number; /** Desktop-only: the site icon as a small data URL. */ favicon?: string }
export interface BrowserState { tabs:BrowserTab[]; focusedTabID:string|null }
export interface Bridge {
  account(input: { action: string; id?: string; name?: string; repo?: string; configuration?: Record<string, unknown>; page?: string; device?: boolean }): Promise<any>;
  openLink(connection: string | undefined, thread: string | undefined | null, url: string): Promise<"external" | "browser">;
  browserData(connection: string, input?: { action: "get" | "remove" | "clear" | "save"; kind?: "history" | "downloads" | "all"; id?: string }): Promise<import("./browser-settings").BrowserData>;
  browserAttach(connection:string,thread:string):Promise<BrowserState>;
  browserAdopt(connection:string,thread:string):Promise<void>;
  browserCommand(connection:string,thread:string,action:unknown):Promise<unknown>;
  browserLayout(connection:string,thread:string,layout:BrowserLayout):Promise<void>;
  /** A JPEG data URL of the focused page, or null when nothing is drawn. */
  browserSnapshot(connection:string,thread:string):Promise<string | null>;

  notificationReady(): Promise<NotificationTarget | null>;
  notificationHandled(target: NotificationTarget): Promise<void>;
  remoteProjects(
    server: string,
    path?: string,
    options?: {
      prefix?: string;
      hidden?: boolean;
      limit?: number;
      after?: string;
    },
  ): Promise<{
    listing: DirectoryListing;
  }>;
  windowAction(action: string): Promise<void>;
  boot(): Promise<{
    preferences: Preferences;
    recent: Recent[];
    servers: SavedServer[];
    version: string;
    platform: string;
    updateConfigured: boolean;
  }>;
  connect(input: {
    cloudWorkspace?: string;
    cloudSession?: string;
    workspace?: string;
    url?: string;
    token?: string;
    username?: string;
    password?: string;
    label?: string;
    trust?: boolean;
    recent?: string;
  }): Promise<Connection | null>;
  voice(
    action: "start" | "stop" | "cancel",
    id: string,
    locale?: string,
  ): Promise<void>;
  forget(id: string): Promise<void>;
  server(input: {
    id: string;
    action: "rename" | "remove";
    label?: string;
  }): Promise<SavedServer[]>;
  projects(): Promise<Recent[]>;
  api<T = unknown>(
    connection: string,
    method: string,
    args?: unknown[],
  ): Promise<T>;
  files<T = unknown>(
    connection: string,
    action: string,
    input: Record<string, unknown>,
  ): Promise<T>;
  subscribe(connection: string, cursor: number): Promise<string>;
  unsubscribe(id: string): Promise<void>;
  native<T = unknown>(
    connection: string,
    input: Record<string, unknown>,
  ): Promise<T>;
  command(
    connection: string,
    args: string[],
    id: string,
  ): Promise<{ output: string; value: unknown }>;
  cancel(id: string): Promise<void>;
  preferences(input: Partial<Preferences>): Promise<Preferences>;
  storage(key: string, value?: unknown): Promise<unknown>;
  review(input: import("./review-drafts").ReviewRequest): Promise<import("./review-drafts").ReviewResponse>;
  attachmentDraft(key: string, value?: Attachment[]): Promise<Attachment[]>;
  attachments(): Promise<Attachment[]>;
  save(name: string, content: string): Promise<boolean>;
  openArchive(): Promise<unknown | null>;
  pick(kind: "file" | "directory"): Promise<string | null>;
  openExternal(url: string): Promise<void>;
  /** Runs `haarness browser setup|status --json` on this computer. */
  chromeSetup(action: "setup" | "status"): Promise<import("./chrome").ChromeSetupInfo>;
  /** Shows the unpacked Haarness for Chrome folder the last report named. */
  chromeReveal(): Promise<void>;
  reveal(connection: string, path?: string): Promise<void>;
  terminal(
    connection: string,
    action: "connect" | "write" | "detach",
    id: string,
    input?: unknown,
  ): Promise<void>;
  update(action: "check" | "download" | "install"): Promise<unknown>;
  onEvent(callback: (event: DesktopEvent) => void): () => void;
}
declare global {
  interface Window {
    haarness: Bridge;
  }
}
