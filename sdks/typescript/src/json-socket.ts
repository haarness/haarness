import WebSocket from "ws";
import { TransportError } from "./transport.js";
import type { JsonValue } from "./types.js";
/** Bounded JSON protocol connection for browser hosts and Cloud relays. */
export class JsonSocket implements AsyncIterable<JsonValue> {
  private queue: {value: JsonValue; bytes: number}[] = [];
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
        if (binary) throw new Error("Expected a JSON text frame");
        const message: JsonValue = JSON.parse(data.toString());
        this.queuedBytes += binary
          ? (data as Buffer).byteLength
          : data.toString().length;
        if (this.queuedBytes > 24 * 1024 * 1024) {
          this.error = new TransportError(
            "JSON WebSocket consumer exceeded 24 MiB buffer; reconnect with cursor",
          );
          this.close();
          return;
        }
        this.queue.push({value: message, bytes: data.toString().length});
        this.wake?.();
      } catch {
        this.error = new TransportError("Invalid JSON WebSocket control frame");
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
  ): Promise<JsonSocket> {
    signal.throwIfAborted();
    const socket = new WebSocket(url, {
      headers,
      followRedirects: false,
      handshakeTimeout: timeoutMs,
      maxPayload: 24 * 1024 * 1024,
    });
    const connection = new JsonSocket(socket, signal);
    await new Promise<void>((resolve, reject) => {
      const opened = () => {
        socket.off("error", failed);
        socket.off("close", closed);
        resolve();
      };
      const failed = (error: Error) => {
        socket.off("open", opened);
        socket.off("close", closed);
        reject(new TransportError("JSON WebSocket connection failed", { cause: error }));
      };
      const closed = () => failed(new Error("JSON WebSocket closed before connection"));
      socket.once("open", opened);
      socket.once("error", failed);
      socket.once("close", closed);
    });
    return connection;
  }
  send(data: JsonValue): void {
    this.socket.send(JSON.stringify(data), { binary: false });
  }
  close(): void {
    if (this.socket.readyState === WebSocket.CONNECTING)
      this.socket.terminate();
    else this.socket.close();
  }
  async *[Symbol.asyncIterator](): AsyncGenerator<JsonValue> {
    try {
      while (true) {
        while (this.queue.length) {
          const item = this.queue.shift()!;
          this.queuedBytes -= item.bytes;
          yield item.value;
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
