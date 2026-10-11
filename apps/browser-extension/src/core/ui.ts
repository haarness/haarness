/**
 * What the popup and welcome page see and can ask for. Only the service worker
 * talks to Haarness; pages hold a `chrome.runtime` port named `ui`, receive a
 * fresh `UiSnapshot` whenever anything changes, and send `UiRequest`s.
 */
import type { ApprovalLine } from "./approvals";
import type { Connection, Pill } from "./status";

export interface UiChat {
  key: string;
  server: string | null;
  project: string;
  title: string;
  tabs: number;
  working: boolean;
  subagent: boolean;
  refused: "restricted" | "full" | null;
}

export interface UiApproval {
  server: string;
  id: string;
  title: string;
  project: string;
  lines: ApprovalLine[];
}

export interface UiProject {
  id: string;
  name: string;
  workspace: string;
  version: string;
  state: "connecting" | "connected" | "error";
  error?: string;
}

export interface UiSettings {
  language: string;
  label: string;
  cursor: boolean;
}

export interface UiSnapshot {
  connection: Connection;
  pill: Pill;
  /** Consecutive failed attempts to start the bridge. */
  failures: number;
  paused: boolean;
  stopped: boolean;
  /** A newer copy of the extension is on disk. */
  update: string | null;
  projects: UiProject[];
  chats: UiChat[];
  approvals: UiApproval[];
  settings: UiSettings;
  versions: { extension: string; haarness: string | null; browser: string };
  browserName: string;
  /** Where setup copied the extension, as the bridge reports it. */
  extensionPath: string | null;
  /** The keyboard shortcut for "Stop Haarness in this browser", if one is set. */
  shortcut: string | null;
}

export type UiRequest =
  | { type: "answer"; server: string; permission: string; decision: "always" | "reject" }
  | { type: "resume" }
  | { type: "pause"; paused: boolean }
  | { type: "stop-chat"; key: string }
  | { type: "show-tabs"; key: string }
  | { type: "close-tabs"; key: string }
  | { type: "close-all" }
  | { type: "reload" }
  | { type: "retry" }
  | { type: "settings"; settings: Partial<UiSettings> };

export type UiResponse = { ok: true } | { ok: false; error: string };
