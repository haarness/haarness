/**
 * The browser tool wire types the extension reads and writes, as plain
 * TypeScript shapes.
 *
 * They mirror `Browser.*` in the shared driver package (`rpc.ts`, plus the
 * FileRef/HostCommand/HostResult additions of the Chrome contract). Until the
 * driver is wired in, the extension validates the few fields it acts on itself
 * (see `protocol.ts`); afterwards these become re-exports of the driver's types
 * and its codecs decode commands, so no `Schema` from the driver ever reaches
 * this package's own `effect` (see tests/driver-boundary.test.ts).
 */

export type TabID = `tab_${string}`;
export type FileID = `file_${string}`;
export type BrowserName = "haarness" | "chrome";
export type Decision = "allow" | "ask" | "deny";

export const TAB_ID = /^tab_[a-f0-9-]{36}$/;
export const FILE_ID = /^file_[a-f0-9-]{36}$/;
/** The server refuses longer URLs in a published state. */
export const MAX_URL = 16_384;
export const MAX_TITLE = 2_048;
/** Tabs one chat may hold in one browser, as on Desktop. */
export const MAX_TABS = 32;
/** Chats one browser may publish to one server. */
export const MAX_THREADS = 256;

export interface Tab {
  id: TabID;
  url: string;
  title: string;
  loading: boolean;
  canGoBack: boolean;
  canGoForward: boolean;
  generation: number;
  browser?: BrowserName;
}

export interface State {
  tabs: Tab[];
  focusedTabID: TabID | null;
}

export interface Target {
  resources: string[];
  key: string;
}

export interface Access {
  browsing: Decision;
  downloads: Decision;
  uploads: Decision;
}

export interface Policy {
  enabled: boolean;
  rememberApprovals?: boolean;
  defaults: Access;
  sites: (Access & { origin: string })[];
}

/** Every browser tool operation (`browser_<name>` with dots as underscores). */
export const methods = [
  "tabs.list", "tabs.open", "tabs.focus", "tabs.close",
  "navigate", "back", "forward", "reload", "stop",
  "frames", "snapshot", "find", "evaluate",
  "click", "hover", "drag", "fill", "fill_form", "select", "check", "press", "scroll", "wait",
  "screenshot", "dialog",
  "files.upload", "files.drop", "files.list", "files.get",
  "console", "network.list", "network.get",
  "trace.start", "trace.stop", "trace.analyze",
  "cpu.start", "cpu.stop", "cpu.analyze",
  "heap.snapshot", "heap.summary", "heap.query", "heap.object", "heap.compare",
  "lighthouse",
] as const;
export type Method = (typeof methods)[number];

/** An action as the server sends it; fields beyond `type` depend on the method. */
export interface Action {
  type: Method;
  tabID?: TabID;
  url?: string;
  focus?: boolean;
  [field: string]: unknown;
}

/** A file whose bytes travel inline, base64 on the wire. */
export interface File {
  id: FileID;
  name: string;
  mime: string;
  data: string;
}

/** A file the bridge staged on disk (uploads) or will read (downloads). */
export interface FileRef {
  id: FileID;
  name: string;
  mime: string;
  bytes: number;
  path: string;
}

/** `Browser.HostCommand`: the server's command with uploads staged by the bridge. */
export interface HostCommand {
  policy?: Policy;
  action: Action;
  generation?: number;
  files: (File | FileRef)[];
  inspect?: boolean;
  target?: Target;
}

/** `Browser.HostResult`. */
export interface HostResult {
  value: unknown;
  files: (File | FileRef)[];
}

export type Outcome =
  | { type: "success"; result: HostResult }
  | { type: "failure"; code: string; message: string };
