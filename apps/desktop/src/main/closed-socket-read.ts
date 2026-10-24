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
  rawListeners(event: "uncaughtException"): Function[];
  removeListener(event: "uncaughtException", listener: Listener): unknown;
  on(event: "uncaughtException", listener: Listener): unknown;
}

/**
 * Notes a closed socket's read once (`record`) and goes on; every other
 * uncaught exception goes to the listener that was there, as before. In
 * Electron's main process that is Electron's own, the one listener, which
 * shows its dialog only while it is the one: this one takes its place, so
 * it still is. Anything else there (no listener outside Electron, several,
 * or one added with `once`, which `listeners()` would show unwrapped) is
 * left as it is, with nothing installed.
 */
export function quietClosedSocketReads(target: Target, record: (error: RangeError) => void) {
  const present = target.rawListeners("uncaughtException");
  if (present.length !== 1 || present[0] !== target.listeners("uncaughtException")[0]) return;
  const electron = present[0] as Listener;
  target.removeListener("uncaughtException", electron);
  let recorded = false;
  target.on("uncaughtException", (error, origin) => {
    if (closedSocketRead(error)) {
      if (!recorded) record(error);
      recorded = true;
      return;
    }
    electron.call(target, error, origin);
  });
}
