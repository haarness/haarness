import { JsonSocket } from "./json-socket.js";
export { JsonSocket } from "./json-socket.js";
import { randomUUID } from "node:crypto";
import { open } from "node:fs/promises";
import { basename, extname } from "node:path";
import { Api } from "./api.js";
import { PtyConnection } from "./pty.js";
import { TransportError, type RequestOptions } from "./transport.js";
import type { BtwEvent, BtwRequest, StartTurnRequest, Turn, Item } from "./types.js";
export * from "./types.js";
export * from "./capabilities.js";
export * from "./transport.js";
export * from "./pty.js";
export * from "./server.js";
export interface ChatResult {
  turn: Turn;
  items: Item[];
  text: string;
}
export class Haarness extends Api {
  /** Server-owned management forms; raw domain operations use manageResource or the dedicated helpers. */
  managementView(resource: string, body: import("./types.js").JsonValue, options: RequestOptions = {}): Promise<import("./types.js").ManagementView> {
    return this.request("POST", `/v1/manage/${this.segment(resource)}`, { body, timeoutMs: 180_000, ...options });
  }

  /** Persistent availability and definition management on this runtime. Applies when idle. */
  capabilities(input: import("./capabilities.js").CapabilityRequest = { action: "list" }, options: RequestOptions = {}): Promise<import("./types.js").JsonValue> {
    return this.request("POST", "/v1/manage/capabilities", { body: input, timeoutMs: 180_000, ...options });
  }
  /** Catalog discovery, verified installation and version lifecycle on this runtime. */
  marketplace(input: import("./capabilities.js").MarketplaceRequest, options: RequestOptions = {}): Promise<import("./types.js").JsonValue> {
    return this.request("POST", "/v1/manage/marketplace", { body: input, timeoutMs: 180_000, ...options });
  }

  /** Bind every runtime endpoint to a workspace returned by hostOpen/hostProjects. */
  workspace(id: string): Haarness {
    const headers = () => this.headers();
    return new class extends Haarness {
      protected override headers() { return headers(); }
    }({baseUrl:this.url(`/v1/host/workspaces/${this.segment(id)}`), fetch:this.fetcher});
  }

  /** Start and wait for one turn; approvals must be handled by a concurrent observer. */
  async chat(
    id: string,
    text: string,
    body: Partial<Omit<StartTurnRequest, "text">> = {},
    options: RequestOptions = {},
  ): Promise<ChatResult> {
    const turn = await this.startTurn(
      id,
      { ...body, requestId: body.requestId ?? randomUUID(), text },
      options,
    );
    const result = await this.waitTurn(
      turn.id,
      { timeout_ms: Math.min(options.timeoutMs ?? 300_000, 300_000) },
      { timeoutMs: 305_000, ...options },
    );
    const items = (await this.listItems(id, {}, options)).filter(
      (item) => item.turnId === turn.id,
    );
    const output = items
      .filter((item) => item.data.type === "assistant_message")
      .map((item) =>
        item.data.type === "assistant_message" ? item.data.text : "",
      )
      .join("");
    return { turn: result, items, text: output };
  }
  /**
   * POST /v1/threads/{id}/btw/stream: a side question whose answer streams as
   * it is written. `delta` extends the answer, `text` replaces it, and the
   * stream ends with `done` or `error`. Aborting `options.signal` stops the
   * answer; a connection silent for `idleTimeoutMs` (default 45 s, three of
   * the server's keepalives) fails with a `TransportError`. The request is
   * never sent twice. Servers without `btw.stream.v1` lack the route.
   */
  async *btwStream(
    id: string,
    body: BtwRequest,
    options: Omit<RequestOptions, "timeoutMs"> & { idleTimeoutMs?: number } = {},
  ): AsyncGenerator<BtwEvent> {
    const quiet = new AbortController();
    const signal = AbortSignal.any([this.signal(options.signal), quiet.signal]);
    const limit = options.idleTimeoutMs ?? 45_000;
    let timer: ReturnType<typeof setTimeout> | undefined;
    let reader: ReadableStreamDefaultReader<Uint8Array> | undefined;
    const heard = () => {
      clearTimeout(timer);
      timer = setTimeout(() => {
        quiet.abort();
        void reader?.cancel().catch(() => {});
      }, limit);
    };
    try {
      heard();
      const response = await this.fetcher(this.url(`/v1/threads/${this.segment(id)}/btw/stream`), {
        method: "POST",
        headers: { ...await this.headers(), ...options.headers, Accept: "text/event-stream", "Content-Type": "application/json" },
        body: JSON.stringify(body),
        redirect: "error",
        signal,
      });
      if (!response.ok) return await this.failure(response);
      if (!response.body) throw new TransportError("Side question stream has no body");
      reader = response.body.getReader();
      const decoder = new TextDecoder();
      let buffer = "";
      let data: string[] = [];
      while (true) {
        const chunk = await reader.read();
        heard();
        buffer += decoder.decode(chunk.value, { stream: !chunk.done });
        if (chunk.done) buffer += "\n\n";
        if (buffer.length > 8 * 1024 * 1024) throw new TransportError("Event exceeds 8 MiB");
        let match: RegExpExecArray | null;
        while ((match = /\r\n|\r(?!$)|\n/.exec(buffer))) {
          const line = buffer.slice(0, match.index);
          buffer = buffer.slice(match.index + match[0].length);
          if (line.startsWith("data:")) data.push(line.slice(5).replace(/^ /, ""));
          if (line !== "" || !data.length) continue;
          const event = JSON.parse(data.join("\n")) as BtwEvent;
          data = [];
          yield event;
          if (event.type === "done" || event.type === "error") return;
        }
        if (chunk.done) throw new TransportError("The answer ended before it finished");
      }
    } catch (error) {
      if (quiet.signal.aborted && !options.signal?.aborted)
        throw new TransportError(`The side question sent nothing for ${limit} ms`, { cause: error });
      throw error;
    } finally {
      clearTimeout(timer);
      await reader?.cancel().catch(() => {});
    }
  }
  async connectBrowserHost(id: string, query: {version?: 1 | 2} = {version: 2}, options: RequestOptions = {}): Promise<JsonSocket> {
    return JsonSocket.connect(this.url(`/v1/threads/${this.segment(id)}/browser/host`, query).replace(/^http/, "ws"), {...await this.headers(), ...options.headers}, this.signal(options.signal), options.timeoutMs ?? this.timeoutMs);
  }
  /** Haarness for Chrome's bridge: one Chrome profile serving every chat on this server. Send `hello` first. */
  async connectBrowserExtensionHost(query: {version?: 1} = {version: 1}, options: RequestOptions = {}): Promise<JsonSocket> {
    return JsonSocket.connect(this.url("/v1/browser/extension/host", {version: 1, ...query}).replace(/^http/, "ws"), {...await this.headers(), ...options.headers}, this.signal(options.signal), options.timeoutMs ?? this.timeoutMs);
  }
  async connectDraftBrowserHost(id: string, options: RequestOptions = {}): Promise<JsonSocket> {
    return JsonSocket.connect(this.url(`/v1/browser/drafts/${this.segment(id)}/host`).replace(/^http/, "ws"), {...await this.headers(), ...options.headers}, this.signal(options.signal), options.timeoutMs ?? this.timeoutMs);
  }
  async connectPty(
    id: string,
    query: { cursor?: number } = {},
    options: RequestOptions = {},
  ): Promise<PtyConnection> {
    return PtyConnection.connect(
      this.url(`/v1/ptys/${this.segment(id)}/connect`, query).replace(
        /^http/,
        "ws",
      ),
      await this.headers(),
      this.signal(options.signal),
      options.timeoutMs ?? this.timeoutMs,
    );
  }
}
const mimes: Record<string, string> = {
  ".pdf": "application/pdf",
  ".png": "image/png",
  ".jpg": "image/jpeg",
  ".jpeg": "image/jpeg",
  ".webp": "image/webp",
  ".gif": "image/gif",
  // The server converts these to PNG or JPEG.
  ".heic": "image/heic",
  ".heif": "image/heif",
  ".bmp": "image/bmp",
  ".tif": "image/tiff",
  ".tiff": "image/tiff",
  ".mp3": "audio/mpeg",
  ".wav": "audio/wav",
  ".mp4": "video/mp4",
  ".webm": "video/webm",
  ".m4a": "audio/mp4",
  ".ogg": "audio/ogg",
  ".flac": "audio/flac",
  ".mov": "video/quicktime",
};
/** Read local media into a remote-safe data URL. Enforces the aggregate server limit. */
export async function attachments(paths: string[]): Promise<string[]> {
  const output: string[] = [];
  let size = 0;
  for (const path of paths) {
    const mime = mimes[extname(path).toLowerCase()];
    if (!mime) throw new TypeError("Unsupported attachment extension");
    const file = await open(path, "r");
    let bytes: Buffer;
    try {
      const buffer = Buffer.alloc(20 * 1024 * 1024 - size + 1);
      let offset = 0;
      while (offset < buffer.length) {
        const chunk = await file.read(
          buffer,
          offset,
          buffer.length - offset,
          null,
        );
        if (!chunk.bytesRead) break;
        offset += chunk.bytesRead;
      }
      bytes = buffer.subarray(0, offset);
    } finally {
      await file.close();
    }
    size += bytes.length;
    if (size > 20 * 1024 * 1024)
      throw new RangeError("Attachments exceed 20 MiB");
    output.push(
      `data:${mime};name=${encodeURIComponent(basename(path))};base64,${bytes.toString("base64")}`,
    );
  }
  return output;
}

export * from "./cloud.js";

export * from "./router.js";
