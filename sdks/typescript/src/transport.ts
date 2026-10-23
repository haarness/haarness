import type { EventEnvelope } from "./types.js";

export type ServerAuth =
  | { type: "none" }
  | { type: "bearer"; token: string }
  | { type: "basic"; username: string; password: string };

export function authorizationHeaders(
  auth: ServerAuth | string,
): Record<string, string> {
  if (typeof auth === "string")
    auth = auth ? { type: "bearer", token: auth } : { type: "none" };
  if (auth.type === "none") return {};
  if (auth.type === "bearer") {
    if (/[\r\n]/.test(auth.token)) throw new TypeError("Invalid server token");
    return auth.token ? { Authorization: `Bearer ${auth.token}` } : {};
  }
  const username = auth.username.trim() || "haarness";
  if (/[:\x00-\x1f\x7f]/.test(username))
    throw new TypeError("Username cannot contain colons or control characters");
  if (!auth.password) return {};
  return {
    Authorization: `Basic ${Buffer.from(`${username}:${auth.password}`, "utf8").toString("base64")}`,
  };
}

export interface ClientOptions {
  /** Resolve short-lived runtime credentials before each request or reconnect. */
  resolveAuth?: () => Promise<ServerAuth>;
  baseUrl: string;
  token?: string;
  auth?: ServerAuth;
  username?: string;
  password?: string;
  /** HTTP request deadline. Streams have no overall deadline. Default: 30 seconds. */
  timeoutMs?: number;
  fetch?: typeof globalThis.fetch;
}
export interface RequestOptions {
  signal?: AbortSignal;
  timeoutMs?: number;
  headers?: Record<string, string>;
}
const REQUEST_OPTIONS = new Set(["signal", "timeoutMs", "headers"]);
/** One of `A` or `B`, never a mix of their keys. */
export type Exclusive<A, B> = (A & { [K in keyof B]?: never }) | (B & { [K in keyof A]?: never });
/**
 * The first argument of a method that took request options there before it
 * took a query, as its query and options: request option keys are options
 * (a call written before the query), the rest is the query. The generator
 * makes sure no query key is also an option key.
 */
export function splitQuery<Q extends object>(
  first: Q | RequestOptions,
  options: RequestOptions,
): [Q, RequestOptions] {
  const query: Record<string, unknown> = {};
  const moved: Record<string, unknown> = {};
  for (const [key, value] of Object.entries(first)) (REQUEST_OPTIONS.has(key) ? moved : query)[key] = value;
  return [query as Q, { ...moved, ...options }];
}
type WireOptions = RequestOptions & {
  body?: unknown;
  query?: object;
  binaryBody?: boolean;
  binaryResponse?: boolean;
  contentType?: string;
  textResponse?: boolean;
  redirectResponse?: boolean;
};
export class ApiError extends Error {
  constructor(
    public readonly status: number,
    public readonly code: string,
    message: string,
    public readonly retryable: boolean,
    public readonly body: unknown,
  ) {
    super(message);
    this.name = "ApiError";
  }
}
export class TransportError extends Error {
  constructor(message: string, options?: ErrorOptions) {
    super(message, options);
    this.name = "TransportError";
  }
}
export type ServerEvent =
  EventEnvelope | { type: "resync_required"; schemaVersion?: string };
export interface EventOptions extends Omit<RequestOptions, "timeoutMs"> {
  after?: number;
  reconnect?: boolean;
  retryMs?: number;
  /**
   * How long the stream may stay silent, in ms, before it counts as lost.
   * Every chunk resets it, the keepalive comments a server sends every 15 s
   * included, so only a connection that died without closing (a laptop that
   * slept, a network that changed, a proxy that froze) reaches it. Then the
   * stream connects again from its cursor with `reconnect`, and otherwise
   * ends with a `TransportError`. Time the consumer spends with an event is
   * not counted. A wait that ran far past its time (the machine slept, or
   * the event loop was blocked) first gives the server min(5 s, limit) more.
   */
  idleTimeoutMs?: number;
}

/**
 * One connection attempt's silence limit. `heard` on every chunk; `pause`
 * while the consumer has an event, whose time is not the server's. A timer
 * that fires more than min(5 s, limit) after it was due says this process
 * could not run, not that the server went quiet, so it gives the server
 * min(5 s, limit) more, once per silence: hearing anything starts the next
 * silence afresh.
 */
function silence(limit: number | undefined, lost: () => void) {
  let timer: ReturnType<typeof setTimeout> | undefined;
  let due = 0;
  let extended = false;
  let expired = false;
  const arm = (ms: number) => {
    due = Date.now() + ms;
    timer = setTimeout(fire, ms);
  };
  const fire = () => {
    if (!extended && Date.now() - due > Math.min(5_000, limit!)) {
      extended = true;
      arm(Math.min(5_000, limit!));
      return;
    }
    expired = true;
    lost();
  };
  if (limit !== undefined) {
    if (!(limit > 0)) throw new TypeError("idleTimeoutMs must be positive");
    arm(limit);
  }
  return {
    heard() {
      if (limit === undefined || expired) return;
      clearTimeout(timer);
      extended = false;
      arm(limit);
    },
    pause() {
      clearTimeout(timer);
    },
    stop() {
      clearTimeout(timer);
    },
    get expired() {
      return expired;
    },
  };
}

export class HttpTransport {
  readonly baseUrl: string;
  private readonly authentication: Record<string, string>;
  private readonly resolveAuth?: () => Promise<ServerAuth>;
  protected readonly timeoutMs: number;
  protected readonly fetcher: typeof globalThis.fetch;
  private readonly lifetime = new AbortController();
  constructor(options: ClientOptions) {
    const url = new URL(options.baseUrl);
    if (
      !["http:", "https:"].includes(url.protocol) ||
      url.username ||
      url.password ||
      url.search ||
      url.hash
    )
      throw new TypeError(
        "Use an HTTP(S) server URL without credentials, query or fragment",
      );
    this.resolveAuth = options.resolveAuth;
    this.baseUrl = url.href.replace(/\/+$/, "");
    this.authentication = authorizationHeaders(
      options.auth ??
        (options.password !== undefined
          ? {
              type: "basic",
              username: options.username ?? "haarness",
              password: options.password,
            }
          : (options.token ?? process.env.HAARNESS_SERVER_TOKEN ?? "")),
    );
    this.timeoutMs = options.timeoutMs ?? 30_000;
    if (!(this.timeoutMs > 0))
      throw new TypeError("timeoutMs must be positive");
    this.fetcher = options.fetch ?? globalThis.fetch;
  }
  /** Detach this client and cancel its HTTP streams; server work continues. */
  close(): void {
    this.lifetime.abort();
  }
  protected segment(value: string): string {
    if (!value || value === "." || value === "..")
      throw new TypeError(
        "Path parameters must be nonempty and cannot be dot segments",
      );
    return encodeURIComponent(value);
  }
  protected url(path: string, query?: object): string {
    const url = new URL(this.baseUrl + path);
    for (const [key, value] of Object.entries(query ?? {}))
      if (value !== undefined && value !== null)
        url.searchParams.set(key, String(value));
    return url.href;
  }
  protected async headers(): Promise<Record<string, string>> {
    return this.resolveAuth ? authorizationHeaders(await this.resolveAuth()) : { ...this.authentication };
  }
  protected signal(signal?: AbortSignal): AbortSignal {
    return signal
      ? AbortSignal.any([signal, this.lifetime.signal])
      : this.lifetime.signal;
  }
  protected async failure(response: Response): Promise<never> {
    const text = await response.text();
    let body: unknown = text;
    try {
      body = JSON.parse(text);
    } catch {
      /* Framework errors can be plain text. */
    }
    const outer = body && typeof body === "object" ? body as Record<string, unknown> : {};
    const record = outer.error && typeof outer.error === "object" ? outer.error as Record<string, unknown> : outer;
    throw new ApiError(
      response.status,
      String(record.code ?? "http_error"),
      String(record.message ?? record.error ?? `HTTP ${response.status}`),
      record.retryable === true,
      body,
    );
  }
  protected async request<T>(
    method: string,
    path: string,
    options: WireOptions = {},
  ): Promise<T> {
    const headers = { ...await this.headers(), ...options.headers };
    const deadline = new AbortController();
    const timeout = options.timeoutMs ?? this.timeoutMs;
    if (!(timeout > 0)) throw new TypeError("timeoutMs must be positive");
    const timer = setTimeout(
      () =>
        deadline.abort(new DOMException("Request timed out", "TimeoutError")),
      timeout,
    );
    const signal = AbortSignal.any([
      this.signal(options.signal),
      deadline.signal,
    ]);
    try {
      if (options.body !== undefined)
        headers["Content-Type"] = options.contentType ?? (options.binaryBody
          ? "application/zip"
          : "application/json");
      const response = await this.fetcher(this.url(path, options.query), {
        method,
        headers,
        redirect: options.redirectResponse ? "manual" : "error",
        signal,
        body:
          options.body === undefined
            ? undefined
            : options.binaryBody
              ? (options.body as BodyInit)
              : options.contentType === "application/x-www-form-urlencoded"
                ? new URLSearchParams(options.body as Record<string,string>).toString()
                : options.contentType === "text/plain" ? String(options.body) : JSON.stringify(options.body),
      });
      if (options.redirectResponse && response.status >= 300 && response.status < 400) {
        await response.body?.cancel();
        return { status: response.status, location: response.headers.get("location") } as T;
      }
      if (!response.ok) return await this.failure(response);
      if (options.binaryResponse)
        return new Uint8Array(await response.arrayBuffer()) as T;
      const text = await response.text();
      if (options.textResponse) return text as T;
      if (!text) return undefined as T;
      try {
        return JSON.parse(text) as T;
      } catch {
        throw new TransportError("Server returned invalid JSON");
      }
    } catch (error) {
      if (error instanceof ApiError || error instanceof TransportError)
        throw error;
      if (signal.aborted) throw signal.reason;
      throw new TransportError("Haarness request failed", { cause: error });
    } finally {
      clearTimeout(timer);
    }
  }
}

export class Transport extends HttpTransport {
  /** Ordered events. Resync markers are yielded; hydrate REST snapshots before reconnecting. */
  async *events(options: EventOptions = {}): AsyncGenerator<ServerEvent> {
    const signal = this.signal(options.signal);
    let cursor = options.after;
    let instance: string | undefined;
    while (!signal.aborted) {
      const headers = {
        ...await this.headers(),
        Accept: "text/event-stream",
        ...(cursor === undefined ? {} : { "Last-Event-ID": String(cursor) }),
      };
      let reader: ReadableStreamDefaultReader<Uint8Array> | undefined;
      const attempt = new AbortController();
      const quiet = silence(options.idleTimeoutMs, () => {
        attempt.abort();
        void reader?.cancel().catch(() => {});
      });
      try {
        const response = await this.fetcher(this.url("/v1/events"), {
          headers,
          redirect: "error",
          signal: AbortSignal.any([signal, attempt.signal]),
        });
        if (!response.ok) await this.failure(response);
        if (!response.body)
          throw new TransportError("Event stream has no body");
        quiet.heard();
        reader = response.body!.getReader();
        const decoder = new TextDecoder();
        let buffer = "";
        let data: string[] = [];
        let size = 0;
        while (true) {
          const chunk = await reader.read();
          if (quiet.expired) break;
          quiet.heard();
          buffer += decoder.decode(chunk.value, { stream: !chunk.done });
          if (chunk.done && buffer.endsWith("\r")) buffer += "\n";
          if (buffer.length > 8 * 1024 * 1024)
            throw new TransportError("Event exceeds 8 MiB");
          let match: RegExpExecArray | null;
          while ((match = /\r\n|\r(?!$)|\n/.exec(buffer))) {
            const line = buffer.slice(0, match.index);
            buffer = buffer.slice(match.index + match[0].length);
            if (line === "") {
              if (!data.length) continue;
              const event = JSON.parse(data.join("\n")) as ServerEvent;
              data = [];
              size = 0;
              if (!("eventId" in event)) {
                yield event;
                return;
              }
              if (instance && instance !== event.serverInstanceId) {
                yield { type: "resync_required" };
                return;
              }
              instance = event.serverInstanceId;
              if (cursor !== undefined && event.eventId <= cursor) continue;
              cursor = event.eventId;
              // The consumer's time with it is not the server's silence.
              quiet.pause();
              yield event;
              quiet.heard();
            } else if (line.startsWith("data:")) {
              size += line.length;
              if (size > 8 * 1024 * 1024)
                throw new TransportError("Event exceeds 8 MiB");
              data.push(line.slice(5).replace(/^ /, ""));
            }
          }
          if (chunk.done) break;
        }
        if (quiet.expired && !options.reconnect)
          throw new TransportError(
            `The event stream sent nothing for ${options.idleTimeoutMs} ms`,
          );
      } catch (error) {
        if (signal.aborted) return;
        if (quiet.expired) {
          if (!options.reconnect)
            throw error instanceof TransportError
              ? error
              : new TransportError(
                  `The event stream sent nothing for ${options.idleTimeoutMs} ms`,
                  { cause: error },
                );
        } else if (
          !options.reconnect ||
          error instanceof ApiError ||
          error instanceof SyntaxError ||
          error instanceof TransportError
        )
          throw error;
      } finally {
        quiet.stop();
        await reader?.cancel().catch(() => {});
        reader?.releaseLock();
      }
      if (!options.reconnect || signal.aborted) return;
      await new Promise<void>((resolve) => {
        const finish = () => {
          clearTimeout(timer);
          signal.removeEventListener("abort", finish);
          resolve();
        };
        const timer = setTimeout(finish, options.retryMs ?? 500);
        signal.addEventListener("abort", finish, { once: true });
      });
    }
  }
}
