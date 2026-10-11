/**
 * What the popup and welcome page see and can ask for. Only the service worker
 * talks to Haarness; pages hold a `chrome.runtime` port named `ui`, receive a
 * fresh `UiSnapshot` whenever anything changes, and send `UiRequest`s.
 */
import type { ApprovalLine } from "./approvals";
import { key } from "./i18n";
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
  /** Why it is not connected: an interface key, or Haarness's own text when it has none. */
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

/**
 * A failure is an English interface key, translated where the page shows
 * it, with the values of its `{placeholders}`.
 */
export type UiResponse = { ok: true } | { ok: false; error: string; values?: Record<string, string> };

export const failure = (error: string, values?: Record<string, string>): UiResponse =>
  values ? { ok: false, error, values } : { ok: false, error };

/** Texts the bridge answers with, as interface keys. */
const answers: Record<string, string> = {
  "This Haarness server is not connected.": key("This project is not connected."),
  "Chrome can only answer a Chrome approval that is waiting.": key("This approval is no longer waiting."),
  "Haarness did not answer in time": key("Haarness did not answer."),
  "Haarness's answer was cut off": key("Haarness did not answer."),
  "Haarness is not reachable": key("Haarness is not reachable."),
};

/** Why Haarness (through the bridge) could not do something; text it has no key for is shown as the reason. */
export function haarnessFailure(error: string | undefined): UiResponse {
  if (!error) return failure(key("Haarness did not answer."));
  return answers[error] ? failure(answers[error]) : failure(key("Haarness could not do that: {reason}"), { reason: error });
}

/** Why the bridge cannot reach a project's server, as interface keys. */
const problems: Record<string, string> = {
  "restart this project's Haarness server": key("Restart this project's Haarness server."),
  "this project's Haarness server does not accept Chrome; restart it without --anonymous": key(
    "This project's Haarness server does not accept Chrome. Restart it without --anonymous.",
  ),
  "not reachable": key("This project's Haarness server is not reachable."),
  "did not answer in time": key("This project's Haarness server did not answer in time."),
  "invalid server address": key("Restart this project's Haarness server."),
  "invalid server token": key("Restart this project's Haarness server."),
};

export const projectProblem = (error: string) => problems[error] ?? error;
