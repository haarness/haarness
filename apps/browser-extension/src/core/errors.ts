/**
 * Failures the extension reports with a code. The server owns the model-facing
 * text for these codes and replaces `message`; the messages here are what an
 * older server, a log, or a test sees.
 */
import type { Action, Outcome } from "./browser";

export type FailureCode =
  | "paused"
  | "stopped-by-user"
  | "unsupported"
  | "restricted-page"
  | "not-in-group"
  | "site-blocked"
  | "background-tab";

const defaults: Record<FailureCode, string> = {
  paused: "The user paused Haarness in Chrome.",
  "stopped-by-user": "The user stopped Haarness in Chrome.",
  unsupported: "This browser tool is not available in Chrome.",
  "restricted-page": "Haarness cannot control this Chrome page.",
  "not-in-group": "This tab is no longer in the chat's Chrome tab group. Call browser_tabs_list.",
  "site-blocked": "Chrome blocked this site.",
  "background-tab": "Chrome cannot capture this tab while it is in the background.",
};

export class HostError extends Error {
  constructor(
    readonly code: FailureCode,
    message = defaults[code],
  ) {
    super(message);
  }
}

const NAVIGATIONS = new Set(["tabs.open", "navigate", "back", "forward", "reload"]);

export const CANCELLED =
  "Browser command cancelled. Inspect the page before repeating a mutation.";
export const TARGET_CHANGED = "Browser target changed while permission was pending";
export const NOT_THIS_CHAT = "Tab is closed or belongs to another conversation. Call browser_tabs_list.";

/** One failure outcome; uncoded errors keep Desktop's `operation_failed` shape. */
export function failure(action: Pick<Action, "type"> | undefined, error: unknown): Extract<Outcome, { type: "failure" }> {
  if (error instanceof HostError) return { type: "failure", code: error.code, message: error.message.slice(0, 2_048) };
  const detail = (error instanceof Error ? error.message : String(error)).slice(0, 1_700);
  const method = (action?.type ?? "tabs.list").replaceAll(".", "_");
  const network = NAVIGATIONS.has(action?.type ?? "") ? /\bERR_[A-Z_]+\b/.exec(detail)?.[0] : undefined;
  if (network)
    return {
      type: "failure",
      code: "navigation_failed",
      message: `browser_${method} failed. Chrome could not load the page (${network}) on the user's own network. Check the URL; inspect the tab with browser_tabs_list before retrying. Details: ${detail.slice(0, 400)}`,
    };
  return { type: "failure", code: "operation_failed", message: `browser_${method} failed. ${detail}`.slice(0, 2_048) };
}
