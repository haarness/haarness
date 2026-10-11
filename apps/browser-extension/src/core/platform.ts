/**
 * What the extension core needs from a browser. `platform/chrome` implements it
 * with `chrome.*`; another browser (Firefox, Safari) would implement the same
 * interface. Nothing in `src/core` touches a browser API directly.
 */
import type { FetchContext } from "./fetch-policy";
import type { CdpTransport, CreatePage, Page, PageOptions } from "./driver-contract";
import type { TabID } from "./browser";

/** A browser tab as the core reads it. */
export interface LiveTab {
  id: number;
  windowId: number;
  /** -1 when the tab is in no group. */
  groupId: number;
  /** The committed URL, or the pending one while the first load runs. */
  url: string;
  title: string;
  loading: boolean;
  active: boolean;
  openerTabId?: number;
}

export interface TabChange {
  url?: string;
  groupId?: number;
  title?: string;
  status?: string;
}

/** Browser events the core reacts to. Platforms register their listeners synchronously at start. */
export interface PlatformEvents {
  tabCreated(tab: LiveTab): void;
  tabUpdated(tabId: number, change: TabChange, tab: LiveTab): void;
  tabRemoved(tabId: number): void;
  tabReplaced(added: number, removed: number): void;
  windowFocused(windowId: number | undefined): void;
  /** The debugger left a tab. `canceled_by_user` means the user dismissed Chrome's debugging bar. */
  debuggerDetached(tabId: number, reason: string): void;
  /** A download an agent started finished (`complete`) or failed. */
  downloadFinished(download: { thread: string; server: string; command: string; path: string; url: string }): void;
  /** The user pressed the "Stop Haarness in this browser" shortcut. */
  stopAll(): void;
  /** A wake-up alarm fired (reconnect). */
  alarm(name: string): void;
}

export interface TabsPort {
  get(tabId: number): Promise<LiveTab | undefined>;
  /** Opens a tab in the window the user used last, in the background unless `active`. */
  create(options: { url: string; active: boolean }): Promise<LiveTab>;
  remove(tabIds: number[]): Promise<void>;
  /** Selects the tab in its own window. Never raises or focuses a window. */
  activate(tabId: number): Promise<void>;
  /** Raises a window: only for a user's own action (the popup's "Show tabs"). */
  raiseWindow(windowId: number): Promise<void>;
  focusedWindow(): Promise<number | undefined>;
  activeTab(windowId: number): Promise<number | undefined>;
}

/** Tab groups, when the browser has them. */
export interface GroupsPort {
  /** Puts tabs into `groupId`, or into a new group; returns the group. */
  add(tabIds: number[], groupId?: number): Promise<number>;
  ungroup(tabIds: number[]): Promise<void>;
  update(groupId: number, properties: { title?: string; color?: string; collapsed?: boolean }): Promise<void>;
  exists(groupId: number): Promise<boolean>;
  tabs(groupId: number): Promise<LiveTab[]>;
}

/** A debugger session on one tab. */
export interface TabSession {
  readonly cdp: CdpTransport;
  /**
   * Runs `work` with the chat's site rules enforced inside the page, then
   * checks where the tab landed. A blocked page of the tab's own, or a
   * landing on a site the chat may not use, fails with `site-blocked`; a
   * blocked frame or resource is only not loaded.
   */
  guard<T>(context: FetchContext, signal: AbortSignal, work: () => Promise<T>): Promise<T>;
  /** Loads a URL and waits for its document (used by `tabs.open`). */
  load(url: string, signal: AbortSignal): Promise<void>;
  /**
   * The driver page for this tab, created with the driver on first use. The
   * session tells the page about main-frame navigations itself.
   */
  page(create: CreatePage, options: Omit<PageOptions, "files" | "input">): Page;
  /** A command starts: downloads it begins are attributed to it. */
  begin(command: { server: string; thread: string; command: string }): void;
  /** The command finished: undo what it borrowed (a tab shown for a capture). */
  end(): Promise<void>;
}

export interface DebuggerPort {
  session(tabId: number, id: TabID): Promise<TabSession>;
  attached(tabId: number): boolean;
  detach(tabId: number): Promise<void>;
  detachAll(): Promise<void>;
}

export interface KeyValue {
  get<T>(key: string): Promise<T | undefined>;
  set(key: string, value: unknown): Promise<void>;
}

export interface NativePort {
  post(message: unknown): void;
  close(): void;
}

export interface Platform {
  tabs: TabsPort;
  /** Absent when the browser has no tab groups; tabs are then tracked ungrouped. */
  groups?: GroupsPort;
  debugger: DebuggerPort;
  /** Lost when the browser quits. */
  session: KeyValue;
  /** Kept across restarts. */
  local: KeyValue;
  connectNative(
    name: string,
    handlers: { message(message: unknown): void; disconnect(error: string | undefined): void },
  ): NativePort;
  /** Badge text and its meaning ("waiting" asks for attention). */
  badge(text: string, kind: "waiting" | "working" | "idle"): void;
  title(text: string): void;
  alarm(name: string, minutes: number | null): void;
  /** Restarts the extension (to pick up an updated copy on disk). */
  reload(): void;
  /** The extension's own version string (`version_name`). */
  version(): string;
  browser(): Promise<{ name: string; version: string }>;
  /** The page-safe uploads root the bridge announced, for upload paths. */
  setUploadsRoot(root: string | null): void;
  /** Whether to draw the agent's cursor. */
  setCursor(enabled: boolean): void;
  now(): number;
  uuid(): string;
}
