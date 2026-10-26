import { HaarnessRouter } from "./router.js";
import { JsonSocket } from "./json-socket.js";
import { CloudApi } from "./cloud-api.js";
import { Haarness } from "./index.js";
import { type RequestOptions } from "./transport.js";
import type {
  AgentSessionCreatedResponse,
  AttachResponse,
  CreateAgentSessionRequest,
} from "./cloud-types.js";
import type {
  CreateThreadRequest,
  Thread,
  ThreadQuery,
  StartTurnRequest,
} from "./types.js";
export type * as CloudTypes from "./cloud-types.js";
export type { CloudOptions, CloudEventOptions } from "./cloud-transport.js";

/** Cloud account API. Runtime access is scoped to an explicit session or remote host. */
export class HaarnessCloud extends CloudApi {
  /** Explicit inference key; account OAuth/session credentials are never forwarded. */
  async router(id: string, apiKey: string): Promise<HaarnessRouter> {
    const router = await this.getRouter(id);
    return new HaarnessRouter({baseUrl:validateGateway(router.routerUrl), token:apiKey, fetch:this.fetcher, timeoutMs:this.timeoutMs});
  }
  /** Host-side relay protocol. Authenticate with the dedicated connector token. */
  async connectRemoteHost(id: string, options: RequestOptions = {}): Promise<JsonSocket> {
    return JsonSocket.connect(this.url(`/api/v1/remote-hosts/${this.segment(id)}/connect`).replace(/^http/, "ws"), {...await this.headers(), ...options.headers}, this.signal(options.signal), options.timeoutMs ?? this.timeoutMs);
  }
  /** Full host gateway: discover projects, then use .workspace(projectId). */
  remoteHost(id: string): Haarness {
    return this.remoteRuntime(`/api/v1/remote-hosts/${this.segment(id)}/runtime`);
  }
  /** One published conversation tree, including its shared workspace files. */
  remoteSession(id: string): Haarness {
    return this.remoteRuntime(`/api/v1/remote-sessions/${this.segment(id)}/runtime`);
  }
  private remoteRuntime(path: string): Haarness {
    const headers = () => { this.signal().throwIfAborted(); return this.headers(); };
    return new class extends Haarness {
      protected override headers() { return headers(); }
    }({baseUrl: this.baseUrl + path, token: "", fetch: this.fetcher, timeoutMs: this.timeoutMs});
  }
  /** The fetch function and timeout this client was made with, which its session runtimes use too. */
  runtimeOptions(): { fetch: typeof globalThis.fetch; timeoutMs: number } {
    return { fetch: this.fetcher, timeoutMs: this.timeoutMs };
  }
  session(sessionId: string): CloudSession {
    this.segment(sessionId);
    return new CloudSession(this, sessionId);
  }
  async createSession(
    body: CreateAgentSessionRequest,
    options?: RequestOptions,
  ): Promise<CloudSession> {
    const result = await this.createAgentSession(body, options);
    const session = this.session(result.session.id);
    session.created = result;
    return session;
  }
}

export function validateGateway(value: string): string {
  const url = new URL(value);
  if (
    (url.protocol !== "https:" &&
      !(
        url.protocol === "http:" &&
        ["localhost", "127.0.0.1", "[::1]"].includes(url.hostname)
      )) ||
    url.username ||
    url.password ||
    url.search ||
    url.hash
  )
    throw new TypeError(
      "Cloud gateway must use HTTPS or loopback HTTP without credentials, query or fragment",
    );
  return url.href.replace(/\/+$/, "");
}

/** One machine/filesystem, with many independent chat histories. close() only detaches. */
export class CloudSession {
  readonly chats: CloudChats;
  /** What `createSession` answered, with the first message's launch (`created.session.launch?.chatId`). */
  created?: AgentSessionCreatedResponse;
  private runtime?: Promise<Haarness>;
  private ticket?: AttachResponse;
  private renewing?: Promise<AttachResponse>;
  constructor(
    readonly cloud: HaarnessCloud,
    readonly id: string,
  ) {
    this.chats = new CloudChats(this);
  }
  get(options?: RequestOptions) {
    return this.cloud.getAgentSession(this.id, options);
  }
  start(options?: RequestOptions) {
    return this.cloud.startAgentSession(this.id, options);
  }
  stop(options?: RequestOptions) {
    return this.cloud.stopAgentSession(this.id, options);
  }
  delete(options?: RequestOptions) {
    return this.cloud.deleteAgentSession(this.id, options);
  }
  private async credentials(): Promise<AttachResponse> {
    if (this.ticket && Date.parse(this.ticket.expiresAt) > Date.now() + 30_000)
      return this.ticket;
    this.renewing ??= this.cloud
      .attachAgentSession(this.id)
      .then((ticket) => {
        validateGateway(ticket.gatewayUrl);
        this.ticket = ticket;
        return ticket;
      })
      .finally(() => {
        this.renewing = undefined;
      });
    return this.renewing;
  }
  /** All native runtime endpoints, files, PTYs and SSE; this never starts compute. */
  async connect(): Promise<Haarness> {
    this.runtime ??= this.credentials()
      .then((ticket) => {
        const { fetch: fetcher0, timeoutMs } = this.cloud.runtimeOptions();
        const initial = validateGateway(ticket.gatewayUrl);
        const fetcher: typeof fetch = async (input, init) => {
          const original = new URL(
            typeof input === "string"
              ? input
              : input instanceof URL
                ? input
                : input.url,
          );
          const suffix = original.href.slice(initial.length);
          for (let attempt = 0; ; attempt++) {
            const current = await this.credentials();
            const headers = new Headers(init?.headers);
            headers.set("Authorization", `Bearer ${current.ticket}`);
            const response = await fetcher0(
              validateGateway(current.gatewayUrl) + suffix,
              { ...init, headers },
            );
            const stale =
              response.status === 400 &&
              (init?.method ?? "GET") === "GET" &&
              (await response
                .clone()
                .json()
                .then((body) => {
                  const error = body.error ?? body;
                  return (
                    error.code === "remote_project" &&
                    String(error.message).includes("workspace restarted")
                  );
                })
                .catch(() => false));
            if (attempt === 0 && (response.status === 401 || stale)) {
              await response.body?.cancel();
              this.ticket = undefined;
              continue;
            }
            return response;
          }
        };
        return new Haarness({
          baseUrl: initial,
          fetch: fetcher,
          timeoutMs,
          resolveAuth: async () => ({
            type: "bearer",
            token: (await this.credentials()).ticket,
          }),
        });
      })
      .catch((error) => {
        this.runtime = undefined;
        throw error;
      });
    return this.runtime;
  }
  async close(): Promise<void> {
    if (this.runtime) (await this.runtime).close();
    this.runtime = undefined;
    this.ticket = undefined;
  }
}

export class CloudChats {
  constructor(readonly session: CloudSession) {}
  async list(
    query: ThreadQuery = {},
    options?: RequestOptions,
  ): Promise<Thread[]> {
    return (
      await (await this.session.connect()).listThreads(query, options)
    ).filter((thread) => !thread.parentId);
  }
  async create(
    body: CreateThreadRequest = {},
    options?: RequestOptions,
  ): Promise<CloudChat> {
    const runtime = await this.session.connect();
    const info = await runtime.info(options);
    const thread = await runtime.createThread(
      {
        agent: info.defaultAgent,
        provider: info.defaultProvider,
        model: info.defaultModel,
        ...body,
      },
      options,
    );
    return this.get(thread.id);
  }
  get(chatId: string): CloudChat {
    if (!chatId || chatId === "." || chatId === "..")
      throw new TypeError("chatId is required");
    return new CloudChat(this.session, chatId);
  }
}

/** The chat ID is the native thread ID; a turn ID never substitutes for it. */
export class CloudChat {
  constructor(
    readonly session: CloudSession,
    readonly id: string,
  ) {}
  get sessionId(): string {
    return this.session.id;
  }
  async get(options?: RequestOptions) {
    return (await this.session.connect()).getThread(this.id, options);
  }
  async rename(title: string, options?: RequestOptions) {
    return (await this.session.connect()).updateThread(
      this.id,
      { title },
      options,
    );
  }
  async archive(archived = true, options?: RequestOptions) {
    return (await this.session.connect()).updateThread(
      this.id,
      { archived },
      options,
    );
  }
  async messages(after?: number, options?: RequestOptions) {
    return (await this.session.connect()).listItems(
      this.id,
      { after },
      options,
    );
  }
  async ask(
    text: string,
    body: Partial<Omit<StartTurnRequest, "text">> = {},
    options?: RequestOptions,
  ) {
    return (await this.session.connect()).chat(this.id, text, body, options);
  }
  async send(body: StartTurnRequest, options?: RequestOptions) {
    return (await this.session.connect()).startTurn(this.id, body, options);
  }
  private async turn(turnId: string, options?: RequestOptions) {
    const runtime = await this.session.connect();
    if ((await runtime.getTurn(turnId, options)).threadId !== this.id)
      throw new TypeError("Turn belongs to another chat");
    return runtime;
  }
  async wait(turnId: string, timeoutMs = 120_000, options?: RequestOptions) {
    return (await this.turn(turnId, options)).waitTurn(
      turnId,
      { timeout_ms: timeoutMs },
      { timeoutMs: timeoutMs + 5_000, ...options },
    );
  }
  async abort(turnId?: string, options?: RequestOptions) {
    if (turnId)
      return (await this.turn(turnId, options)).interruptTurn(turnId, {}, options);
    return (await this.session.connect()).interruptThreadTree(this.id, {}, options);
  }
}
