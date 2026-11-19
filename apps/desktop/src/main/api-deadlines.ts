import { ApiError, TransportError } from "@haarness/sdk";
import { COMPACTION_WAIT_MS } from "../shared";

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

/**
 * Whether a call failed on its way to the server rather than at it: a proxy
 * gave up (Cloudflare's 524 at its 100 s limit, a gateway's 502 or 504
 * without the runtime's own error code, Cloud's relay losing the computer's
 * answer) or the connection dropped. A compaction goes on there and lands.
 */
export function cutOnTheWay(error: unknown): boolean {
  if (error instanceof ApiError)
    return (
      error.status === 524 ||
      ((error.status === 502 || error.status === 504) &&
        (error.code === "http_error" || error.code === "bad_gateway"))
    );
  // A response that came but could not be read has no cause; a request that
  // never got one does.
  return error instanceof TransportError && error.cause !== undefined;
}
