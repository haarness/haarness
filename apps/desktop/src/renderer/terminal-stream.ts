/**
 * Whether what xterm sends may reach the shell. A terminal shown again (the
 * panel reopened, another terminal tab chosen, a reconnect) replays output
 * the shell wrote earlier, and xterm answers the queries in it (device
 * attributes, cursor position, colours) as if they had just arrived. Sent
 * on, those answers land in the shell or the program running there as typed
 * text, such as `^[[?1;2c` at the prompt.
 *
 * The gate is shut from the server's replay frame until xterm has parsed
 * every replayed byte: the first cursor frame follows the replay, and `flush`
 * calls back once xterm has parsed everything written before it. Keys typed
 * in that moment are dropped with the answers; the replay takes a moment.
 */
export function replayGate(flush: (done: () => void) => void) {
  let replaying = true;
  let generation = 0;
  let flushing = false;
  return {
    /** The server is about to replay output: shut the gate. */
    replay() {
      replaying = true;
      flushing = false;
      generation++;
    },
    /** A cursor frame: the first one after a replay opens the gate once
     * xterm has parsed the replay. */
    cursor() {
      if (!replaying || flushing) return;
      flushing = true;
      const current = generation;
      flush(() => {
        if (current !== generation) return;
        flushing = false;
        replaying = false;
      });
    },
    /** Whether xterm's data may go to the shell now. */
    open() {
      return !replaying;
    },
  };
}

/** The most a terminal sends the PTY in one write; a long paste goes in
 * several, in order. */
export const WRITE_LIMIT = 32 * 1024;

/** `bytes` cut into pieces of at most `limit` bytes. A cut may fall inside
 * a character: the PTY takes a byte stream. */
export function writeChunks(bytes: Uint8Array, limit = WRITE_LIMIT) {
  const chunks: Uint8Array[] = [];
  for (let start = 0; start < bytes.length; start += limit)
    chunks.push(bytes.subarray(start, start + limit));
  return chunks;
}

/** How long a terminal waits before its `attempt`th reconnect (from 0):
 * 250 ms, doubling up to 5 s. */
export function reconnectDelay(attempt: number) {
  return Math.min(5000, 250 * 2 ** Math.min(attempt, 5));
}
