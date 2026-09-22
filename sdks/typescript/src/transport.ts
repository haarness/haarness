import type { JsonValue, EventEnvelope } from "./types.js";

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
}
type WireOptions = RequestOptions & {
  body?: unknown;
  query?: object;
  binaryBody?: boolean;
  binaryResponse?: boolean;
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
}

export class Transport {
  readonly baseUrl: string;
  private readonly authentication: Record<string, string>;
  protected readonly timeoutMs: number;
  private readonly fetcher: typeof globalThis.fetch;
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
  protected headers(): Record<string, string> {
    return { ...this.authentication };
  }
  protected signal(signal?: AbortSignal): AbortSignal {
    return signal
      ? AbortSignal.any([signal, this.lifetime.signal])
      : this.lifetime.signal;
  }
  private async failure(response: Response): Promise<never> {
    const text = await response.text();
    let body: unknown = text;
    try {
      body = JSON.parse(text);
    } catch {
      /* Framework errors can be plain text. */
    }
    const record =
      body && typeof body === "object" ? (body as Record<string, unknown>) : {};
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
    const headers = this.headers();
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
        headers["Content-Type"] = options.binaryBody
          ? "application/zip"
          : "application/json";
      const response = await this.fetcher(this.url(path, options.query), {
        method,
        headers,
        redirect: "error",
        signal,
        body:
          options.body === undefined
            ? undefined
            : options.binaryBody
              ? (options.body as BodyInit)
              : JSON.stringify(options.body),
      });
      if (!response.ok) return await this.failure(response);
      if (options.binaryResponse)
        return new Uint8Array(await response.arrayBuffer()) as T;
      const text = await response.text();
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
  /** Ordered events. Resync markers are yielded; hydrate REST snapshots before reconnecting. */
  async *events(options: EventOptions = {}): AsyncGenerator<ServerEvent> {
    const signal = this.signal(options.signal);
    let cursor = options.after;
    let instance: string | undefined;
    while (!signal.aborted) {
      const headers = {
        ...this.headers(),
        Accept: "text/event-stream",
        ...(cursor === undefined ? {} : { "Last-Event-ID": String(cursor) }),
      };
      let reader: ReadableStreamDefaultReader<Uint8Array> | undefined;
      try {
        const response = await this.fetcher(this.url("/v1/events"), {
          headers,
          redirect: "error",
          signal,
        });
        if (!response.ok) await this.failure(response);
        if (!response.body)
          throw new TransportError("Event stream has no body");
        reader = response.body!.getReader();
        const decoder = new TextDecoder();
        let buffer = "";
        let data: string[] = [];
        let size = 0;
        while (true) {
          const chunk = await reader.read();
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
              yield event;
            } else if (line.startsWith("data:")) {
              size += line.length;
              if (size > 8 * 1024 * 1024)
                throw new TransportError("Event exceeds 8 MiB");
              data.push(line.slice(5).replace(/^ /, ""));
            }
          }
          if (chunk.done) break;
        }
      } catch (error) {
        if (signal.aborted) return;
        if (
          !options.reconnect ||
          error instanceof ApiError ||
          error instanceof SyntaxError ||
          error instanceof TransportError
        )
          throw error;
      } finally {
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
