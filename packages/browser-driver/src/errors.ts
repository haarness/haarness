// Includes MIT-licensed code; see THIRD-PARTY-NOTICES.md.
import type { Browser } from "./rpc";

/**
 * A failure that carries a host failure code instead of `operation_failed`,
 * such as Chrome's `unsupported`. The server owns the model-facing text for
 * the codes it knows and replaces `message` for them.
 */
export class BrowserCodeError extends Error {
  readonly code: string;
  constructor(code: string, message: string) {
    super(message);
    this.name = "BrowserCodeError";
    this.code = code;
  }
}

// Hints that name where a page's traffic goes and what the user can see. The
// Haarness browser loads sites through the connected server inside the
// Desktop window; Chrome loads them on the user's own computer and network.
const hints = {
  haarness: {
    refused:
      "The browser could not connect to the site. Check its hostname/port on the connected server. localhost means that server, not the desktop.",
    unresolved:
      "The connected server could not resolve the hostname. Check the URL spelling and the server's DNS/network connection.",
    tls: "The desktop rejected the site's TLS connection. Ask the user to fix the certificate or trust configuration; do not bypass certificate checks.",
    network:
      "The site failed to load through the connected server. Check its URL/network and the Haarness connection before retrying; inspect the current tab first.",
    capture:
      "Chromium could not capture a rendered frame. Call browser.tabs.focus({tabID}) and keep the desktop window visible. If it is already visible, report the capture failure instead of repeating it unchanged.",
    unsupported:
      "This Chromium target does not support or allow the operation. Check Desktop/server compatibility and report it; do not retry unchanged or disable browser security.",
  },
  chrome: {
    refused:
      "Chrome could not connect to the site. Check its hostname/port. localhost means the user's computer.",
    unresolved:
      "Chrome could not resolve the hostname. Check the URL spelling; the user's network may be offline.",
    tls: "Chrome rejected the site's TLS connection. Ask the user to fix the certificate or trust configuration; do not bypass certificate checks.",
    network:
      "The site failed to load in the user's Chrome. Check its URL before retrying; inspect the current tab first.",
    capture:
      "Chrome could not capture a rendered frame. Call browser.tabs.focus({tabID}) first. If the tab is already showing, report the capture failure instead of repeating it unchanged.",
    unsupported:
      "Chrome does not support or allow this operation for Haarness. Report it; do not retry unchanged or disable browser security.",
  },
} satisfies Record<Browser.BrowserName, Record<string, string>>;

/** A DevTools command failure with a recovery hint; `host` picks the wording. */
export function protocolError(
  method: string,
  error: unknown,
  host: Browser.BrowserName = "haarness",
) {
  const detail = message(error);
  const recovery =
    method === "DOM.setFileInputFiles" && /not.*file input/i.test(detail)
      ? "Target is not a file input. Call browser.snapshot({tabID}) and choose an input[type=file] ref for browser.files.upload, or use browser.files.drop for a drop area."
      : /(?:node|object).*(?:not found|not exist)|(?:find|resolve).*(?:node|object)|detached/i.test(
            detail,
          )
        ? "The element may have detached. Call browser.snapshot({tabID}) and use a fresh ref from that tab."
        : /context.*(?:destroyed|not found)|find.*context|session.*not found/i.test(
              detail,
            )
          ? "The document or frame changed. Call browser.frames({tabID}) and browser.snapshot({tabID}); use current frame IDs and refs."
          : /wasn't found|method not found|not implemented|not allowed/i.test(
                detail,
              )
            ? hints[host].unsupported
            : "Inspect browser.tabs.list({}) and the target tab before deciding to retry; a partially completed action is not automatically safe to repeat.";
  return new Error(`${recovery} Chromium command ${method} failed: ${detail}`, {
    cause: error,
  });
}

/** An error from a tool call as its outcome; `host` picks the wording of hints. */
export function browserFailure(
  action: Browser.Action,
  error: unknown,
  host: Browser.BrowserName = "haarness",
): Extract<Browser.Outcome, { type: "failure" }> {
  if (error instanceof BrowserCodeError)
    return {
      type: "failure",
      code: error.code,
      message: methodNames(error.message).slice(0, 2_048),
    };
  const detail = message(error, 1_700);
  const navigation = [
    "tabs.open",
    "navigate",
    "back",
    "forward",
    "reload",
  ].includes(action.type);
  const network = navigation ? detail.match(/\bERR_[A-Z_]+\b/)?.[0] : undefined;
  const wording = hints[host];
  const hint =
    network === "ERR_CONNECTION_REFUSED"
      ? wording.refused
      : network === "ERR_NAME_NOT_RESOLVED"
        ? wording.unresolved
        : network?.startsWith("ERR_CERT_") || network?.startsWith("ERR_SSL_")
          ? wording.tls
          : network === "ERR_ABORTED"
            ? "Navigation was interrupted or became a download. Inspect browser.tabs.list({}) and browser.files.list({tabID}) before deciding to navigate again."
            : network
              ? wording.network
              : action.type === "screenshot" &&
                  /UnknownVizError|capture.*(?:failed|unavailable)/i.test(
                    detail,
                  )
                ? wording.capture
                : undefined;
  return {
    type: "failure",
    code: network ? "navigation_failed" : "operation_failed",
    message: methodNames(
      `browser.${action.type} failed. ${hint ? `${hint} Details: ${detail.slice(0, 400)}` : detail}`,
    ).slice(0, 2_048),
  };
}

/** The model calls tools by their catalog names: browser.files.get → browser_files_get. */
function methodNames(text: string) {
  return text.replace(
    /browser\.([a-z_]+(?:\.[a-z_]+)?)/g,
    (_, method: string) => "browser_" + method.replaceAll(".", "_"),
  );
}

function message(error: unknown, limit = 400) {
  return (error instanceof Error ? error.message : String(error)).slice(
    0,
    limit,
  );
}
