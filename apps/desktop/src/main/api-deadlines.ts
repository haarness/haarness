import { ApiError, TransportError } from "@haarness/sdk";
import { COMPACTION_WAIT_MS } from "../shared";
import { compareVersions } from "../updates";

/**
 * Server calls that may take longer than the client's own 30 s (45 s
 * through a relay), with the argument that holds their request options. A
 * compaction's summary of a long chat can take minutes, and the server
 * finishes it even when the request goes: Desktop waits as long as the
 * phones and the web do, so a slow summary ends in success, not a timeout.
 */
const DEADLINES: Record<string, { options: number; timeoutMs: number }> = {
  compact: { options: 2, timeoutMs: COMPACTION_WAIT_MS },
};

/** `args` for `method`, with its longer deadline when it has one. */
export function withDeadline(method: string, args: unknown[]): unknown[] {
  const deadline = Object.hasOwn(DEADLINES, method) ? DEADLINES[method] : undefined;
  if (!deadline || args.length > deadline.options + 1) return args;
  const given = args[deadline.options];
  const options = given && typeof given === "object" ? (given as Record<string, unknown>) : {};
  const result = [...args];
  result[deadline.options] = { ...options, timeoutMs: deadline.timeoutMs };
  return result;
}

/** The first release whose server finishes a summary whose request went. */
const FINISHES_CUT_COMPACTIONS = "0.1.24";

/** Where a compact call went: through Cloud's relay, to which address, and
 * the release of the server there. */
export interface CallPath {
  relayed: boolean;
  url?: string;
  version?: string;
}

/** Failures to reach a server at all: the request never got there. */
const CONNECT_FAILURES = new Set([
  "ECONNREFUSED",
  "ENOTFOUND",
  "EHOSTUNREACH",
  "ENETUNREACH",
  "EAI_AGAIN",
  "UND_ERR_CONNECT_TIMEOUT",
]);

function connectFailed(error: unknown): boolean {
  let cause: unknown = error;
  for (let depth = 0; cause && typeof cause === "object" && depth < 8; depth++) {
    const code = (cause as { code?: unknown }).code;
    if (typeof code === "string" && CONNECT_FAILURES.has(code)) return true;
    cause = (cause as { cause?: unknown }).cause;
  }
  return false;
}

function onThisComputer(url: string | undefined): boolean {
  if (!url) return true;
  try {
    const host = new URL(url).hostname.replace(/^\[|\]$/g, "");
    return host === "localhost" || host === "::1" || /^127\./.test(host);
  } catch {
    return true;
  }
}

/** The server said another compaction of the chat is under way, which lands
 * as its own `thread.compacted`. */
export function alreadyCompacting(error: unknown): boolean {
  return error instanceof ApiError && error.status === 409 && /already being compacted/i.test(error.message);
}

/**
 * Whether a compact call's answer was lost on its way back from another
 * computer, which goes on with the summary: a proxy gave up (Cloudflare's 524
 * at its 100 s limit, a gateway's 502 or 504 with no error of the runtime's
 * own), Cloud lost the computer's connection while the request was out, or
 * the connection to a server elsewhere dropped. Not a failure to connect, not
 * a server on this computer (gone, and its summary with it), not Cloud's word
 * that the computer's runtime failed, and not a release older than the one
 * that finishes such summaries. Another compaction under way is waited for
 * whatever the release.
 */
export function cutOnTheWay(error: unknown, path: CallPath): boolean {
  if (alreadyCompacting(error)) return true;
  if (!path.version || compareVersions(path.version, FINISHES_CUT_COMPACTIONS) < 0) return false;
  if (error instanceof ApiError)
    return (
      error.status === 524 ||
      ((error.status === 502 || error.status === 504) && error.code === "http_error") ||
      (error.status === 409 &&
        (error.code === "remote_offline" || /offline or its connection changed/i.test(error.message)))
    );
  // A response that came but could not be read has no cause; a request that
  // never got one does.
  return (
    error instanceof TransportError &&
    error.cause !== undefined &&
    (path.relayed || !onThisComputer(path.url)) &&
    !connectFailed(error)
  );
}
