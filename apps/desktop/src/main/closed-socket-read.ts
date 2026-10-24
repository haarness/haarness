/**
 * A read that reaches a socket already closed, which Node throws where
 * nothing can catch it.
 *
 * Node hands every read of a socket to its `onStreamRead`. When that socket
 * is already marked destroyed, the bytes that arrived (216, say) are taken
 * for an error number, and building that error throws instead:
 * `RangeError [ERR_OUT_OF_RANGE]: The value of "err" is out of range. It must
 * be a negative integer. Received 216`, from `getSystemErrorName` under
 * `new ErrnoException` under `onStreamRead`. It leaves the read callback
 * itself, so no `'error'` listener hears it, and Electron shows its "A
 * JavaScript error occurred in the main process" dialog. A socket gets there
 * when code that ends it runs in the middle of one of its own reads: an
 * inspector's evaluate (Playwright quitting the app, a debugger attached to
 * main) runs between any two JavaScript calls. The socket was being closed
 * anyway and nothing is lost, so a dead connection never interrupts the
 * person: this one error is noted once and the app goes on.
 */

const MESSAGE =
  /^The value of "err" is out of range\. It must be a negative integer\. Received [1-9]\d*$/;
/** The whole stack, called from the read callback: nothing above Node's frames. */
const FRAMES = [
  /^ {4}at (?:\w+\.)?getSystemErrorName \(node:util:\d+:\d+\)$/,
  /^ {4}at new ErrnoException \(node:internal\/errors:\d+:\d+\)$/,
  /^ {4}at (?:\w+\.)?onStreamRead \(node:internal\/stream_base_commons:\d+:\d+\)$/,
];

/** Whether `error` is exactly that error of Node's, and no other. */
export function closedSocketRead(error: unknown): error is RangeError {
  if (!(error instanceof RangeError)) return false;
  if ((error as { code?: unknown }).code !== "ERR_OUT_OF_RANGE" || !MESSAGE.test(error.message))
    return false;
  const frames = (error.stack ?? "").split("\n").slice(1);
  return frames.length === FRAMES.length && FRAMES.every((frame, index) => frame.test(frames[index]));
}

type Listener = (error: Error, origin: string) => void;
interface Target {
  listeners(event: "uncaughtException"): Function[];
  removeAllListeners(event: "uncaughtException"): unknown;
  on(event: "uncaughtException", listener: Listener): unknown;
}

/**
 * Notes a closed socket's read once (`record`) and goes on; every other
 * uncaught exception goes to the listeners that were there, as before. In
 * Electron's main process that is Electron's own, which shows its dialog
 * only while it is the one listener: this one takes its place, so it still
 * is. Without a listener there (outside Electron) nothing changes.
 */
export function quietClosedSocketReads(target: Target, record: (error: RangeError) => void) {
  const earlier = target.listeners("uncaughtException");
  if (!earlier.length) return;
  target.removeAllListeners("uncaughtException");
  let recorded = false;
  target.on("uncaughtException", (error, origin) => {
    if (closedSocketRead(error)) {
      if (!recorded) record(error);
      recorded = true;
      return;
    }
    for (const listener of earlier) listener.call(target, error, origin);
  });
}
