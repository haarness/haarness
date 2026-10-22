import WebSocket from "ws";
import {
  HttpTransport,
  type ClientOptions,
  type EventOptions,
  TransportError,
} from "./transport.js";
import type { CloudEntityEvent } from "./cloud-types.js";
export type { RequestOptions } from "./transport.js";
export interface CloudOptions extends ClientOptions {
  accountId?: string;
}
export interface CloudEventOptions
  extends Omit<EventOptions, "after" | "idleTimeoutMs"> {
  accountId?: string;
}
/** Cloud credentials never fall back to the unrelated local server token. */
export class Transport extends HttpTransport {
  private readonly accountId?: string;
  constructor(options: CloudOptions) {
    super({
      ...options,
      token: options.token ?? process.env.HAARNESS_API_KEY ?? process.env.HAARNESS_CLOUD_TOKEN ?? "",
    });
    this.accountId = options.accountId;
  }
  /** Invalidation hints. Reload snapshots on ready/resync_required, including reconnect. */
  async *events(
    options: CloudEventOptions = {},
  ): AsyncGenerator<CloudEntityEvent> {
    const signal = this.signal(options.signal);
    let delay = options.retryMs ?? 500;
    while (!signal.aborted) {
      signal.throwIfAborted();
      const socket = new WebSocket(
        this.url("/api/v1/events", {
          accountId: options.accountId ?? this.accountId,
        }).replace(/^http/, "ws"),
        {
          headers: await this.headers(),
          followRedirects: false,
          handshakeTimeout: this.timeoutMs,
          maxPayload: 64 * 1024,
        },
      );
      const queue: CloudEntityEvent[] = [];
      let wake: (() => void) | undefined;
      let ended = false;
      let failure: Error | undefined;
      let watchdog: ReturnType<typeof setTimeout>;
      const heartbeat = () => {
        clearTimeout(watchdog);
        watchdog = setTimeout(() => {
          failure = new TransportError("Cloud event heartbeat timed out");
          socket.terminate();
        }, 45_000);
      };
      const stop = () => socket.terminate();
      signal.addEventListener("abort", stop, { once: true });
      socket.on("open", heartbeat);
      socket.on("message", (data) => {
        heartbeat();
        try {
          const event = JSON.parse(data.toString()) as CloudEntityEvent;
          if (!event || typeof event !== "object" || !("type" in event))
            throw new Error("Invalid Cloud event");
          if (queue.length >= 256) {
            queue.length = 0;
            queue.push({ type: "resync_required" });
          }
          queue.push(event);
          wake?.();
        } catch {
          failure = new TransportError("Invalid Cloud event");
          socket.terminate();
        }
      });
      socket.on("error", (error) => {
        failure = error;
      });
      socket.on("close", () => {
        ended = true;
        wake?.();
      });
      try {
        while (!ended || queue.length) {
          signal.throwIfAborted();
          const event = queue.shift();
          if (event) {
            delay = options.retryMs ?? 500;
            yield event;
          } else
            await new Promise<void>((resolve) => {
              wake = resolve;
            });
        }
        signal.throwIfAborted();
        if (!options.reconnect) {
          if (failure) throw failure;
          return;
        }
        yield { type: "resync_required" };
      } finally {
        clearTimeout(watchdog!);
        signal.removeEventListener("abort", stop);
        socket.terminate();
      }
      await new Promise<void>((resolve, reject) => {
        const abort = () => {
          clearTimeout(timer);
          reject(signal.reason);
        };
        const timer = setTimeout(() => {
          signal.removeEventListener("abort", abort);
          resolve();
        }, delay);
        signal.addEventListener("abort", abort, { once: true });
        if (signal.aborted) abort();
      });
      delay = Math.min(delay * 2, 30_000);
    }
    signal.throwIfAborted();
  }
}
