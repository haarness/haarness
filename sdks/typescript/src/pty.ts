import WebSocket from "ws";
import { TransportError } from "./transport.js";
export interface PtyControl {
  type: "replay" | "cursor";
  start: number;
  end: number;
  truncated: boolean;
  /** On a truncated replay: the sequences that restore the terminal modes
   * the dropped output left on, to write to a reset terminal first. */
  modes?: string;
}
export type PtyMessage = Uint8Array | PtyControl;
/** One PTY socket. Closing it only detaches; deletePty terminates the process. */
export class PtyConnection implements AsyncIterable<PtyMessage> {
  cursor = 0;
  private queue: PtyMessage[] = [];
  private wake?: () => void;
  private ended = false;
  private error?: Error;
  private queuedBytes = 0;
  private constructor(
    private readonly socket: WebSocket,
    signal: AbortSignal,
  ) {
    socket.on("message", (data, binary) => {
      try {
        const message: PtyMessage = binary
          ? new Uint8Array(data as Buffer)
          : JSON.parse(data.toString());
        this.queuedBytes += binary
          ? (data as Buffer).byteLength
          : data.toString().length;
        if (this.queuedBytes > 8 * 1024 * 1024) {
          this.error = new TransportError(
            "PTY consumer exceeded 8 MiB buffer; reconnect with cursor",
          );
          this.close();
          return;
        }
        this.queue.push(message);
        this.wake?.();
      } catch {
        this.error = new TransportError("Invalid PTY control frame");
        this.close();
      }
    });
    socket.on("error", (error) => {
      this.error = error;
      this.ended = true;
      this.wake?.();
    });
    const abort = () => this.close();
    socket.on("close", () => {
      this.ended = true;
      signal.removeEventListener("abort", abort);
      this.wake?.();
    });
    signal.addEventListener("abort", abort, { once: true });
    if (signal.aborted) this.close();
  }
  static async connect(
    url: string,
    headers: Record<string, string>,
    signal: AbortSignal,
    timeoutMs: number,
  ): Promise<PtyConnection> {
    signal.throwIfAborted();
    const socket = new WebSocket(url, {
      headers,
      followRedirects: false,
      handshakeTimeout: timeoutMs,
      maxPayload: 8 * 1024 * 1024,
    });
    const connection = new PtyConnection(socket, signal);
    await new Promise<void>((resolve, reject) => {
      const opened = () => {
        socket.off("error", failed);
        socket.off("close", closed);
        resolve();
      };
      const failed = (error: Error) => {
        socket.off("open", opened);
        socket.off("close", closed);
        reject(new TransportError("PTY connection failed", { cause: error }));
      };
      const closed = () => failed(new Error("PTY closed before connection"));
      socket.once("open", opened);
      socket.once("error", failed);
      socket.once("close", closed);
    });
    return connection;
  }
  send(data: string | Uint8Array): void {
    this.socket.send(data, { binary: true });
  }
  close(): void {
    if (this.socket.readyState === WebSocket.CONNECTING)
      this.socket.terminate();
    else this.socket.close();
  }
  async *[Symbol.asyncIterator](): AsyncGenerator<PtyMessage> {
    try {
      while (true) {
        while (this.queue.length) {
          const value = this.queue.shift()!;
          this.queuedBytes -=
            value instanceof Uint8Array
              ? value.byteLength
              : JSON.stringify(value).length;
          if (
            !(value instanceof Uint8Array) &&
            value.type === "cursor" &&
            typeof value.end === "number"
          )
            this.cursor = value.end;
          yield value;
        }
        if (this.error) throw this.error;
        if (this.ended) return;
        await new Promise<void>((resolve) => {
          this.wake = resolve;
        });
      }
    } finally {
      this.close();
    }
  }
}
