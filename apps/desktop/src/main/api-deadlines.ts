/**
 * Server calls that may take longer than the client's own 30 s (45 s
 * through a relay), with the argument that holds their request options. A
 * compaction's summary of a long chat can take minutes, and the server
 * finishes it even when the request goes: Desktop waits as long as the
 * phones and the web do, so a slow summary ends in success, not a timeout.
 */
const DEADLINES: Record<string, { options: number; timeoutMs: number }> = {
  compact: { options: 2, timeoutMs: 310_000 },
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
