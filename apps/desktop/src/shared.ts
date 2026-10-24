import { defaultGlobalShortcut, type GlobalShortcut } from "./global-shortcut";
import type * as Types from "@haarness/sdk";
import type { ThemeId } from "./themes";
import type { Attachment } from "./attachments";
/** What the Screenshots dialog shows: a folder's newest images, without their paths. */
export type ScreenshotListing =
  | { canceled: true }
  | { needsFolder: true }
  | {
      folder: string;
      listing: string;
      items: { name: string; modified: number; thumbnail: string }[];
    };
export interface ProjectLocation {
  cloudWorkspace?: string;
  cloudSession?: string;
  cloudAccount?: string;
  cloudApiUrl?: string;
  remoteWorkspace?: string;
  /** Another of your computers, opened through Haarness Cloud's relay: its
   * Cloud host id. `remoteWorkspace` names the folder there. */
  remoteHost?: string;
  project?: { id: string; name: string; root: string; git: boolean };
  /** The chat home: the one folder on this computer that holds the chats
   * started without a project ("This computer"), each in a folder of its
   * own. It is never a project. */
  chatHome?: boolean;
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
  /** How the model's reasoning shows in the chat: one row that opens on
   * click (`compact`), in full, or not at all. */
  thinking: ThinkingDisplay;
  sendKey: "enter" | "mod-enter";
  editorWrap: boolean;
  diffLayout: "split" | "unified";
  terminalFontSize: number;
  /** Which Option keys send Meta (ESC and the key) in the terminal instead
   * of the character the layout composes; macOS only, off by default. */
  terminalOptionAsMeta: "off" | "left" | "both";
  shortcuts: Record<string, string>;
  /** Shows and hides Haarness from any app; off by default. */
  globalShortcut: GlobalShortcut;
  language: string;
}
export type ThinkingDisplay = "compact" | "show" | "hide";
/**
 * The thinking display saved preferences ask for. It used to be a "Show
 * reasoning" switch that was off by default and saved with every other
 * preference, so an off switch cannot be told from one never touched: on
 * becomes Show, anything else the Compact default.
 */
export function savedThinking(saved: unknown): ThinkingDisplay {
  const value = saved && typeof saved === "object" ? (saved as Record<string, unknown>) : {};
  if (value.thinking === "compact" || value.thinking === "show" || value.thinking === "hide")
    return value.thinking;
  return value.showReasoning === true ? "show" : "compact";
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
  thinking: "compact",
  sendKey: "enter",
  editorWrap: false,
  diffLayout: "split",
  terminalFontSize: 13,
  terminalOptionAsMeta: "off",
  shortcuts: {
    new: "Mod+N",
    settings: "Mod+,",
    palette: "Mod+K",
    files: "Mod+Shift+E",
    diff: "Mod+Shift+D",
    terminal: "Mod+J",
    browser: "Mod+Shift+B",
    // The Chromium DevTools key for inspecting an element.
    pick: "Mod+Shift+C",
    // ⌘J is the terminal; the side chat takes the key beside it.
    sideChat: "Mod+Shift+J",
    // Composer keys, matched while the message box has focus rather than by
    // the native menu. The TUI switches agents with Tab, but here Tab has to
    // keep moving keyboard focus, so the defaults are chords.
    agentNext: "Mod+.",
    agentPrev: "Mod+Shift+.",
    // Matched wherever the keyboard is: the tabs of the panel that has it.
    tabNext: "Ctrl+Tab",
    tabPrev: "Ctrl+Shift+Tab",
    // The chat rows the sidebar draws, from anywhere but a code editor or a
    // terminal. On a focused sidebar row Alt and an arrow still move the row.
    chatPrev: "Alt+ArrowUp",
    chatNext: "Alt+ArrowDown",
    // Control on macOS too; off macOS see `defaultShortcuts`.
    focusSidebar: "Ctrl+Shift+L",
    // The prompts of the open chat. In the message box they act only while
    // it is empty, so the caret keys keep working on a draft.
    promptPrev: "Mod+ArrowUp",
    promptNext: "Mod+ArrowDown",
    promptFirst: "Mod+Shift+ArrowUp",
    promptLast: "Mod+Shift+ArrowDown",
  },
  globalShortcut: defaultGlobalShortcut,
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
/**
 * The keys a shortcut stands for on `platform`, spelled one way: Mod is
 * Command on macOS and Control elsewhere, so off macOS `Mod+Tab` and
 * `Ctrl+Tab` are the same keys.
 */
export function shortcutKeys(value: string, platform: string) {
  const parts = value.toLowerCase().split("+").map((part) => part.trim()).filter(Boolean);
  const key = parts.pop() ?? "";
  const modifiers = parts.map((part) =>
    part === "mod" ? (platform === "darwin" ? "meta" : "ctrl") : part,
  );
  return [...new Set(modifiers)].sort().concat(key).join("+");
}
/** Whether two of `shortcuts` press the same keys; an unset one ("") is none. */
export function shortcutsClash(shortcuts: Record<string, string>, platform: string) {
  const keys = Object.values(shortcuts).filter(Boolean).map((value) => shortcutKeys(value, platform));
  return new Set(keys).size !== keys.length;
}
/**
 * The default keys on `platform`. Off macOS Control is Mod, and
 * Control+Shift+L is the file editor's "Add selection to context", so Focus
 * sidebar takes Alt+Shift+L there. (Control+Alt would be AltGr on many
 * keyboards, where Control+Alt+Shift+L types a letter such as Ł.)
 */
export function defaultShortcuts(platform: string): Preferences["shortcuts"] {
  return platform === "darwin"
    ? defaults.shortcuts
    : { ...defaults.shortcuts, focusSidebar: "Alt+Shift+L" };
}
/** Shortcuts that may be left unset: the ones added after people could
 * choose their own keys. */
export const optionalShortcuts = new Set([
  "pick",
  "sideChat",
  "tabNext",
  "tabPrev",
  "chatPrev",
  "chatNext",
  "focusSidebar",
  "promptPrev",
  "promptNext",
  "promptFirst",
  "promptLast",
]);
/**
 * The tab steps (Ctrl+Tab, Ctrl+Shift+Tab), and later the chat, sidebar,
 * prompt and pick keys, came after people chose their own keys. A new
 * default that is already a key the person has (another action's key in
 * `saved`, such as a `Mod+Tab` agent key off macOS or a chat key moved to
 * `Mod+Shift+C`, or their `global` shortcut) is left unset, so they keep it
 * and Settings still saves; a key saved before is kept as it is.
 */
export function migrateTabKeys(
  saved: Record<string, string> | undefined,
  shortcuts: Preferences["shortcuts"],
  platform: string,
  global?: string,
) {
  let next = shortcuts;
  for (const name of optionalShortcuts) {
    if (!next[name] || (saved && name in saved)) continue;
    const mine = shortcutKeys(next[name], platform);
    const theirs = (other: string) => !optionalShortcuts.has(other) || (!!saved && other in saved);
    const taken =
      Object.entries(next).some(
        ([other, value]) => other !== name && theirs(other) && value && shortcutKeys(value, platform) === mine,
      ) || (!!global && shortcutKeys(global, platform) === mine);
    if (taken) next = { ...next, [name]: "" };
  }
  return next;
}
/** Optional shortcuts that are native menu accelerators, not keys the page matches. */
export const menuShortcuts = new Set(["pick"]);
// Menu shortcuts become native accelerators, so they need a modifier and a
// single character. Composer, tab, chat and prompt shortcuts are matched in
// the page and may also use a named key, such as Tab for people who want
// the TUI's, or the arrows the page reports as ArrowUp and so on.
const KEY = "[a-z0-9,.;/\\[\\]\\\\\\-=`]|F(?:[1-9]|1[0-9]|2[0-4])";
const NAMED = `${KEY}|Tab|Enter|Space|Escape|Backspace|Up|Down|Left|Right|ArrowUp|ArrowDown|ArrowLeft|ArrowRight|Home|End|PageUp|PageDown`;
const MODIFIER = "Mod|Ctrl|Alt|Shift|Meta";
const accelerator = new RegExp(`^(${MODIFIER})(\\+(${MODIFIER}))*\\+(${KEY})$`, "i");
const composer = new RegExp(`^((${MODIFIER})\\+)*(${NAMED})$`, "i");
/**
 * Whether Settings may save `value` for the shortcut `name`: an optional one
 * may be unset (""), the keys the page matches may be a named key, and the
 * rest, menu accelerators (Pick an element included), need a modifier.
 */
export function shortcutAccepted(name: string, value: string) {
  if (value === "" && optionalShortcuts.has(name)) return true;
  const page = (name === "agentNext" || name === "agentPrev" || optionalShortcuts.has(name)) && !menuShortcuts.has(name);
  return (page ? composer : accelerator).test(value);
}
export interface SavedServer {
  id: string;
  label: string;
  url?: string;
  /** Haarness Cloud sessions of one account, derived from saved locations. */
  cloud?: boolean;
  /** Another of your computers, opened through Haarness Cloud: its host id. */
  host?: string;
}
/** One of your other computers, as the account lists it (`haarness remote enable`). */
export interface Computer {
  id: string;
  name: string;
  connected: boolean;
  lastSeenAt?: string | null;
  platform?: string | null;
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
    | "projects"
    | "btw";
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
  /** Where a dropped or pasted file is on this computer, or "" for one
   * that has no path. */
  pathForFile(file: File): string;
  /** Holds the global shortcut off while Settings records a new one. */
  globalShortcut(action: "suspend" | "resume"): Promise<void>;
  account(input: { action: string; id?: string; name?: string; repo?: string; configuration?: Record<string, unknown>; page?: string; device?: boolean }): Promise<any>;
  openLink(connection: string | undefined, thread: string | undefined | null, url: string): Promise<"external" | "browser">;
  /** An image on another host as a data URL, fetched by the app without cookies or a referrer; throws why it did not load. */
  remoteImage(url: string): Promise<string>;
  browserData(connection: string, input?: { action: "get" | "remove" | "clear" | "save"; kind?: "history" | "downloads" | "all"; id?: string }): Promise<import("./browser-settings").BrowserData>;
  browserAttach(connection:string,thread:string):Promise<BrowserState>;
  browserAdopt(connection:string,thread:string):Promise<void>;
  browserCommand(connection:string,thread:string,action:unknown):Promise<unknown>;
  browserLayout(connection:string,thread:string,layout:BrowserLayout):Promise<void>;
  /** A JPEG data URL of the focused page, or null when nothing is drawn. */
  browserSnapshot(connection:string,thread:string):Promise<string | null>;
  /** Starts picking an element in the page the window shows for this chat, or stops (null). */
  browserPick(connection: string, thread: string, request: import("./browser-pick").PickRequest | null): Promise<{ tab: string } | null>;

  notificationReady(): Promise<NotificationTarget | null>;
  notificationHandled(target: NotificationTarget): Promise<void>;
  /** The projects another computer offers (`server` is its `host:` id). */
  remoteCatalog(server: string): Promise<Types.WorkspaceListResponse>;
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
    /** This computer's home folder. */
    home: string;
    /** The chat home's folder (see `ProjectLocation.chatHome`), made or not. */
    chatHome: string;
    updateConfigured: boolean;
  }>;
  connect(input: {
    cloudWorkspace?: string;
    cloudSession?: string;
    /** Another of your computers (its Cloud host id), with `remoteWorkspace`
     * from its catalog or `workspace`, a folder to open there. */
    remoteHost?: string;
    remoteWorkspace?: string;
    workspace?: string;
    url?: string;
    token?: string;
    username?: string;
    password?: string;
    label?: string;
    trust?: boolean;
    recent?: string;
    /** From a haarness:// link: an unsaved folder is confirmed and opens untrusted. */
    deepLink?: boolean;
    /** The chat home ("This computer"), created when it is missing. */
    chatHome?: boolean;
  }): Promise<Connection | null>;
  voice(
    action: "start" | "stop" | "cancel",
    id: string,
    locale?: string,
  ): Promise<void>;
  forget(id: string): Promise<void>;
  server(input: {
    id: string;
    /** `host` saves another computer (`id` is its Cloud host id) as a
     * server to switch to, or renames it; `shown` says which server the
     * window shows now. */
    action: "rename" | "remove" | "host" | "shown";
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
  /** Asks a side question of `thread`. Its answer arrives as `btw` events
   * with `id` (`delta`, `text`), and the call answers with how it ended
   * (`done` or `error`; nothing when the stream broke off); `cancel(id)`
   * stops it. */
  btw(
    connection: string,
    thread: string,
    body: { question: string; history?: { question: string; answer: string }[] },
    id: string,
  ): Promise<
    | { type: "done"; answer: string; inputTokens: number; outputTokens: number }
    | { type: "error"; code: string; message: string; retryable: boolean }
    | undefined
  >;
  /** The keyboard came into (or left) the side chat, where ⌘W closes the
   * thread in front instead of the window. */
  sideChatFocus(focused: boolean): Promise<void>;
  /** A chat's stored side chats (`key` is its project and chat), or with
   * `value` saves them; null forgets them. Kept in files of their own. */
  sideChats(key: string, value?: unknown): Promise<unknown>;
  preferences(input: Partial<Preferences>): Promise<Preferences>;
  storage(key: string, value?: unknown): Promise<unknown>;
  review(input: import("./review-drafts").ReviewRequest): Promise<import("./review-drafts").ReviewResponse>;
  attachmentDraft(key: string, value?: Attachment[]): Promise<Attachment[]>;
  attachments(): Promise<Attachment[]>;
  /** The composer's Attach menu opened: a token for listing screenshots from it, or null. */
  screenshotMenu(): Promise<string | null>;
  /** The newest images of the screenshots folder (`token` from the Attach menu), or another folder the person chooses. */
  screenshots(input?: { choose?: boolean; token?: string }): Promise<ScreenshotListing>;
  /** Reads the screenshots at these places in the window's last listing, in this order. */
  screenshotAttach(listing: string, indexes: number[]): Promise<Attachment[]>;
  save(name: string, content: string): Promise<boolean>;
  openArchive(): Promise<unknown | null>;
  pick(kind: "file" | "directory"): Promise<string | null>;
  openExternal(url: string): Promise<void>;
  /** Runs `haarness browser setup|status --json` on this computer. */
  chromeSetup(action: "setup" | "status"): Promise<import("./chrome").ChromeSetupInfo>;
  /** Shows the unpacked Haarness for Chrome folder the last report named. */
  chromeReveal(): Promise<void>;
  /** The raised log level on this computer, or Detailed logging turned on or off. */
  logLevel(action: "status" | "detail-on" | "detail-off"): Promise<import("./diagnostics").LogLevelStatus>;
  /** Saves this computer's logs where the person chooses and shows the zip; null when cancelled. */
  exportLogs(): Promise<import("./diagnostics").ExportedLogs | null>;
  reveal(connection: string, path?: string): Promise<void>;
  /** `write` (the person's keys) and `answer` (xterm answering a program's
   * query, which only one window does per terminal) answer false when the
   * terminal has no connection now. */
  terminal(
    connection: string,
    action: "connect" | "write" | "answer" | "detach",
    id: string,
    input?: unknown,
  ): Promise<unknown>;
  update(action: "check" | "download" | "install"): Promise<unknown>;
  onEvent(callback: (event: DesktopEvent) => void): () => void;
}
declare global {
  interface Window {
    haarness: Bridge;
  }
}
